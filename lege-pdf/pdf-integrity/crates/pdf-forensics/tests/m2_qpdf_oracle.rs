//! Differential oracle: per-revision object inventory against `qpdf --json`.
//!
//! For every fixture in the M2 corpus — classic, cross-reference stream,
//! hybrid-reference, object-stream, linearized, and the incremental
//! variants — each logical revision's byte prefix is inventoried twice:
//! once through [`pdf_forensics::open_revision`] (the ordinary open path
//! over the truncated prefix) and once through the `qpdf` CLI's JSON view
//! of the same bytes. The live-object sets (`N G R`) must agree exactly.
//!
//! Our side reads the replayed snapshot's merged xref map
//! (`live_entries`, which skips free entries); the qpdf side reads the
//! `obj:O G R` keys of the objects dictionary — the second element of the
//! top-level `qpdf` array in JSON v2 (qpdf 11+). The trailer key is not an
//! object in either view and is excluded on both sides.
//!
//! # Prerequisite: the `qpdf` CLI on `PATH`
//!
//! These tests shell out to `qpdf`, resolved through `PATH` only. There is
//! deliberately no Rust dependency on qpdf and no vendored binary: the
//! released tool stays fully Rust-native and the oracle is test-only. If
//! `qpdf` is absent the tests **fail** with an actionable message instead
//! of skipping, so a missing oracle can never look like a passing suite.
//! Install it with:
//!
//! ```sh
//! apt install qpdf
//! ```
//!
//! Every test is `#[ignore]` because of that external dependency; run them
//! with:
//!
//! ```sh
//! cargo test -p pdf-forensics --test m2_qpdf_oracle -- --ignored
//! ```
//!
//! from `lege-pdf/pdf-integrity`. Developed against qpdf 12.x JSON v2.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use pdf_forensics::{AuditReport, LogicalRevisionFacts, audit_bytes, open_revision};
use pdf_test_support::builder;

/// Audit `bytes` and require the report to satisfy the schema's own rules.
fn audit(bytes: &[u8]) -> AuditReport {
    let report = audit_bytes(bytes, None);
    report.validate().unwrap();
    report
}

/// Fail unless the `qpdf` CLI resolves through `PATH` and runs.
///
/// This is the suite's choke point: a missing qpdf is a loud failure with
/// install instructions, never a silent skip.
fn require_qpdf() {
    match Command::new("qpdf").arg("--version").output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => panic!(
            "`qpdf --version` failed (status {}): {}; the oracle needs a working qpdf CLI on PATH",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(err) => panic!(
            "the `qpdf` CLI was not found on PATH ({err}); this oracle shells out to qpdf \
             (resolved through PATH only, no Rust dependency) and never skips: \
             install it with `apt install qpdf`"
        ),
    }
}

/// A scratch path unique to this test process, case, and revision.
fn work_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pdf-forensics-qpdf-oracle-{}-{name}",
        std::process::id()
    ))
}

/// The live-object inventory (`N G R`) our replay sees at `revision`: the
/// merged xref map of the prefix opened through [`open_revision`].
fn our_inventory(bytes: &[u8], revision: &LogicalRevisionFacts) -> BTreeSet<String> {
    let snapshot = open_revision(bytes, revision, None)
        .unwrap_or_else(|err| panic!("revision {} did not replay: {err}", revision.index));
    snapshot
        .structure()
        .xref
        .live_entries()
        .map(|entry| entry.id.to_string())
        .collect()
}

