//! M4 DocMDP/FieldMDP layer: permission policies parsed from the
//! signature, exact post-signature object changes via the M2 change engine,
//! compliance evaluation, and `POST_SIGNATURE_REVISION` /
//! `DOCMDP_POLICY_VIOLATION` findings — silent for permitted fills, loud
//! for violating edits.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]

use pdf_forensics::{
    AuditReport, DOCMDP_POLICY_VIOLATION, Disposition, DocMdpPermission, FieldMdpAction,
    FindingCode, ObjectChangeKind, ObjectRole, POST_SIGNATURE_REVISION, SignatureFacts,
    audit_bytes, post_signature_changes,
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

/// How many findings carry `code`.
fn count_code(report: &AuditReport, code: &FindingCode) -> usize {
    report
        .findings
        .iter()
        .filter(|finding| &finding.code == code)
        .count()
}

/// The explanation of the first finding carrying `code`.
fn explanation(report: &AuditReport, code: &FindingCode) -> String {
    report
        .findings
        .iter()
        .find(|finding| &finding.code == code)
        .unwrap()
        .explanation
        .clone()
}

/// No DocMDP/FieldMDP finding of either code.
fn assert_docmdp_silent(report: &AuditReport) {
    assert_eq!(count_code(report, &POST_SIGNATURE_REVISION), 0);
    assert_eq!(count_code(report, &DOCMDP_POLICY_VIOLATION), 0);
}

#[test]
fn docmdp_policy_parses_into_facts() {
    let bytes = builder::signed_docmdp_p2_then_form_fill();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert_eq!(signature.revision, Some(0));
    assert_eq!(signature.ends_at_revision_end, Some(true));
    let policy = signature.docmdp.as_ref().unwrap();
    assert_eq!(policy.p, Some(2));
    assert_eq!(policy.permission, Some(DocMdpPermission::FormFillAndSign));
    assert!(signature.fieldmdp.is_none());

    // The JSON spelling of the policy facts is the consumer contract.
    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    let json_sig = &json["document"]["signatures"]["value"]["signatures"][0];
    assert_eq!(json_sig["docmdp"]["p"], 2);
    assert_eq!(json_sig["docmdp"]["permission"], "form_fill_and_sign");
    assert!(json_sig["fieldmdp"].is_null());
}

#[test]
fn permitted_form_fill_stays_silent() {
    let bytes = builder::signed_docmdp_p2_then_form_fill();
    let report = audit(&bytes);

    // The silence is evaluated, not blind: the detector saw the redefined
    // field and permitted it.
    let changes = post_signature_changes(&report.document, signature(&report));
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].revision, 1);
    assert_eq!(changes[0].object, "7 0 R");
    assert_eq!(changes[0].change, ObjectChangeKind::Redefined);
    assert_eq!(changes[0].role, ObjectRole::Other);

    assert_docmdp_silent(&report);
    assert!(report.findings.is_empty());
}

#[test]
fn content_edit_emits_both_findings_with_exact_objects() {
    let bytes = builder::signed_docmdp_p2_then_content_edit();
    let report = audit(&bytes);

    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 1);
    assert_eq!(report.disposition, Disposition::ReviewRecommended);

    let post = explanation(&report, &POST_SIGNATURE_REVISION);
    assert!(post.contains("Sig1"), "{post}");
    assert!(post.contains("revision 0"), "{post}");
    assert!(post.contains("DocMDP P=2"), "{post}");
    assert!(
        post.contains("rev 1: 11 0 R redefined (contents)"),
        "{post}"
    );

    let violation = explanation(&report, &DOCMDP_POLICY_VIOLATION);
    assert!(violation.contains("DocMDP P=2"), "{violation}");
    assert!(
        violation.contains("11 0 R redefined (contents)"),
        "{violation}"
    );
    assert!(violation.contains("page content"), "{violation}");

    // The content edit is also a text change: the layers agree.
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == pdf_forensics::TEXT_CHANGED_BETWEEN_REVISIONS),
    );
}

#[test]
fn p1_fill_violates_because_nothing_is_permitted() {
    let bytes = builder::signed_docmdp_p1_then_form_fill();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert_eq!(
        signature.docmdp.as_ref().unwrap().permission,
        Some(DocMdpPermission::NoChanges)
    );
    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 1);
    let violation = explanation(&report, &DOCMDP_POLICY_VIOLATION);
    assert!(violation.contains("7 0 R redefined"), "{violation}");
    assert!(
        violation.contains("no changes are permitted"),
        "{violation}"
    );
}

#[test]
fn p3_content_edit_still_violates() {
    let bytes = builder::signed_docmdp_p3_then_content_edit();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert_eq!(
        signature.docmdp.as_ref().unwrap().permission,
        Some(DocMdpPermission::FormFillSignAndAnnotate)
    );
    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 1);
    let violation = explanation(&report, &DOCMDP_POLICY_VIOLATION);
    assert!(
        violation.contains("11 0 R redefined (contents)"),
        "{violation}"
    );
}

