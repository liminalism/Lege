#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use pdf_content::{PageCompiler, SemanticPage, semantic::SemanticOp};
use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_page_ir::Matrix;
use pdf_source::OwnedBytesSource;
use pdf_test_support::builder::PdfBuilder;

fn compile(content: &str, nested: &str) -> SemanticPage {
    let mut b = PdfBuilder::new();
    b.add_object(
        1,
        "<</Type/Catalog/Pages 2 0 R/OCProperties<</OCGs[12 0 R]/D<</OFF[12 0 R]>>>>>>",
    );
    b.add_object(
        2,
        "<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 200 200]>>",
    );
    b.add_object(3, "<</Type/Page/Parent 2 0 R/Contents 4 0 R/Resources<</Font<</F1 5 0 R/F3 8 0 R>>/XObject<</Fm 6 0 R>>/ExtGState<</SM 7 0 R>>/Properties<</Off 12 0 R>>>>>>");
    b.add_stream(4, "", content.as_bytes());
    b.add_object(5, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>");
    b.add_stream(
        6,
        "/Type/XObject/Subtype/Form/BBox[0 0 100 100]/Group<</S/Transparency/I true>>/Resources<</Font<</F1 5 0 R>>>>",
        nested.as_bytes(),
    );
    b.add_object(7, "<</SMask<</S/Luminosity/G 6 0 R>>>>");
    b.add_object(8, "<</Type/Font/Subtype/Type3/FontBBox[0 0 1000 1000]/FontMatrix[0.001 0 0 0.001 0 0]/FirstChar 65/LastChar 65/Widths[1000]/CharProcs<</A 6 0 R>>/Encoding<</Type/Encoding/Differences[65/A]>>/Resources<</Font<</F1 5 0 R>>>>>>");
    b.add_object(12, "<</Type/OCG/Name(Hidden)>>");
    b.finish_classic_xref("/Root 1 0 R");
    let snapshot = DocumentSnapshot::open(
        Arc::new(OwnedBytesSource::new(b.into_bytes())),
        DocumentLimits::default(),
    )
    .unwrap();
    PageCompiler::new()
        .compile_semantic(&snapshot, PageIndex(0), &mut ParseContext::new())
        .unwrap()
}

const INVOCATIONS: [&str; 3] = ["/Fm Do", "/SM gs", "BT /F3 10 Tf (A) Tj ET"];

#[test]
fn malformed_nested_saves_cannot_consume_or_leak_caller_graphics_state() {
    for invoke in INVOCATIONS {
        let page = compile(
            &format!("q 1 0 0 1 10 20 cm {invoke} 0 0 2 2 re f Q 5 5 1 1 re f"),
            "Q 1 0 0 1 30 40 cm 0 0 1 1 re f q 2 0 0 2 0 0 cm",
        );
        let mut ctm = Matrix::IDENTITY;
        let mut saves = Vec::new();
        let mut fills = Vec::new();
        for op in page.ops.iter() {
            match op {
                SemanticOp::Save => saves.push(ctm),
                SemanticOp::Restore => ctm = saves.pop().expect(invoke),
                SemanticOp::Concat(m) => ctm = m.then(ctm),
                SemanticOp::Fill { .. } => fills.push(ctm),
                _ => {}
            }
        }
        assert!(saves.is_empty(), "{invoke}: every nested save must close");
        assert_eq!(
            fills[fills.len() - 2],
            Matrix::translate(10.0, 20.0),
            "{invoke}"
        );
        assert_eq!(fills[fills.len() - 1], Matrix::IDENTITY, "{invoke}");
    }
}

#[test]
fn nested_marked_content_closers_cannot_pop_a_callers_hidden_span() {
    // A soft-mask is resolved even in a hidden span. Its stray EMC used to
    // consume the caller frame, leaving the cached hidden count inconsistent.
    let page = compile("/OC /Off BDC /SM gs EMC 0 0 2 2 re f", "EMC /P BMC");
    assert_eq!(
        page.ops
            .iter()
            .filter(|op| matches!(op, SemanticOp::Fill { .. }))
            .count(),
        1
    );
}

#[test]
fn nested_paths_and_pending_clips_are_isolated_in_every_stream_kind() {
    for invoke in INVOCATIONS {
        let page = compile(
            &format!("0 0 20 20 re W {invoke} n"),
            "0 0 1 1 re f 3 3 2 2 re W",
        );
        let fills: Vec<_> = page
            .ops
            .iter()
            .filter_map(|op| match op {
                SemanticOp::Fill { path, .. } => Some(*path),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 1, "{invoke}");
        assert_eq!(
            page.paths[fills[0].index()].points.len(),
            4,
            "caller path must not join nested fill: {invoke}"
        );
        let clips: Vec<_> = page
            .ops
            .iter()
            .filter_map(|op| match op {
                SemanticOp::Clip { path, .. } => Some(*path),
                _ => None,
            })
            .collect();
        let caller_clip = &page.paths[clips.last().unwrap().index()];
        assert_eq!(caller_clip.points[1].x, 20.0, "{invoke}");
    }
}

#[test]
fn nested_text_does_not_flush_or_move_the_callers_text_object() {
    for invoke in ["/Fm Do", "/SM gs"] {
        let page = compile(
            &format!("BT /F1 12 Tf 7 Tr 1 0 0 1 10 20 Tm (A) Tj {invoke} (B) Tj ET"),
            "BT /F1 12 Tf 0 Tr 1 0 0 1 90 90 Tm (X) Tj ET",
        );
        let outer: Vec<_> = page
            .text_runs
            .iter()
            .filter(|run| run.render_mode == 7)
            .collect();
        assert_eq!(outer.len(), 2);
        assert_eq!(outer[0].text_matrix.f, 20.0);
        assert_eq!(outer[1].text_matrix.f, 20.0, "{invoke}");
        assert!(outer[1].text_matrix.e > 10.0 && outer[1].text_matrix.e < 30.0);
        let clips: Vec<_> = page
            .ops
            .iter()
            .filter_map(|op| match op {
                SemanticOp::ClipText { runs } => Some(runs),
                _ => None,
            })
            .collect();
        assert_eq!(clips.len(), 1, "{invoke}");
        assert_eq!(
            clips[0].len(),
            2,
            "nested BT must not discard caller clips: {invoke}"
        );
    }
}

#[test]
fn repeated_bt_commits_previous_clip_and_text_after_et_starts_fresh() {
    let page = compile(
        "BT /F1 12 Tf 7 Tr 1 0 0 1 40 50 Tm (A) Tj BT (B) Tj ET ET 0 Tr (C) Tj",
        "",
    );
    assert_eq!(
        page.ops
            .iter()
            .filter(|op| matches!(op, SemanticOp::ClipText { .. }))
            .count(),
        2
    );
    assert_eq!(page.text_runs[2].text_matrix, Matrix::IDENTITY);
}

#[test]
fn charproc_text_cannot_discard_the_callers_accumulated_text_clip() {
    let page = compile(
        "BT /F1 12 Tf 7 Tr (A) Tj 0 Tr /F3 10 Tf (A) Tj /F1 12 Tf 7 Tr (B) Tj ET",
        "BT /F1 10 Tf 0 Tr (X) Tj ET",
    );
    let clips: Vec<_> = page
        .ops
        .iter()
        .filter_map(|op| match op {
            SemanticOp::ClipText { runs } => Some(runs),
            _ => None,
        })
        .collect();
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].len(), 2);
}
