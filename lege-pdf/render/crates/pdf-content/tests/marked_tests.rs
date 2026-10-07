#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! Marked-content inventory tests: tag names, content-stream offsets, and
//! enclosed-run bounds from [`PageCompiler::marked_content_spans`].

use std::sync::Arc;

use pdf_content::PageCompiler;
use pdf_content::semantic::SemanticOp;
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_object::PdfObject;
use pdf_source::{OwnedBytesSource, PdfSource};
use pdf_test_support::builder::PdfBuilder;

fn open(bytes: Vec<u8>) -> DocumentSnapshot {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes));
    DocumentSnapshot::open(source, DocumentLimits::default()).expect("open failed")
}

/// A one-page document with the given content stream and an `/F1` Helvetica.
fn single_page(content: &[u8]) -> DocumentSnapshot {
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
    b.add_stream(4, "", content);
    b.add_object(5, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    b.finish_classic_xref("/Root 1 0 R");
    open(b.into_bytes())
}

fn spans(snapshot: &DocumentSnapshot, page: u32) -> Vec<pdf_content::MarkedContentSpan> {
    let mut ctx = ParseContext::new();
    PageCompiler::new()
        .marked_content_spans(snapshot, PageIndex(page), &mut ctx)
        .expect("marker pass failed")
}

/// The decoded bytes of a page's single content stream (every fixture page
/// here carries exactly one).
fn decoded_content(snapshot: &DocumentSnapshot, page: u32) -> Vec<u8> {
    let mut ctx = ParseContext::new();
    let page_ref = snapshot.page(PageIndex(page)).expect("page");
    let contents = page_ref.contents.clone().expect("contents");
    let resolved = match &contents {
        PdfObject::Reference(id) => snapshot
            .objects()
            .resolve(snapshot, *id, &mut ctx)
            .expect("resolve content"),
        direct => Arc::new(direct.clone()),
    };
    match resolved.as_ref() {
        PdfObject::Stream(stream) => snapshot
            .decode_stream_data(stream, &mut ctx)
            .expect("decode content"),
        other => panic!("content is not a stream: {other:?}"),
    }
}

#[test]
fn touchup_span_reports_tag_offsets_and_bbox() {
    let snapshot = open(pdf_test_support::builder::touchup_edit());
    let found = spans(&snapshot, 0);
    assert_eq!(found.len(), 1, "{found:?}");
    let span = &found[0];
    assert_eq!(span.tag, "TouchUp_TextEdit");
    let close = span.close_offset.expect("closed span");
    assert!(span.open_offset < close, "{span:?}");

    // Offsets land on the operator keywords in the decoded stream.
    let content = decoded_content(&snapshot, 0);
    assert_eq!(&content[span.open_offset..span.open_offset + 3], b"BMC");
    assert_eq!(&content[close..close + 3], b"EMC");

    // The enclosed run sits at (72,700) in 12pt: the box starts there and
    // is one em tall. The first, untagged run at y=720 must not leak in.
    let bbox = span.bbox.expect("enclosed run");
    assert!((bbox.x0 - 72.0).abs() < 0.01, "{bbox:?}");
    assert!((bbox.y0 - 700.0).abs() < 0.01, "{bbox:?}");
    assert!(bbox.x1 > bbox.x0 + 10.0, "{bbox:?}");
    assert!((bbox.y1 - 712.0).abs() < 0.01, "{bbox:?}");
}

#[test]
fn neutral_tags_are_inventoried_too() {
    let snapshot = open(pdf_test_support::builder::touchup_edit());
    let found = spans(&snapshot, 1);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].tag, "Span");
    assert!(found[0].close_offset.is_some());
    assert!(found[0].bbox.is_some());
}

#[test]
fn document_without_marked_content_has_no_spans() {
    let snapshot = open(pdf_test_support::builder::multipage_classic(2));
    assert!(spans(&snapshot, 0).is_empty());
    assert!(spans(&snapshot, 1).is_empty());
}

#[test]
fn bdc_spans_report_their_tag() {
    let snapshot = single_page(b"/TouchUp_TextEdit /P0 BDC BT /F1 12 Tf 10 10 Td (x) Tj ET EMC");
    let found = spans(&snapshot, 0);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].tag, "TouchUp_TextEdit");
    assert!(found[0].close_offset.is_some());
    assert!(found[0].bbox.is_some());
}

#[test]
fn unclosed_span_is_reported_without_close_offset() {
    let snapshot = single_page(b"/TouchUp_TextEdit BMC BT /F1 12 Tf 10 10 Td (x) Tj ET");
    let found = spans(&snapshot, 0);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].tag, "TouchUp_TextEdit");
    assert_eq!(found[0].close_offset, None);
    assert!(found[0].bbox.is_some());
}

#[test]
fn empty_span_has_no_bbox() {
    let snapshot = single_page(b"/TouchUp_TextEdit BMC EMC");
    let found = spans(&snapshot, 0);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].close_offset.is_some());
    assert!(found[0].bbox.is_none());
}

#[test]
fn default_compilation_is_unaffected_by_the_inventory() {
    // Both runs still compile exactly as before; the accessor is a
    // separate pass that default compilation never observes.
    let snapshot = open(pdf_test_support::builder::touchup_edit());
    let mut ctx = ParseContext::new();
    let page = PageCompiler::new()
        .compile_semantic(&snapshot, PageIndex(0), &mut ctx)
        .expect("compile failed");
    assert_eq!(page.text_runs.len(), 2);
    assert_eq!(
        page.ops
            .iter()
            .filter(|op| matches!(op, SemanticOp::ShowText(_)))
            .count(),
        2
    );
}