#[test]
fn p3_annot_edit_stays_silent() {
    let bytes = builder::signed_docmdp_p3_then_annot_edit();
    let report = audit(&bytes);

    let changes = post_signature_changes(&report.document, signature(&report));
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].object, "60 0 R");
    assert_eq!(changes[0].change, ObjectChangeKind::Redefined);
    assert_eq!(changes[0].role, ObjectRole::Annotation);

    assert_docmdp_silent(&report);
}

#[test]
fn p2_annot_edit_violates() {
    let bytes = builder::signed_docmdp_p2_then_annot_edit();
    let report = audit(&bytes);

    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 1);
    let violation = explanation(&report, &DOCMDP_POLICY_VIOLATION);
    assert!(
        violation.contains("60 0 R redefined (annotation)"),
        "{violation}"
    );
    assert!(violation.contains("require P=3"), "{violation}");
}

#[test]
fn widget_fill_with_new_appearance_stays_silent() {
    let bytes = builder::signed_docmdp_p2_then_widget_fill();
    let report = audit(&bytes);

    // Both the redefined widget and its regenerated appearance stream are
    // form filling.
    let changes = post_signature_changes(&report.document, signature(&report));
    let objects: Vec<&str> = changes
        .iter()
        .map(|change| change.object.as_str())
        .collect();
    assert_eq!(objects, vec!["7 0 R", "71 0 R"]);

    assert_docmdp_silent(&report);
}

#[test]
fn fieldmdp_policy_parses_into_facts() {
    let bytes = builder::signed_fieldmdp_include_then_locked_fill();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert!(signature.docmdp.is_none());
    let policy = signature.fieldmdp.as_ref().unwrap();
    assert_eq!(policy.action, Some(FieldMdpAction::Include));
    assert_eq!(policy.fields, Some(vec!["full-name".to_owned()]));

    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    let json_sig = &json["document"]["signatures"]["value"]["signatures"][0];
    assert!(json_sig["docmdp"].is_null());
    assert_eq!(json_sig["fieldmdp"]["action"], "include");
    assert_eq!(
        json_sig["fieldmdp"]["fields"],
        serde_json::json!(["full-name"])
    );
}

#[test]
fn fieldmdp_locked_fill_violates() {
    let bytes = builder::signed_fieldmdp_include_then_locked_fill();
    let report = audit(&bytes);

    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 1);
    let post = explanation(&report, &POST_SIGNATURE_REVISION);
    assert!(post.contains("7 0 R redefined"), "{post}");
    let violation = explanation(&report, &DOCMDP_POLICY_VIOLATION);
    assert!(violation.contains("full-name"), "{violation}");
    assert!(violation.contains("locked"), "{violation}");
}

#[test]
fn fieldmdp_unlocked_fill_stays_silent() {
    let bytes = builder::signed_fieldmdp_include_then_unlocked_fill();
    let report = audit(&bytes);

    let changes = post_signature_changes(&report.document, signature(&report));
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].object, "7 0 R");

    assert_docmdp_silent(&report);
}

#[test]
fn fieldmdp_exclude_locks_everything_outside_the_list() {
    let bytes = builder::signed_fieldmdp_exclude_then_locked_fill();
    let report = audit(&bytes);

    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 1);
    let violation = explanation(&report, &DOCMDP_POLICY_VIOLATION);
    assert!(violation.contains("full-name"), "{violation}");
    assert!(violation.contains("locked"), "{violation}");
}

#[test]
fn fieldmdp_all_locks_every_fill() {
    let bytes = builder::signed_fieldmdp_all_then_fill();
    let report = audit(&bytes);

    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 1);
}

#[test]
fn unestablished_docmdp_permission_reports_without_claiming() {
    let bytes = builder::signed_docmdp_p9_then_content_edit();
    let report = audit(&bytes);

    let signature = signature(&report);
    let policy = signature.docmdp.as_ref().unwrap();
    assert_eq!(policy.p, Some(9));
    assert_eq!(policy.permission, None);

    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 0);
    let post = explanation(&report, &POST_SIGNATURE_REVISION);
    assert!(post.contains("11 0 R redefined (contents)"), "{post}");
    assert!(post.contains("unestablished"), "{post}");
}

#[test]
fn unestablished_fieldmdp_lock_reports_without_claiming() {
    let bytes = builder::signed_fieldmdp_no_action_then_fill();
    let report = audit(&bytes);

    let signature = signature(&report);
    assert_eq!(signature.fieldmdp.as_ref().unwrap().action, None);

    assert_eq!(count_code(&report, &POST_SIGNATURE_REVISION), 1);
    assert_eq!(count_code(&report, &DOCMDP_POLICY_VIOLATION), 0);
}

#[test]
fn approval_signature_with_later_revision_stays_silent() {
    // No DocMDP, no FieldMDP: later revisions are the ordinary approval
    // workflow, and the policy detector has no policy to evaluate.
    let bytes = builder::signed_then_updated();
    let report = audit(&bytes);

    assert!(signature(&report).docmdp.is_none());
    let changes = post_signature_changes(&report.document, signature(&report));
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].object, "50 0 R");

    assert_docmdp_silent(&report);
}
