//! Findings policy: every detector's findings reach the verdict, and the
//! verdict follows the policy — review recommended when any review indicator
//! exists, never an all-clear while the inventory is still partial.
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::{
    AnalysisMode, Disposition, EvidenceStrength, Finding, FindingCode, PendingSection,
    TEXT_CHANGED_BETWEEN_REVISIONS, audit_bytes, audit_bytes_with_evidence, decide_disposition,
    write_evidence_pngs,
};
use pdf_test_support::builder;

const PNG_MAGIC: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

#[test]
fn revision_text_change_recommends_review() {
    let bytes = builder::gpa_edit();
    let report = audit_bytes(&bytes, None);
    report.validate().unwrap();

    assert_eq!(report.disposition, Disposition::ReviewRecommended);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == TEXT_CHANGED_BETWEEN_REVISIONS),
        "expected a revision text finding in {:?}",
        report.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
    );
}

#[test]
fn annotation_overlay_recommends_review() {
    let bytes = builder::freetext_overlay();
    let report = audit_bytes(&bytes, None);
    report.validate().unwrap();

    assert_eq!(report.disposition, Disposition::ReviewRecommended);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == pdf_forensics::EXPLICIT_ANNOTATION_OVER_TEXT),
        "expected an overlay finding in {:?}",
        report.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
    );
}

#[test]
fn flattened_occlusion_recommends_review() {
    let bytes = builder::white_rect_replacement();
    let report = audit_bytes(&bytes, None);
    report.validate().unwrap();

    assert_eq!(report.disposition, Disposition::ReviewRecommended);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == pdf_forensics::PAGE_CONTENT_COVERS_TEXT),
        "expected an occlusion finding in {:?}",
        report.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
    );
}

#[test]
fn clean_document_reports_no_findings_and_stays_inconclusive() {
    let report = audit_bytes(&builder::multipage_classic(1), None);
    report.validate().unwrap();

    assert!(report.findings.is_empty());
    assert!(!report.document.pending.is_empty());
    assert_eq!(report.disposition, Disposition::Inconclusive);
}

#[test]
fn all_clear_requires_no_findings_and_no_pending() {
    let pending = vec![PendingSection::new("signature_cryptography", "M4")];

    assert_eq!(
        decide_disposition(&[], &pending),
        Disposition::Inconclusive,
        "a partial inventory cannot declare an all-clear"
    );
    assert_eq!(
        decide_disposition(&[], &[]),
        Disposition::NoReviewIndicatorFound
    );
    assert_eq!(
        decide_disposition(&[review_indicator()], &pending),
        Disposition::ReviewRecommended
    );
    assert_eq!(
        decide_disposition(&[review_indicator()], &[]),
        Disposition::ReviewRecommended
    );
}

/// One contextual review indicator, for exercising the policy directly.
fn review_indicator() -> Finding {
    Finding {
        code: FindingCode::new("test.review_indicator"),
        strength: EvidenceStrength::ContextualIndicator,
        explanation: "a review indicator for the policy test".to_owned(),
        limitation: Some("test only".to_owned()),
        requires_mode: AnalysisMode::Full,
        rests_on_recovered_structure: false,
    }
}

#[test]
fn evidence_pngs_are_written_with_deterministic_names_and_bytes() {
    let bytes = builder::gpa_edit();
    let (_, evidence) = audit_bytes_with_evidence(&bytes, None);

    let first = work_dir("policy-first");
    let second = work_dir("policy-second");
    let first_files = write_evidence_pngs(&evidence, &first).unwrap();
    let second_files = write_evidence_pngs(&evidence, &second).unwrap();

    assert_eq!(
        first_files,
        vec![
            "rev00-to-rev01-page-000-after.png".to_owned(),
            "rev00-to-rev01-page-000-before.png".to_owned(),
            "rev00-to-rev01-page-000-diff.png".to_owned(),
        ]
    );
    assert_eq!(first_files, second_files);
    for name in &first_files {
        let earlier = std::fs::read(first.join(name)).unwrap();
        let later = std::fs::read(second.join(name)).unwrap();
        assert!(earlier.starts_with(&PNG_MAGIC), "{name} is a PNG");
        assert_eq!(earlier, later, "{name} is deterministic");
    }

    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

/// A scratch directory unique to this test process and name.
fn work_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "pdf-forensics-findings-{}-{name}",
        std::process::id()
    ))
}
