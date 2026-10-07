//! M3 revision text diff: a controlled GPA edit between two saves reports
//! exact previous/current strings with its page rectangle, content objects,
//! and revision index, plus before/after/diff PNG evidence.
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::{
    AnalysisMode, AuditReport, BoundaryConfidence, Disposition, EvidenceStrength,
    LogicalRevisionFacts, OpenState, ParseAssessment, RevisionHistoryFacts,
    RevisionTextDiffOptions, TEXT_CHANGED_BETWEEN_REVISIONS, XrefEntryCounts, XrefSectionKind,
    audit_bytes, diff_revision_text, encode_png, highlight_changed_regions,
};
use pdf_test_support::builder;

/// Audit `bytes` and require the report to satisfy the schema's own rules.
fn audit(bytes: &[u8]) -> AuditReport {
    let report = audit_bytes(bytes, None);
    report.validate().unwrap();
    report
}

#[test]
fn gpa_edit_reports_exact_strings_rect_objects_and_revision() {
    let bytes = builder::gpa_edit();
    let report = audit(&bytes);
    let history = report.document.revisions.value().unwrap();
    assert_eq!(history.revisions.len(), 2);

    let outcome = diff_revision_text(
        &bytes,
        history,
        &report.document.parse,
        None,
        &RevisionTextDiffOptions::default(),
    );

    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.changes.len(), 1);
    let change = &outcome.changes[0];
    assert_eq!(change.page, 0);
    assert_eq!(change.previous_revision, 0);
    assert_eq!(change.revision, 1);
    assert_eq!(change.previous_text, "Cumulative GPA: 2.84");
    assert_eq!(change.current_text, "Cumulative GPA: 3.84");
    assert_eq!(change.objects, vec!["11 0 R".to_owned()]);

    let rect = change.rect.unwrap();
    assert!(rect[0] < rect[2] && rect[1] < rect[3]);
    assert!(rect[0] >= 0 && rect[1] >= 0 && rect[2] <= 612_000 && rect[3] <= 792_000);

    assert_eq!(change.finding.code, TEXT_CHANGED_BETWEEN_REVISIONS);
    assert_eq!(change.finding.strength, EvidenceStrength::StrongDerived);
    assert!(change.finding.limitation.is_some());
    assert_eq!(change.finding.requires_mode, AnalysisMode::Full);
    assert!(!change.finding.rests_on_recovered_structure);
    assert!(change.finding.explanation.contains("Cumulative GPA: 2.84"));
    assert!(change.finding.explanation.contains("Cumulative GPA: 3.84"));

    // The emitted finding satisfies the schema's own rules.
    let finding_report = AuditReport::new_with_facts(
        AnalysisMode::Full,
        Disposition::ReviewRecommended,
        vec![change.finding.clone()],
        report.document.clone(),
    );
    finding_report.validate().unwrap();
}

#[test]
fn gpa_edit_carries_before_after_and_diff_pngs() {
    let bytes = builder::gpa_edit();
    let report = audit(&bytes);
    let history = report.document.revisions.value().unwrap();

    let outcome = diff_revision_text(
        &bytes,
        history,
        &report.document.parse,
        None,
        &RevisionTextDiffOptions::default(),
    );

    let change = &outcome.changes[0];
    assert_eq!(
        (change.before.width(), change.before.height()),
        (change.after.width(), change.after.height())
    );
    let diff = change.diff.as_ref().unwrap();
    assert!(diff.changed_pixels > 0);
    assert!(!diff.regions.is_empty());

    let overlay = highlight_changed_regions(&change.after, diff).unwrap();
    let before_png = encode_png(&change.before).unwrap();
    let after_png = encode_png(&change.after).unwrap();
    let diff_png = encode_png(&overlay).unwrap();
    assert!(!before_png.is_empty());
    assert!(!after_png.is_empty());
    assert!(!diff_png.is_empty());
    assert_ne!(before_png, after_png);
    assert_eq!(encode_png(&change.before).unwrap(), before_png);
}

#[test]
fn single_revision_document_has_no_text_changes() {
    let bytes = builder::multipage_classic(1);
    let report = audit(&bytes);
    let history = report.document.revisions.value().unwrap();

    let outcome = diff_revision_text(
        &bytes,
        history,
        &report.document.parse,
        None,
        &RevisionTextDiffOptions::default(),
    );

    assert!(outcome.changes.is_empty());
    assert!(outcome.skipped.is_empty());
}

/// Two revisions where the later save's end was never established: the pair
/// is reported as skipped, never as unchanged and never as an error.
#[test]
fn revision_without_an_established_end_is_skipped() {
    let bytes = builder::multipage_classic(1);
    let end = bytes.len() as u64;
    let history = RevisionHistoryFacts {
        revisions: vec![
            LogicalRevisionFacts {
                index: 0,
                section_offset: 0,
                section_kind: XrefSectionKind::Classic,
                entry_counts: XrefEntryCounts::default(),
                auxiliary_sections: Vec::new(),
                start: Some(0),
                end: Some(end),
                boundary_confidence: BoundaryConfidence::High,
                boundary_note: None,
                changes: Vec::new(),
            },
            LogicalRevisionFacts {
                index: 1,
                section_offset: 0,
                section_kind: XrefSectionKind::Classic,
                entry_counts: XrefEntryCounts::default(),
                auxiliary_sections: Vec::new(),
                start: Some(end),
                end: None,
                boundary_confidence: BoundaryConfidence::Low,
                boundary_note: Some("no established end".to_owned()),
                changes: Vec::new(),
            },
        ],
        ..RevisionHistoryFacts::default()
    };
    let parse = ParseAssessment {
        state: OpenState::Clean,
        recovery_events: Vec::new(),
        xref_rebuilt: false,
        xref_section_count: 2,
    };

    let outcome = diff_revision_text(
        &bytes,
        &history,
        &parse,
        None,
        &RevisionTextDiffOptions::default(),
    );

    assert!(outcome.changes.is_empty());
    assert_eq!(outcome.skipped.len(), 1);
    assert_eq!(outcome.skipped[0].previous_revision, 0);
    assert_eq!(outcome.skipped[0].revision, 1);
    assert_eq!(outcome.skipped[0].page, None);
}

#[test]
fn finding_rests_on_recovered_structure_when_the_parse_did() {
    let bytes = builder::gpa_edit();
    let report = audit(&bytes);
    let history = report.document.revisions.value().unwrap();
    let mut parse = report.document.parse.clone();
    parse.recovery_events.push("test recovery".to_owned());

    let outcome = diff_revision_text(
        &bytes,
        history,
        &parse,
        None,
        &RevisionTextDiffOptions::default(),
    );

    assert_eq!(outcome.changes.len(), 1);
    assert!(outcome.changes[0].finding.rests_on_recovered_structure);
}
