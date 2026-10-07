#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex};
use pdf_object::ObjectId;
use pdf_source::{OwnedBytesSource, PdfSource};

/// The annotation-heavy fixture's provenance survives onto `PageAnnotation`:
/// source ids, text fields, dates verbatim, and the `/IT` intent — with
/// absent keys staying `None` rather than defaulting.
#[test]
fn page_annotations_carry_neutral_provenance() {
    let source: Arc<dyn PdfSource> =
        Arc::new(OwnedBytesSource::new(pdf_test_support::builder::annotated()));
    let snapshot =
        DocumentSnapshot::open(source, DocumentLimits::default()).expect("fixture opens");
    let page = snapshot.page(PageIndex(0)).expect("page 0");
    assert_eq!(page.annotations.len(), 3);

    let names = snapshot.names();
    let subtype = |index: usize| {
        page.annotations[index]
            .subtype
            .map(|id| String::from_utf8_lossy(&names.resolve(id)).into_owned())
    };
    assert_eq!(subtype(0).as_deref(), Some("Text"));
    assert_eq!(subtype(1).as_deref(), Some("Highlight"));
    assert_eq!(subtype(2).as_deref(), Some("Ink"));

    let note = &page.annotations[0];
    assert_eq!(note.source, Some(ObjectId::new(60, 0)));
    assert_eq!(note.contents.as_deref(), Some("margin note"));
    assert_eq!(note.name.as_deref(), Some("note-1"));
    assert_eq!(note.modified.as_deref(), Some("D:20240101120000Z"));
    assert_eq!(note.creation_date.as_deref(), Some("D:20240101090000Z"));
    assert_eq!(note.author.as_deref(), Some("Alice"));
    assert_eq!(note.subject.as_deref(), Some("Review"));
    assert_eq!(note.intent.as_deref(), Some("Comment"));

    let highlight = &page.annotations[1];
    assert_eq!(highlight.source, Some(ObjectId::new(61, 0)));
    assert_eq!(highlight.contents.as_deref(), Some("highlighted passage"));
    assert_eq!(highlight.name.as_deref(), Some("hl-1"));
    assert_eq!(highlight.author.as_deref(), Some("Bob"));
    assert!(highlight.creation_date.is_none());
    assert!(highlight.subject.is_none());
    assert!(highlight.intent.is_none());

    let ink = &page.annotations[2];
    assert_eq!(ink.source, Some(ObjectId::new(62, 0)));
    assert_eq!(ink.name.as_deref(), Some("ink-1"));
    assert!(ink.contents.is_none());
    assert!(ink.modified.is_none());
    assert!(ink.author.is_none());
}
