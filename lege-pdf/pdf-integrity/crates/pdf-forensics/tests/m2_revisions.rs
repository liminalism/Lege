//! M2 revision-history tests: logical-revision grouping over known-answer
//! documents.
//!
//! Each test audits bytes built below or by `pdf-test-support` and pins the
//! revision count, the oldest-to-newest grouping, and the `startxref`-plus-
//! `%%EOF` boundary pairing. Hybrid saves count once, linearization stubs
//! are not revisions, and a rebuilt xref reports membership as unknown.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]

use pdf_forensics::{
    AuditReport, Availability, BoundaryConfidence, LogicalRevisionFacts, ObjectChangeFacts,
    ObjectChangeKind, ObjectLocationFacts, OpenState, RevisionHistoryFacts, UnavailableReason,
    XrefSectionKind, audit_bytes,
};
use pdf_test_support::builder;

/// Audit `bytes` and require the report to satisfy the schema's own rules.
fn audit(bytes: &[u8]) -> AuditReport {
    let report = audit_bytes(bytes, None);
    report.validate().unwrap();
    report
}

fn history(report: &AuditReport) -> &RevisionHistoryFacts {
    report.document.revisions.value().unwrap()
}

/// The change record for `object` (`N G R`) in `revision`.
fn change<'a>(revision: &'a LogicalRevisionFacts, object: &str) -> &'a ObjectChangeFacts {
    revision
        .changes
        .iter()
        .find(|change| change.object == object)
        .unwrap_or_else(|| panic!("no change for {object} in revision {}", revision.index))
}

/// Absolute end (one past the `F`) of the first `%%EOF` in `bytes`.
fn first_eof_end(bytes: &[u8]) -> u64 {
    bytes
        .windows(5)
        .position(|w| w == b"%%EOF")
        .map(|pos| pos as u64 + 5)
        .unwrap()
}

/// Absolute end (one past the `F`) of the last `%%EOF` in `bytes`.
fn last_eof_end(bytes: &[u8]) -> u64 {
    bytes
        .windows(5)
        .rposition(|w| w == b"%%EOF")
        .map(|pos| pos as u64 + 5)
        .unwrap()
}

#[test]
fn single_save_is_one_high_confidence_revision() {
    let bytes = builder::multipage_classic(2);
    let report = audit(&bytes);

    assert_eq!(report.document.parse.state, OpenState::Clean);
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    assert!(history.linearization_stubs.is_empty());
    let revision = &history.revisions[0];
    assert_eq!(revision.index, 0);
    assert_eq!(revision.section_kind, XrefSectionKind::Classic);
    assert_eq!(revision.entry_counts.total, 8);
    assert!(revision.auxiliary_sections.is_empty());
    assert_eq!(revision.start, Some(0));
    assert_eq!(revision.end, Some(last_eof_end(&bytes)));
    assert_eq!(revision.boundary_confidence, BoundaryConfidence::High);
    assert_eq!(revision.boundary_note, None);

    // The JSON spelling of the revisions section is the consumer contract.
    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    assert_eq!(json["document"]["revisions"]["status"], "available");
    let first = &json["document"]["revisions"]["value"]["revisions"][0];
    assert_eq!(first["section_kind"], "classic");
    assert_eq!(first["boundary_confidence"], "high");
}

#[test]
fn incremental_update_is_two_ordered_revisions() {
    let bytes = builder::phase1_exit_fixture(1);
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);
    let (first, second) = (&history.revisions[0], &history.revisions[1]);
    assert_eq!((first.index, second.index), (0, 1));
    assert_eq!(first.section_kind, XrefSectionKind::Classic);
    assert_eq!(second.section_kind, XrefSectionKind::Stream);
    // Ranges partition the file in order: each save ends where its own
    // `startxref`/`%%EOF` tail says, and the next starts there.
    assert_eq!(first.start, Some(0));
    assert_eq!(first.end, Some(first_eof_end(&bytes)));
    assert_eq!(second.start, first.end);
    assert_eq!(second.end, Some(last_eof_end(&bytes)));
    assert_eq!(first.boundary_confidence, BoundaryConfidence::High);
    assert_eq!(second.boundary_confidence, BoundaryConfidence::High);
}

#[test]
fn hybrid_reference_save_counts_once() {
    let bytes = builder::hybrid_fixture(1);
    let report = audit(&bytes);

    // Two xref sections, one user-visible save.
    assert_eq!(report.document.parse.xref_section_count, 2);
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    let revision = &history.revisions[0];
    assert_eq!(revision.section_kind, XrefSectionKind::Classic);
    assert_eq!(revision.auxiliary_sections.len(), 1);
    assert_eq!(revision.auxiliary_sections[0].kind, XrefSectionKind::Stream);
    assert_eq!(revision.end, Some(last_eof_end(&bytes)));
    assert_eq!(revision.boundary_confidence, BoundaryConfidence::High);
}

