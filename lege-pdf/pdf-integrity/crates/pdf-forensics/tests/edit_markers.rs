//! `/TouchUp_*` marked-content spans reach the audit report as per-page
//! edit markers, and the section leaves the pending list.
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::{Availability, UnavailableReason, audit_bytes};

#[test]
fn touchup_markers_reach_the_report() {
    let report = audit_bytes(&pdf_test_support::builder::touchup_edit(), None);
    report.validate().unwrap();

    let markers = report.document.edit_markers.value().unwrap();
    // Sparse: only the marked page appears, and the neutral /Span span on
    // page 1 is not an edit marker.
    assert_eq!(markers.len(), 1, "{markers:?}");
    assert_eq!(markers[0].page, 0);
    assert_eq!(markers[0].markers.len(), 1);
    let marker = &markers[0].markers[0];
    assert_eq!(marker.tag, "TouchUp_TextEdit");
    assert!(
        marker
            .close_offset
            .is_some_and(|close| close > marker.open_offset),
        "{marker:?}"
    );
    // Millipoints: the re-typed run sits at (72,700) in 12pt.
    let bbox = marker.bbox.unwrap();
    assert_eq!(bbox[0], 72_000, "{bbox:?}");
    assert_eq!(bbox[1], 700_000, "{bbox:?}");
    assert!(bbox[2] > bbox[0] + 10_000, "{bbox:?}");
    assert_eq!(bbox[3], 712_000, "{bbox:?}");

    assert!(
        !report
            .document
            .pending
            .iter()
            .any(|p| p.section == "edit_markers"),
        "edit_markers is surfaced, so it must not stay pending"
    );
}

#[test]
fn clean_document_reports_no_markers_not_a_gap() {
    let report = audit_bytes(&pdf_test_support::builder::multipage_classic(1), None);
    assert_eq!(report.document.edit_markers.value().map(Vec::len), Some(0));
}

#[test]
fn unopenable_file_reports_markers_as_unavailable() {
    let report = audit_bytes(b"this is not a PDF at all", None);
    assert!(matches!(
        report.document.edit_markers,
        Availability::Unavailable {
            reason: UnavailableReason::Unparseable,
            ..
        }
    ));
}
