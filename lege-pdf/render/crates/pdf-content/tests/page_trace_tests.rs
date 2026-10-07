#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! Audit sidecar tests: [`PageTrace`](pdf_content::PageTrace) entries stay
//! index-aligned with `SemanticPage::ops` and name each op's operator, source
//! stream object, nested-object stack, byte span, and paint origin.

use std::sync::Arc;

use pdf_content::semantic::SemanticOp;
use pdf_content::{PageCompiler, PageTrace, SemanticPage};
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_object::ObjectId;
use pdf_page_ir::PaintOrigin;
use pdf_source::{OwnedBytesSource, PdfSource};
use pdf_test_support::builder::PdfBuilder;

fn open(bytes: Vec<u8>) -> DocumentSnapshot {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes));
    DocumentSnapshot::open(source, DocumentLimits::default()).expect("open failed")
}

fn compile(snapshot: &DocumentSnapshot) -> SemanticPage {
    let mut ctx = ParseContext::new();
    PageCompiler::new()
        .compile_semantic(snapshot, PageIndex(0), &mut ctx)
        .expect("compile failed")
}

fn compile_traced(snapshot: &DocumentSnapshot) -> (SemanticPage, PageTrace) {
    let mut ctx = ParseContext::new();
    PageCompiler::new()
        .compile_semantic_traced(snapshot, PageIndex(0), &mut ctx)
        .expect("traced compile failed")
}

fn single_page(content: &[u8]) -> DocumentSnapshot {
    let mut b = PdfBuilder::new();
    b.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    b.add_object(
        2,
        "<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 200 200]>>",
    );
    b.add_object(
        3,
        "<</Type/Page/Parent 2 0 R/Contents 4 0 R/Resources<<>>>>",
    );
    b.add_stream(4, "", content);
    b.finish_classic_xref("/Root 1 0 R");
    open(b.into_bytes())
}

#[test]
fn traced_page_matches_plain_compilation_with_aligned_entries() {
    let snap = single_page(b"q 1 0 0 rg 0 0 200 200 re f Q");
    let plain = compile(&snap);
    let (traced, trace) = compile_traced(&snap);
    // The audit pass must not change the semantic page.
    assert_eq!(traced, plain);
    assert_eq!(trace.len(), traced.ops.len());
    for (index, entry) in trace.entries.iter().enumerate() {
        assert_eq!(entry.op_index, index);
    }
}

#[test]
fn page_level_op_names_stream_span_operator_and_index() {
    let content = b"0 0 200 200 re f";
    let snap = single_page(content);
    let (page, trace) = compile_traced(&snap);
    assert!(matches!(page.ops[0], SemanticOp::Fill { .. }));
    assert_eq!(trace.len(), 1);
    let entry = &trace.entries[0];
    assert_eq!(entry.operator, b"f");
    assert!(!entry.synthesized);
    assert_eq!(entry.stream, Some(ObjectId::new(4, 0)));
    assert_eq!(&content[entry.span.0..entry.span.1], b"f");
    assert!(entry.stack.is_empty());
    assert_eq!(entry.origin, PaintOrigin::PageContent);
    // `re` is operator 1, `f` is operator 2 — path construction consumes an
    // index even though it emits no op.
    assert_eq!(entry.content_op_index, 2);
}

