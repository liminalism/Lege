//! Acceptance test for `@audit.milestone.m1-factual-audit#encrypted-mode-labelled`.
//!
//! The rule this guards: an encrypted document is audited rather than refused,
//! and an audit that could not read the document must not resemble one that
//! read it and found nothing.
//!
//! Both halves are covered here: the structure-only audit of a document that
//! does not open (a declined scheme, or a supported scheme without its
//! password), and the password upgrade — the same RC4 V2/R3 file audited
//! without a password (structure-only, decryption-gated sections unavailable)
//! and again with the correct non-empty user password (full, pages available).
#![allow(clippy::unwrap_used, reason = "a failed unwrap is a failed test")]

use pdf_forensics::{
    AnalysisMode, Availability, Disposition, OpenState, UnavailableReason, audit_bytes,
};
use pdf_test_support::builder;

#[test]
fn permissions_only_encryption_audits_in_full() {
    // RC4 V2/R3 whose /U validates for the empty user password: encryption is
    // real and reported, but it obstructs nothing.
    let report = audit_bytes(&builder::encrypted_fixture(), None);
    report.validate().unwrap();

    assert_eq!(report.analysis_mode, AnalysisMode::Full);
    assert_eq!(report.document.parse.state, OpenState::Clean);

    let encryption = report
        .document
        .encryption
        .value()
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(encryption.version, 2);
    assert_eq!(encryption.revision, 3);
    assert!(
        encryption.user_password_empty,
        "the empty user password validates, which is why this audits in full"
    );
    assert!(report.document.pages.is_available());
}

#[test]
fn an_unopenable_encrypted_document_is_still_audited() {
    // AES-256 V5/R6, which the standard handler declines. The document does not
    // open, yet the structural retry still reaches the /Encrypt dictionary —
    // /Encrypt is itself never encrypted.
    let report = audit_bytes(&builder::aes256_encrypted_fixture(), None);
    report.validate().unwrap();

    assert_eq!(report.analysis_mode, AnalysisMode::StructureOnly);
    assert!(matches!(
        report.document.parse.state,
        OpenState::Failed { .. }
    ));

    // Encryption is a determined fact even though nothing else is.
    let encryption = report
        .document
        .encryption
        .value()
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(encryption.version, 5);
    assert_eq!(encryption.revision, 6);

    // Everything gated behind decryption is an explicit gap, attributed to the
    // encryption rather than to damage.
    assert!(matches!(
        report.document.pages,
        Availability::Unavailable {
            reason: UnavailableReason::Encrypted,
            ..
        }
    ));
    assert!(matches!(
        report.document.features,
        Availability::Unavailable {
            reason: UnavailableReason::Encrypted,
            ..
        }
    ));
    assert!(matches!(
        report.document.annotations,
        Availability::Unavailable {
            reason: UnavailableReason::Encrypted,
            ..
        }
    ));
    assert!(matches!(
        report.document.fonts,
        Availability::Unavailable {
            reason: UnavailableReason::Encrypted,
            ..
        }
    ));
    assert!(matches!(
        report.document.images,
        Availability::Unavailable {
            reason: UnavailableReason::Encrypted,
            ..
        }
    ));

    // And it must not read as a clean bill of health.
    assert_eq!(report.disposition, Disposition::Inconclusive);
}

#[test]
fn a_structure_only_run_emits_no_finding_it_could_not_support() {
    let report = audit_bytes(&builder::aes256_encrypted_fixture(), None);
    assert_eq!(report.analysis_mode, AnalysisMode::StructureOnly);

    // validate() enforces this mechanically for whatever findings exist; assert
    // it directly so the guarantee survives the arrival of a findings engine.
    for finding in &report.findings {
        assert!(
            report.analysis_mode.reaches(finding.requires_mode),
            "finding {} rests on evidence a structure-only run cannot reach",
            finding.code
        );
    }
    report.validate().unwrap();
}

#[test]
fn supplying_the_user_password_upgrades_the_same_file_to_full() {
    // RC4 V2/R3 whose /U validates for a non-empty user password: without it
    // the file does not open; with it the same bytes audit in full.
    let password = "correct-horse-battery";
    let bytes = builder::encrypted_with_user_password(password);

    let without = audit_bytes(&bytes, None);
    without.validate().unwrap();
    assert_eq!(without.analysis_mode, AnalysisMode::StructureOnly);
    assert!(matches!(
        without.document.parse.state,
        OpenState::Failed { .. }
    ));
    let encryption = without
        .document
        .encryption
        .value()
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(encryption.version, 2);
    assert_eq!(encryption.revision, 3);
    assert!(
        !encryption.user_password_empty,
        "a non-empty user password is required, which is why this is structure-only"
    );
    assert!(matches!(
        without.document.pages,
        Availability::Unavailable {
            reason: UnavailableReason::Encrypted,
            ..
        }
    ));
    assert_eq!(without.disposition, Disposition::Inconclusive);

    let with = audit_bytes(&bytes, Some(password));
    with.validate().unwrap();
    assert_eq!(with.analysis_mode, AnalysisMode::Full);
    assert_eq!(with.document.parse.state, OpenState::Clean);
    assert!(with.document.pages.is_available());
    assert_eq!(with.document.pages.value().unwrap().len(), 1);
}