/// The live-object inventory qpdf reports for `prefix`: the `obj:O G R`
/// keys of the JSON v2 objects dictionary, with the `obj:` prefix stripped
/// so they read as `N G R` like our side.
fn qpdf_objects(case: &str, index: u32, prefix: &[u8]) -> BTreeSet<String> {
    let path = work_file(&format!("{case}-rev{index}.pdf"));
    std::fs::write(&path, prefix).unwrap();
    let output = Command::new("qpdf")
        .arg("--json")
        .arg("--json-key=qpdf")
        .arg(&path)
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "case {case} revision {index}: could not run `qpdf` from PATH ({err}); \
                 install it with `apt install qpdf`"
            )
        });
    let _ = std::fs::remove_file(&path);
    // Parse stdout whatever the exit status: a warning (nonzero status)
    // with valid JSON still carries the inventory, while a genuine
    // failure carries no JSON and fails below with qpdf's stderr.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|err| {
        panic!(
            "case {case} revision {index}: `qpdf --json` output did not parse ({err}); \
             status: {}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let objects = json
        .get("qpdf")
        .and_then(|qpdf| qpdf.get(1))
        .and_then(|dict| dict.as_object())
        .unwrap_or_else(|| {
            panic!("case {case} revision {index}: unexpected `qpdf --json` shape: {json}")
        });
    objects
        .keys()
        .filter_map(|key| key.strip_prefix("obj:").map(str::to_owned))
        .collect()
}

/// Every revision of `bytes` must inventory identically on both sides.
///
/// A revision without an established end has no prefix to feed qpdf and
/// fails here rather than silently dropping out of the comparison.
fn assert_oracle_agrees(case: &str, bytes: &[u8]) {
    require_qpdf();
    let report = audit(bytes);
    let history = report.document.revisions.value().unwrap_or_else(|| {
        panic!("case {case}: revision history unavailable; the oracle needs grouped revisions")
    });
    assert!(
        !history.revisions.is_empty(),
        "case {case}: no revisions to compare"
    );
    for revision in &history.revisions {
        let end = revision.end.unwrap_or_else(|| {
            panic!(
                "case {case}: revision {} has no established end; the oracle needs a prefix",
                revision.index
            )
        });
        let prefix = &bytes[..end as usize];
        let ours = our_inventory(bytes, revision);
        let theirs = qpdf_objects(case, revision.index, prefix);
        let only_ours: Vec<&String> = ours.difference(&theirs).collect();
        let only_qpdf: Vec<&String> = theirs.difference(&ours).collect();
        assert!(
            only_ours.is_empty() && only_qpdf.is_empty(),
            "case {case} revision {}: object inventory differs ({} ours, {} qpdf): \
             only in pdf-forensics: {only_ours:?}; only in qpdf: {only_qpdf:?}",
            revision.index,
            ours.len(),
            theirs.len()
        );
    }
}

#[test]
#[ignore = "requires the qpdf CLI on PATH (apt install qpdf)"]
fn classic_inventory_matches_qpdf() {
    assert_oracle_agrees("classic", &builder::multipage_classic(2));
}

#[test]
#[ignore = "requires the qpdf CLI on PATH (apt install qpdf)"]
fn xref_stream_inventory_matches_qpdf() {
    assert_oracle_agrees("xref-stream", &builder::xref_stream_fixture(1));
}

#[test]
#[ignore = "requires the qpdf CLI on PATH (apt install qpdf)"]
fn hybrid_inventory_matches_qpdf() {
    assert_oracle_agrees("hybrid", &builder::hybrid_fixture(1));
}

#[test]
#[ignore = "requires the qpdf CLI on PATH (apt install qpdf)"]
fn linearized_inventory_matches_qpdf() {
    assert_oracle_agrees("linearized", &builder::linearized());
}

#[test]
#[ignore = "requires the qpdf CLI on PATH (apt install qpdf)"]
fn incremental_redefined_content_matches_qpdf() {
    assert_oracle_agrees("inc-redefined", &builder::incremental_redefined_content());
}

#[test]
#[ignore = "requires the qpdf CLI on PATH (apt install qpdf)"]
fn incremental_added_objects_matches_qpdf() {
    assert_oracle_agrees("inc-added", &builder::incremental_added_objects());
}

#[test]
#[ignore = "requires the qpdf CLI on PATH (apt install qpdf)"]
fn incremental_xref_stream_matches_qpdf() {
    assert_oracle_agrees("inc-xref-stream", &builder::incremental_xref_stream());
}

#[test]
#[ignore = "requires the qpdf CLI on PATH (apt install qpdf)"]
fn incremental_hybrid_matches_qpdf() {
    assert_oracle_agrees("inc-hybrid", &builder::incremental_hybrid());
}
