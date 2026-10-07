//! Per-annotation identity reaches the audit report, and the section
//! leaves the pending list.
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::audit_bytes;

/// The annotation-heavy fixture audits to counts plus a per-annotation
/// identity list — and `annotation_identity` is no longer a pending gap.
#[test]
fn annotation_identity_reaches_the_report() {
    let report = audit_bytes(&pdf_test_support::builder::annotated(), None);
    report.validate().unwrap();

    let annotations = report.document.annotations.value().unwrap();
    assert_eq!(annotations.total(), 3);
    assert_eq!(annotations.count_per_subtype.get("Text"), Some(&1));

    let annots = &annotations.annotations;
    assert_eq!(annots.len(), 3);
    assert_eq!(annots[0].page, 0);
    assert_eq!(annots[0].object.as_deref(), Some("60 0 R"));
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

    assert_eq!(annots[2].subtype.as_deref(), Some("Ink"));
    assert!(annots[2].contents.is_none());

    assert!(
        !report
            .document
            .pending
            .iter()
            .any(|p| p.section == "annotation_identity"),
        "annotation_identity is surfaced, so it must not stay pending"
    );
}
