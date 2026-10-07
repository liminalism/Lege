//! M4 strict ByteRange layer: validity classification over the original
//! bytes, M2 revision association, and `SIGNATURE_BYTE_RANGE_INVALID`
//! findings for bad ranges — all without touching cryptography.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]

use pdf_forensics::{
    AuditReport, ByteRangeValidity, Disposition, SIGNATURE_BYTE_RANGE_INVALID, SignatureFacts,
    audit_bytes,
};
use pdf_test_support::builder;

/// Audit `bytes` and require the report to satisfy the schema's own rules.
fn audit(bytes: &[u8]) -> AuditReport {
    let report = audit_bytes(bytes, None);
    report.validate().unwrap();
    report
}

/// The single signature of a signed fixture.
fn signature(report: &AuditReport) -> &SignatureFacts {
    let signatures = report.document.signatures.value().unwrap();
    assert_eq!(signatures.signatures.len(), 1);
    &signatures.signatures[0]
}

/// Whether the report carries a byte-range finding.
fn has_byterange_finding(report: &AuditReport) -> bool {
    report
        .findings
        .iter()
        .any(|finding| finding.code == SIGNATURE_BYTE_RANGE_INVALID)
}

#[test]
fn valid_range_spans_the_file_and_finds_nothing() {
    let bytes = builder::signed_valid();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert_eq!(signature.field_name.as_deref(), Some("Sig1"));
    assert_eq!(signature.byte_range_validity, ByteRangeValidity::Valid);
    assert!(signature.covers_whole_file);
    assert_eq!(signature.revision, Some(0));
    assert_eq!(signature.ends_at_revision_end, Some(true));
    assert!(!has_byterange_finding(&report));

    // The claimed runs straddle the /Contents hole and partition the file.
    assert_eq!(signature.covered_ranges.len(), 2);
    let (first, second) = (&signature.covered_ranges[0], &signature.covered_ranges[1]);
    assert_eq!(first.offset, 0);
    assert_eq!(
        first.offset + first.length,
        bytes.windows(10).position(|w| w == b"/Contents<").unwrap() as u64 + 9
    );
    assert_eq!(second.offset + second.length, bytes.len() as u64);

    // The JSON spelling of the new facts is the consumer contract.
    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    let json_sig = &json["document"]["signatures"]["value"]["signatures"][0];
    assert_eq!(json_sig["byte_range_validity"], "valid");
    assert_eq!(json_sig["revision"], 0);
    assert_eq!(json_sig["ends_at_revision_end"], true);
    assert_eq!(json_sig["covered_ranges"].as_array().unwrap().len(), 2);
}

#[test]
fn truncated_range_emits_a_finding() {
    let bytes = builder::signed_truncated();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert_eq!(signature.byte_range_validity, ByteRangeValidity::Truncated);
    assert!(!signature.covers_whole_file);
    assert!(has_byterange_finding(&report));
    assert_eq!(report.disposition, Disposition::ReviewRecommended);
}

#[test]
fn overlapping_range_emits_a_finding() {
    let bytes = builder::signed_overlapping();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert_eq!(
        signature.byte_range_validity,
        ByteRangeValidity::Overlapping
    );
    assert!(has_byterange_finding(&report));
}

#[test]
fn gap_within_the_own_revision_emits_a_finding() {
    // Well-formed, but trailing bytes its own single revision cannot
    // explain: the range does not end at its revision's %%EOF.
    let bytes = builder::signed_with_gap();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert_eq!(
        signature.byte_range_validity,
        ByteRangeValidity::GapContaining
    );
    assert_eq!(signature.revision, Some(0));
    assert_eq!(signature.ends_at_revision_end, Some(false));
    assert!(has_byterange_finding(&report));
}

#[test]
fn malformed_shapes_emit_findings() {
    for range in ["0 100 200", "0 100", "0 100 200 -5", "0 100 (x) 200", "0"] {
        let bytes = builder::signed_with_range(range);
        let report = audit(&bytes);

        let signature = signature(&report);
        assert_eq!(
            signature.byte_range_validity,
            ByteRangeValidity::Malformed,
            "range [{range}]"
        );
        assert!(signature.covered_ranges.is_empty(), "range [{range}]");
        assert!(has_byterange_finding(&report), "range [{range}]");
    }
}

#[test]
fn appended_update_after_signing_is_not_a_finding() {
    // Gap-containing against the whole file, but the range ends exactly
    // at its own revision's %%EOF: the legitimate post-signing update.
    let bytes = builder::signed_then_updated();
    let report = audit(&bytes);

    assert_eq!(
        report.document.revisions.value().unwrap().revisions.len(),
        2
    );
    let signature = signature(&report);
    assert_eq!(
        signature.byte_range_validity,
        ByteRangeValidity::GapContaining
    );
    assert!(!signature.covers_whole_file);
    assert_eq!(signature.revision, Some(0));
    assert_eq!(signature.ends_at_revision_end, Some(true));
    assert!(!has_byterange_finding(&report));
}
