#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism

//! Xref-section inventory (`XrefHealth::sections`) over classic,
//! incremental-update, and hybrid fixtures.

use std::sync::Arc;

use pdf_document::DocumentLimits;
use pdf_read::{OpenOutcome, XrefSectionKind, examine};
use pdf_source::{OwnedBytesSource, PdfSource};
use pdf_test_support::builder;

fn examine_bytes(bytes: Vec<u8>) -> pdf_read::DocumentReport {
    let source: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes));
    examine(source, DocumentLimits::default())
}

#[test]
fn classic_inventory_carries_counts_and_no_linkage() {
    let report = examine_bytes(builder::multipage_classic(2));
    assert!(
        matches!(report.open, OpenOutcome::Ok),
        "open: {:?}",
        report.open
    );
    assert_eq!(report.xref.revision_count, 1);
    assert_eq!(report.xref.sections.len(), 1);
    let section = &report.xref.sections[0];
    assert!(section.offset > 0);
    assert_eq!(section.kind, XrefSectionKind::Classic);
    // Catalog, pages, two page/content pairs, and the font, plus the
    // free-list head.
    assert_eq!(section.entry_counts.total, 8);
    assert_eq!(section.entry_counts.live, 7);
    assert_eq!(section.entry_counts.free, 1);
    assert_eq!(section.entry_counts.compressed, 0);
    assert_eq!(section.prev, None);
    assert_eq!(section.auxiliary_of, None);
    assert!(section.recovery.is_empty());
}

#[test]
fn incremental_sections_link_newest_first_by_prev() {
    let report = examine_bytes(builder::phase1_exit_fixture(1));
    assert!(
        matches!(report.open, OpenOutcome::Ok),
        "open: {:?}",
        report.open
    );
    assert_eq!(report.xref.revision_count, 2);
    assert_eq!(report.xref.sections.len(), 2);
    let (update, base) = (&report.xref.sections[0], &report.xref.sections[1]);
    assert_eq!(update.kind, XrefSectionKind::Stream);
    assert_eq!(base.kind, XrefSectionKind::Classic);
    assert_eq!(update.prev, Some(base.offset));
    assert_eq!(base.prev, None);
    assert!(update.auxiliary_of.is_none() && base.auxiliary_of.is_none());
    // Update: the object stream, two compressed members, the rewritten
    // content stream, and the xref stream itself.
    assert_eq!(update.entry_counts.total, 5);
    assert_eq!(update.entry_counts.live, 3);
    assert_eq!(update.entry_counts.compressed, 2);
    assert_eq!(update.entry_counts.free, 0);
    // Base: catalog, pages, page, and content, plus the free-list head.
    assert_eq!(base.entry_counts.total, 5);
    assert_eq!(base.entry_counts.live, 4);
    assert_eq!(base.entry_counts.free, 1);
    assert_eq!(base.entry_counts.compressed, 0);
}

#[test]
fn hybrid_stream_marks_its_classic_table() {
    let report = examine_bytes(builder::hybrid_fixture(1));
    assert!(
        matches!(report.open, OpenOutcome::Ok),
        "open: {:?}",
        report.open
    );
    assert_eq!(report.xref.revision_count, 2);
    assert_eq!(report.xref.sections.len(), 2);
    let (table, stream) = (&report.xref.sections[0], &report.xref.sections[1]);
    assert_eq!(table.kind, XrefSectionKind::Classic);
    assert!(table.auxiliary_of.is_none());
    assert_eq!(stream.kind, XrefSectionKind::Stream);
    assert_eq!(stream.auxiliary_of, Some(0));
    // Table: catalog, pages, page, and content, plus the free-list head.
    assert_eq!(table.entry_counts.total, 5);
    assert_eq!(table.entry_counts.live, 4);
    assert_eq!(table.entry_counts.free, 1);
    // Stream: the same four objects plus the object stream, its member, and
    // the xref stream itself, plus the free-list head.
    assert_eq!(stream.entry_counts.total, 8);
    assert_eq!(stream.entry_counts.live, 6);
    assert_eq!(stream.entry_counts.compressed, 1);
    assert_eq!(stream.entry_counts.free, 1);
}
