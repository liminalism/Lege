#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use pdf_document::DocumentLimits;
use pdf_object::ObjectId;
use pdf_read::{OpenOutcome, examine};
use pdf_source::{OwnedBytesSource, PdfSource};

/// The doctor inventories per-annotation identity alongside the subtype
/// counts: page, source object, and the verbatim provenance strings.
#[test]
fn annotation_inventory_carries_per_annotation_identity() {
    let source: Arc<dyn PdfSource> =
        Arc::new(OwnedBytesSource::new(pdf_test_support::builder::annotated()));
    let report = examine(source, DocumentLimits::default());
    assert!(
        matches!(report.open, OpenOutcome::Ok),
        "open: {:?}",
        report.open
    );

    assert_eq!(report.annotations.total(), 3);
    assert_eq!(report.annotations.count_per_subtype.get("Text"), Some(&1));

    let annots = &report.annotations.annotations;
    assert_eq!(annots.len(), 3);

    assert_eq!(annots[0].page, 0);
    assert_eq!(annots[0].object, Some(ObjectId::new(60, 0)));
    assert_eq!(annots[0].subtype.as_deref(), Some("Text"));
    assert_eq!(annots[0].contents.as_deref(), Some("margin note"));
    assert_eq!(annots[0].name.as_deref(), Some("note-1"));
    assert_eq!(annots[0].modified.as_deref(), Some("D:20240101120000Z"));
    assert_eq!(
        annots[0].creation_date.as_deref(),
        Some("D:20240101090000Z")
    );
    assert_eq!(annots[0].author.as_deref(), Some("Alice"));
    assert_eq!(annots[0].subject.as_deref(), Some("Review"));
    assert_eq!(annots[0].intent.as_deref(), Some("Comment"));

    assert_eq!(annots[1].subtype.as_deref(), Some("Highlight"));
    assert_eq!(annots[1].author.as_deref(), Some("Bob"));
    assert!(annots[1].intent.is_none());

    assert_eq!(annots[2].subtype.as_deref(), Some("Ink"));
    assert_eq!(annots[2].name.as_deref(), Some("ink-1"));
    assert!(annots[2].contents.is_none());
}