#[test]
fn linearized_stub_is_not_a_revision() {
    let (bytes, stub, main) = linearized_two_section();
    let report = audit(&bytes);

    // The stub's forward `/Prev` closes a chain cycle, so the run recovers;
    // that by-design quirk must not become a second revision.
    assert!(matches!(report.document.parse.state, OpenState::Recovered));
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    assert_eq!(history.linearization_stubs, vec![stub]);
    let revision = &history.revisions[0];
    assert_eq!(revision.section_offset, main);
    assert_eq!(revision.section_kind, XrefSectionKind::Classic);
    assert_eq!(revision.entry_counts.total, 1);
    assert_eq!(revision.start, Some(0));
    // The save spans the whole file: the stub's own mid-file `%%EOF` is not
    // a revision boundary.
    assert_eq!(revision.end, Some(last_eof_end(&bytes)));
    assert_ne!(revision.end, Some(first_eof_end(&bytes)));
    assert_eq!(revision.boundary_confidence, BoundaryConfidence::High);
}

#[test]
fn linearized_marker_only_is_one_revision() {
    // The marker-only shape carries a single section: exclusion must not
    // strand it into zero revisions.
    let bytes = builder::linearized();
    let report = audit(&bytes);

    assert!(report.document.linearization.value().unwrap().linearized);
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    assert!(history.linearization_stubs.is_empty());
    assert_eq!(history.revisions[0].end, Some(last_eof_end(&bytes)));
}

#[test]
fn rebuilt_xref_reports_membership_unknown() {
    let bytes = builder::without_xref(builder::multipage_classic(1));
    let report = audit(&bytes);

    assert!(report.document.parse.xref_rebuilt);
    match &report.document.revisions {
        Availability::Unavailable { reason, detail } => {
            assert_eq!(*reason, UnavailableReason::Ambiguous);
            assert!(
                detail.contains("original revision membership cannot be established"),
                "detail: {detail}"
            );
        }
        Availability::Available { .. } => {
            panic!("a rebuilt xref must not yield grouped revisions")
        }
    }
}

#[test]
fn missing_startxref_leaves_boundary_unestablished() {
    let bytes = builder::without_startxref(builder::multipage_classic(1));
    let report = audit(&bytes);

    // The section is walked via recovery, but its tail is gone: the dangling
    // `%%EOF` is never paired without a valid `startxref`.
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    let revision = &history.revisions[0];
    assert_eq!(revision.start, Some(0));
    assert_eq!(revision.end, None);
    assert_eq!(revision.boundary_confidence, BoundaryConfidence::Low);
    assert!(
        revision
            .boundary_note
            .as_deref()
            .unwrap_or_default()
            .contains("startxref"),
        "note: {:?}",
        revision.boundary_note
    );
}

#[test]
fn structure_only_audit_still_reports_revisions() {
    // AES-256 declines to open, but the sections and their tails are
    // structural facts a structure-only run still reaches.
    let bytes = builder::aes256_encrypted_fixture();
    let report = audit(&bytes);

    assert!(matches!(
        report.document.parse.state,
        OpenState::Failed { .. }
    ));
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    assert_eq!(history.revisions[0].end, Some(last_eof_end(&bytes)));
    assert_eq!(
        history.revisions[0].boundary_confidence,
        BoundaryConfidence::High
    );
}