#[test]
fn form_invocation_nests_stream_stack_and_origin() {
    let form_content = b"1 0 0 rg 0 0 50 50 re f";
    let mut b = PdfBuilder::new();
    b.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    b.add_object(
        2,
        "<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 200 200]>>",
    );
    b.add_object(
        3,
        "<</Type/Page/Parent 2 0 R/Contents 4 0 R/Resources<</XObject<</Fm 5 0 R>>>>>>",
    );
    b.add_stream(4, "", b"q /Fm Do Q");
    b.add_stream(
        5,
        "/Type/XObject/Subtype/Form/BBox[0 0 50 50]/Matrix[1 0 0 1 10 20]",
        form_content,
    );
    b.finish_classic_xref("/Root 1 0 R");
    let (page, trace) = compile_traced(&open(b.into_bytes()));
    assert_eq!(page.ops.len(), 10);
    assert_eq!(trace.len(), 10);

    // The form-invocation wrappers attribute to the `Do` in the page stream.
    assert!(matches!(trace.entries[1].operator.as_slice(), b"Do"));
    assert_eq!(trace.entries[1].stream, Some(ObjectId::new(4, 0)));
    assert!(trace.entries[1].stack.is_empty());

    // Content inside the form names the form stream, stack, and origin.
    let fill = &trace.entries[6];
    assert!(matches!(page.ops[6], SemanticOp::Fill { .. }));
    assert_eq!(fill.operator, b"f");
    assert_eq!(fill.stream, Some(ObjectId::new(5, 0)));
    assert_eq!(fill.stack.len(), 1);
    assert_eq!(fill.stack[0].kind, pdf_content::TraceFrameKind::FormXObject);
    assert_eq!(fill.stack[0].object, Some(ObjectId::new(5, 0)));
    assert_eq!(fill.origin, PaintOrigin::FormXObject);
    assert_eq!(&form_content[fill.span.0..fill.span.1], b"f");
    // q(1) Do(2) rg(3) re(4) f(5): one global execution sequence.
    assert_eq!(fill.content_op_index, 5);

    // The origin markers are synthesized: no operator and no stream, but the
    // stack still names the nested form.
    assert!(trace.entries[4].synthesized);
    assert_eq!(trace.entries[4].stream, None);
    assert_eq!(trace.entries[4].stack.len(), 1);
    assert_eq!(
        trace.entries[4].stack[0].object,
        Some(ObjectId::new(5, 0))
    );

    // After the form, provenance returns to the page stream.
    assert_eq!(trace.entries[9].operator, b"Q");
    assert_eq!(trace.entries[9].stream, Some(ObjectId::new(4, 0)));
    assert!(trace.entries[9].stack.is_empty());
}

#[test]
fn contents_array_resolves_each_op_to_its_own_stream() {
    let first = b"1 0 0 rg";
    let second = b"0 0 10 10 re f";
    let mut b = PdfBuilder::new();
    b.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    b.add_object(
        2,
        "<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 200 200]>>",
    );
    b.add_object(
        3,
        "<</Type/Page/Parent 2 0 R/Contents[4 0 R 5 0 R]/Resources<<>>>>",
    );
    b.add_stream(4, "", first);
    b.add_stream(5, "", second);
    b.finish_classic_xref("/Root 1 0 R");
    let (page, trace) = compile_traced(&open(b.into_bytes()));
    assert_eq!(page.ops.len(), 2);

    assert!(matches!(page.ops[0], SemanticOp::SetFillColor(_)));
    assert_eq!(trace.entries[0].stream, Some(ObjectId::new(4, 0)));
    let span = trace.entries[0].span;
    assert_eq!(&first[span.0..span.1], b"rg");

    assert!(matches!(page.ops[1], SemanticOp::Fill { .. }));
    assert_eq!(trace.entries[1].stream, Some(ObjectId::new(5, 0)));
    // Stream-relative: the span indexes the second stream's own bytes, not
    // the concatenated content.
    let span = trace.entries[1].span;
    assert_eq!(&second[span.0..span.1], b"f");
}

#[test]
fn inline_image_spans_bi_to_ei() {
    let content = b"BI /W 1 /H 1 /BPC 1 /IM true ID \x80 EI";
    let snap = single_page(content);
    let (page, trace) = compile_traced(&snap);
    assert_eq!(page.ops.len(), 1);
    assert!(matches!(page.ops[0], SemanticOp::DrawImage(_)));
    let entry = &trace.entries[0];
    assert_eq!(entry.operator, b"BI");
    assert!(!entry.synthesized);
    assert_eq!(entry.stream, Some(ObjectId::new(4, 0)));
    let framed = &content[entry.span.0..entry.span.1];
    assert!(framed.starts_with(b"BI"));
    assert!(framed.ends_with(b"EI"));
}

