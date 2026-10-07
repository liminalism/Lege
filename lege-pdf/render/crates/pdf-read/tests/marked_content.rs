#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! The doctor inventories marked-content spans per page: tag names,
//! content-stream offsets, and enclosed-run bounds.

use std::sync::Arc;

use pdf_document::DocumentLimits;
use pdf_read::{OpenOutcome, examine};
use pdf_source::{OwnedBytesSource, PdfSource};
use pdf_test_support::builder;

fn examine_bytes(bytes: Vec<u8>) -> pdf_read::DocumentReport {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes));
    examine(source, DocumentLimits::default())
}

#[test]
fn doctor_reports_marked_content_per_page() {
    let report = examine_bytes(builder::touchup_edit());
    assert!(
        matches!(report.open, OpenOutcome::Ok),
        "open: {:?}",
        report.open
    );
    assert_eq!(report.pages.len(), 2);

    let page0 = &report.pages[0].marked_content;
    assert_eq!(page0.len(), 1, "{page0:?}");
    assert_eq!(page0[0].tag, "TouchUp_TextEdit");
    assert!(
        page0[0]
            .close_offset
            .is_some_and(|close| close > page0[0].open_offset),
        "{page0:?}"
    );
    let bbox = page0[0].bbox.expect("enclosed run");
    assert!((bbox[0] - 72.0).abs() < 0.01, "{bbox:?}");
    assert!((bbox[1] - 700.0).abs() < 0.01, "{bbox:?}");

    // Neutral tags travel too; deciding what counts as an edit marker is
    // the consumer's job.
    let page1 = &report.pages[1].marked_content;
    assert_eq!(page1.len(), 1, "{page1:?}");
    assert_eq!(page1[0].tag, "Span");
}

#[test]
fn document_without_marked_content_reports_empty() {
    let report = examine_bytes(builder::multipage_classic(2));
    assert!(
        report.pages.iter().all(|p| p.marked_content.is_empty()),
        "{:?}",
        report.pages
    );
}