/// A genuinely linearized two-section file: the `/Linearized` parameter
/// dictionary first, first-page objects, a first-page xref whose trailer
/// `/Prev` points *forward* to the main xref, the remaining objects, and the
/// main xref with a backward `/Prev`. Both sections carry their own
/// `startxref`/`%%EOF` tail, as linear writers emit.
///
/// Returns the bytes plus the stub (first-page) and main xref offsets.
fn linearized_two_section() -> (Vec<u8>, u64, u64) {
    let mut buf = b"%PDF-1.7\n".to_vec();
    let mut offsets = [0u64; 7];

    // The linearization parameter dictionary must be the first object. /L
    // (file length), /E (end of first page), and /T (main xref offset) are
    // unknown until the file is laid out, so they are patched below.
    offsets[5] = buf.len() as u64;
    buf.extend_from_slice(b"5 0 obj\n<</Linearized 1");
    let l_at = buf.len();
    buf.extend_from_slice(b"/L 0000000000");
    buf.extend_from_slice(b"/H [0 0]/O 3");
    let e_at = buf.len();
    buf.extend_from_slice(b"/E 0000000000");
    buf.extend_from_slice(b"/N 1");
    let t_at = buf.len();
    buf.extend_from_slice(b"/T 0000000000");
    buf.extend_from_slice(b"/P 0>>\nendobj\n");

    offsets[1] = buf.len() as u64;
    buf.extend_from_slice(b"1 0 obj\n<</Type/Catalog/Pages 2 0 R>>\nendobj\n");
    offsets[2] = buf.len() as u64;
    buf.extend_from_slice(
        b"2 0 obj\n<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 612 792]>>\nendobj\n",
    );
    offsets[3] = buf.len() as u64;
    buf.extend_from_slice(
        b"3 0 obj\n<</Type/Page/Parent 2 0 R/Contents 4 0 R\
          /Resources<</Font<</F1 6 0 R>>>>>>\nendobj\n",
    );
    let content = b"BT /F1 12 Tf 72 720 Td (first page) Tj ET";
    offsets[4] = buf.len() as u64;
    buf.extend_from_slice(format!("4 0 obj\n<</Length {}>>\nstream\n", content.len()).as_bytes());
    buf.extend_from_slice(content);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    // First-page xref over objects 0-5.
    let stub = buf.len() as u64;
    buf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f\r\n");
    for number in 1..=5u32 {
        buf.extend_from_slice(format!("{:010} 00000 n\r\n", offsets[number as usize]).as_bytes());
    }
    buf.extend_from_slice(b"trailer\n<</Size 7");
    let prev_at = buf.len();
    buf.extend_from_slice(b"/Prev 0000000000");
    buf.extend_from_slice(format!(">>\nstartxref\n{stub}\n%%EOF\n").as_bytes());

    // The remaining object, then the main xref with its backward /Prev.
    offsets[6] = buf.len() as u64;
    buf.extend_from_slice(b"6 0 obj\n<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>\nendobj\n");
    let main = buf.len() as u64;
    buf.extend_from_slice(format!("xref\n6 1\n{:010} 00000 n\r\n", offsets[6]).as_bytes());
    buf.extend_from_slice(
        format!("trailer\n<</Size 7/Root 1 0 R/Prev {stub}>>\nstartxref\n{main}\n%%EOF\n")
            .as_bytes(),
    );

    // Patch the forward references, ten zero-padded digits each. Patching
    // preserves the length, so /L measures the final file.
    let file_len = buf.len() as u64;
    patch_decimal(&mut buf, l_at + 3, file_len);
    patch_decimal(&mut buf, e_at + 3, stub);
    patch_decimal(&mut buf, t_at + 3, main);
    patch_decimal(&mut buf, prev_at + 6, main);
    (buf, stub, main)
}

/// Overwrite ten ASCII digits at `digits_at` with the zero-padded value.
fn patch_decimal(buf: &mut [u8], digits_at: usize, value: u64) {
    let text = format!("{value:010}");
    assert_eq!(text.len(), 10);
    buf[digits_at..digits_at + 10].copy_from_slice(text.as_bytes());
}

#[test]
fn xref_stream_only_save_is_one_stream_revision() {
    let bytes = builder::xref_stream_fixture(1);
    let report = audit(&bytes);

    assert_eq!(report.document.parse.state, OpenState::Clean);
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    let revision = &history.revisions[0];
    assert_eq!(revision.section_kind, XrefSectionKind::Stream);
    assert_eq!(revision.entry_counts.total, 8);
    assert_eq!(revision.entry_counts.compressed, 1);
    assert!(revision.auxiliary_sections.is_empty());

    // A single save adds every live member — including the compressed
    // font — and frees the object-0 head.
    assert_eq!(revision.changes.len(), 8);
    assert_eq!(
        change(revision, "0 65535 R").change,
        ObjectChangeKind::Freed
    );
    for member in revision.changes.iter().filter(|c| c.object != "0 65535 R") {
        assert_eq!(member.change, ObjectChangeKind::Added, "{}", member.object);
    }
    let font = change(revision, "101 0 R");
    assert!(matches!(
        font.location,
        ObjectLocationFacts::InObjectStream {
            container: 100,
            index: 0
        }
    ));
}