#[test]
fn type3_glyph_frames_name_charproc_stream() {
    let mut b = PdfBuilder::new();
    b.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    b.add_object(
        2,
        "<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 200 200]>>",
    );
    b.add_object(
        3,
        "<</Type/Page/Parent 2 0 R/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>",
    );
    b.add_stream(4, "", b"BT /F1 100 Tf 10 20 Td (A) Tj ET");
    b.add_object(
        5,
        "<</Type/Font/Subtype/Type3/FontBBox[0 0 1000 1000]\
         /FontMatrix[0.001 0 0 0.001 0 0]/CharProcs 6 0 R/Encoding 7 0 R\
         /FirstChar 65/LastChar 65/Widths[1000]/Resources<<>>>>",
    );
    b.add_object(6, "<</square 8 0 R>>");
    b.add_object(7, "<</Type/Encoding/Differences[65/square]>>");
    b.add_stream(8, "", b"1000 0 d0 0 0 100 100 re f");
    b.finish_classic_xref("/Root 1 0 R");
    let (page, trace) = compile_traced(&open(b.into_bytes()));

    let fill = page
        .ops
        .iter()
        .position(|op| matches!(op, SemanticOp::Fill { .. }))
        .expect("charproc fill");
    let entry = &trace.entries[fill];
    assert_eq!(entry.operator, b"f");
    assert_eq!(entry.stream, Some(ObjectId::new(8, 0)));
    assert_eq!(entry.stack.len(), 1);
    assert_eq!(entry.stack[0].kind, pdf_content::TraceFrameKind::Type3Glyph);
    assert_eq!(entry.stack[0].object, Some(ObjectId::new(8, 0)));
    assert_eq!(entry.origin, PaintOrigin::Type3Glyph);

    // The extraction run stays page-level: shown by `Tj` in stream 4.
    let show = page
        .ops
        .iter()
        .position(|op| matches!(op, SemanticOp::ShowText(_)))
        .expect("extraction run");
    assert_eq!(trace.entries[show].operator, b"Tj");
    assert_eq!(trace.entries[show].stream, Some(ObjectId::new(4, 0)));
    assert!(trace.entries[show].stack.is_empty());
}

#[test]
fn synthesized_markup_appearance_carries_annotation_stack() {
    let mut b = PdfBuilder::new();
    b.add_object(1, "<</Type/Catalog/Pages 2 0 R>>");
    b.add_object(
        2,
        "<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 300 300]>>",
    );
    b.add_object(
        3,
        "<</Type/Page/Parent 2 0 R/Contents 4 0 R/Annots[10 0 R]>>",
    );
    b.add_stream(4, "", b"");
    b.add_object(
        10,
        "<</Type/Annot/Subtype/Highlight/Rect[10 200 110 220]\
         /QuadPoints[10 220 110 220 10 200 110 200]>>",
    );
    b.finish_classic_xref("/Root 1 0 R");
    let snap = open(b.into_bytes());
    let mut ctx = ParseContext::new();
    let (page, trace) = PageCompiler::new()
        .with_annotations(true)
        .compile_semantic_traced(&snap, PageIndex(0), &mut ctx)
        .expect("traced compile failed");

    let fill = page
        .ops
        .iter()
        .position(|op| matches!(op, SemanticOp::Fill { .. }))
        .expect("synthesized fill");
    let entry = &trace.entries[fill];
    assert!(entry.synthesized);
    assert!(entry.operator.is_empty());
    assert_eq!(entry.stream, None);
    assert_eq!(entry.stack.len(), 1);
    assert_eq!(
        entry.stack[0].kind,
        pdf_content::TraceFrameKind::AnnotationAppearance
    );
    assert_eq!(entry.stack[0].object, Some(ObjectId::new(10, 0)));
    assert_eq!(entry.origin, PaintOrigin::AnnotationAppearance);
}