#[test]
fn classic_second_revision_redefines_rewritten_content() {
    let bytes = builder::incremental_redefined_content();
    let report = audit(&bytes);

    assert_eq!(report.document.parse.state, OpenState::Clean);
    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);
    let (first, second) = (&history.revisions[0], &history.revisions[1]);
    assert_eq!(first.section_kind, XrefSectionKind::Classic);
    assert_eq!(second.section_kind, XrefSectionKind::Classic);
    assert_eq!(first.entry_counts.total, 6);
    assert_eq!(second.entry_counts.total, 1);
    assert_eq!(first.end, Some(first_eof_end(&bytes)));
    assert_eq!(second.start, first.end);
    assert_eq!(second.end, Some(last_eof_end(&bytes)));

    // The update rewrites exactly one object; the font it reuses is not
    // a member of the second save at all.
    assert_eq!(change(first, "101 0 R").change, ObjectChangeKind::Added);
    assert_eq!(second.changes.len(), 1);
    let content = change(second, "11 0 R");
    assert_eq!(content.change, ObjectChangeKind::Redefined);
    assert!(matches!(content.location, ObjectLocationFacts::Offset(_)));
    assert!(
        second.changes.iter().all(|c| c.object != "101 0 R"),
        "the reused font is untouched by the update"
    );
}

#[test]
fn classic_second_revision_adds_new_objects() {
    let bytes = builder::incremental_added_objects();
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);
    let second = &history.revisions[1];
    assert_eq!(second.section_kind, XrefSectionKind::Classic);
    assert_eq!(second.entry_counts.total, 2);

    // Nothing redefined: every second-save member is an addition.
    assert_eq!(second.changes.len(), 2);
    assert_eq!(change(second, "102 0 R").change, ObjectChangeKind::Added);
    assert_eq!(change(second, "103 0 R").change, ObjectChangeKind::Added);
}

#[test]
fn xref_stream_second_revision_redefines_and_adds() {
    let bytes = builder::incremental_xref_stream();
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);
    let second = &history.revisions[1];
    assert_eq!(second.section_kind, XrefSectionKind::Stream);
    assert_eq!(second.entry_counts.total, 3);

    assert_eq!(second.changes.len(), 3);
    assert_eq!(change(second, "11 0 R").change, ObjectChangeKind::Redefined);
    assert_eq!(change(second, "102 0 R").change, ObjectChangeKind::Added);
    assert_eq!(change(second, "200 0 R").change, ObjectChangeKind::Added);
}

#[test]
fn hybrid_second_revision_counts_once() {
    let bytes = builder::incremental_hybrid();
    let report = audit(&bytes);

    // Three xref sections, two user-visible saves.
    assert_eq!(report.document.parse.xref_section_count, 3);
    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);
    let second = &history.revisions[1];
    assert_eq!(second.section_kind, XrefSectionKind::Classic);
    assert_eq!(second.entry_counts.total, 2);
    assert_eq!(second.auxiliary_sections.len(), 1);
    let auxiliary = &second.auxiliary_sections[0];
    assert_eq!(auxiliary.kind, XrefSectionKind::Stream);
    assert_eq!(auxiliary.entry_counts.total, 5);
    assert_eq!(second.end, Some(last_eof_end(&bytes)));
    assert_eq!(second.boundary_confidence, BoundaryConfidence::High);

    // The save's members span both sections: the table re-states the
    // rewritten content and the object stream, the auxiliary stream adds
    // the compressed member and the stream itself, plus the re-stated
    // free-list head.
    assert_eq!(second.changes.len(), 5);
    assert_eq!(
        change(second, "0 65535 R").change,
        ObjectChangeKind::RepeatedIdentically
    );
    let content = change(second, "11 0 R");
    assert_eq!(content.change, ObjectChangeKind::Redefined);
    assert!(matches!(content.location, ObjectLocationFacts::Offset(_)));
    assert_eq!(change(second, "100 0 R").change, ObjectChangeKind::Added);
    let member = change(second, "102 0 R");
    assert_eq!(member.change, ObjectChangeKind::Added);
    assert!(matches!(
        member.location,
        ObjectLocationFacts::InObjectStream {
            container: 100,
            index: 0
        }
    ));
    assert_eq!(change(second, "200 0 R").change, ObjectChangeKind::Added);
}

#[test]
fn linearized_save_classifies_its_single_member() {
    let (bytes, _, _) = linearized_two_section();
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    // The stub contributes no members: only the main section's object.
    assert_eq!(history.revisions[0].changes.len(), 1);
    assert_eq!(
        change(&history.revisions[0], "6 0 R").change,
        ObjectChangeKind::Added
    );
}

#[test]
fn unestablished_boundary_still_classifies_declared_members() {
    let bytes = builder::without_startxref(builder::multipage_classic(1));
    let report = audit(&bytes);

    // The missing tail unmoors the range, not the membership: the walked
    // section still declares its objects.
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    let revision = &history.revisions[0];
    assert_eq!(revision.end, None);
    assert_eq!(revision.changes.len(), 6);
    assert_eq!(change(revision, "10 0 R").change, ObjectChangeKind::Added);
}
