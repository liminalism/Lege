//! Object-change classification tests: per-revision member objects as
//! Added/Redefined/Freed/RepeatedIdentically, with hashes, roles, and the
//! reverse object-to-page map.
//!
//! Each test audits bytes built below or by `pdf-test-support` and pins the
//! classification of known members. The first save of a file adds
//! everything; an incremental update redefines what it rewrites; a save
//! may also free, or merely re-state, an entry.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]

use pdf_forensics::{
    AuditReport, LogicalRevisionFacts, ObjectChangeFacts, ObjectChangeKind, ObjectLocationFacts,
    ObjectRole, RevisionHistoryFacts, audit_bytes,
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

#[test]
fn single_save_adds_every_live_member() {
    let bytes = builder::multipage_classic(1);
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    let revision = &history.revisions[0];
    // Five body objects plus the object-0 free-list head.
    assert_eq!(revision.changes.len(), 6);
    // Ascending object-number order.
    let numbers: Vec<&str> = revision
        .changes
        .iter()
        .map(|change| change.object.as_str())
        .collect();
    assert_eq!(
        numbers,
        vec!["0 65535 R", "1 0 R", "2 0 R", "10 0 R", "11 0 R", "101 0 R"]
    );

    let head = change(revision, "0 65535 R");
    assert_eq!(head.change, ObjectChangeKind::Freed);
    assert_eq!(head.location, ObjectLocationFacts::Free);
    assert_eq!(head.raw_sha256, None);
    assert_eq!(head.semantic_sha256, None);
    assert_eq!(head.role, ObjectRole::Unknown);
    assert!(head.pages.is_empty());

    let page = change(revision, "10 0 R");
    assert_eq!(page.change, ObjectChangeKind::Added);
    assert!(matches!(page.location, ObjectLocationFacts::Offset(_)));
    assert!(page.raw_sha256.is_some());
    assert!(page.semantic_sha256.is_some());
    assert_eq!(page.role, ObjectRole::Page);
    assert_eq!(page.pages, vec![0]);

    let contents = change(revision, "11 0 R");
    assert_eq!(contents.change, ObjectChangeKind::Added);
    assert_eq!(contents.role, ObjectRole::Contents);
    assert_eq!(contents.pages, vec![0]);

    let font = change(revision, "101 0 R");
    assert_eq!(font.change, ObjectChangeKind::Added);
    assert_eq!(font.role, ObjectRole::Font);
    assert_eq!(font.pages, vec![0]);

    let catalog = change(revision, "1 0 R");
    assert_eq!(catalog.change, ObjectChangeKind::Added);
    assert_eq!(catalog.role, ObjectRole::Other);
    assert!(catalog.pages.is_empty());

    // The reverse map mirrors the per-change pages as a single lookup.
    assert_eq!(
        history.object_pages.get("10 0 R"),
        Some(&vec![0]),
        "page dictionary serves page 0"
    );
    assert_eq!(history.object_pages.get("11 0 R"), Some(&vec![0]));
    assert_eq!(history.object_pages.get("101 0 R"), Some(&vec![0]));
    assert_eq!(history.object_pages.get("1 0 R"), Some(&vec![]));

    // The JSON spelling of the changes section is the consumer contract.
    let json: serde_json::Value = serde_json::to_value(&report).unwrap();
    let first = &json["document"]["revisions"]["value"]["revisions"][0]["changes"][3];
    assert_eq!(first["object"], "10 0 R");
    assert_eq!(first["change"], "added");
    assert_eq!(first["role"], "page");
    assert_eq!(first["pages"], serde_json::json!([0]));
    assert!(first["location"].get("offset").is_some());
}

#[test]
fn incremental_update_redefines_rewritten_content() {
    let bytes = builder::phase1_exit_fixture(1);
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);
    let (base, update) = (&history.revisions[0], &history.revisions[1]);

    // The base save adds its four body objects (plus the free head); the
    // font does not exist yet.
    assert_eq!(base.changes.len(), 5);
    assert_eq!(
        change(base, "11 0 R").change,
        ObjectChangeKind::Added,
        "base content is new"
    );
    assert!(
        base.changes.iter().all(|change| change.object != "101 0 R"),
        "the font arrives only with the update"
    );

    // The update redefines the rewritten content stream, adds the object
    // stream and its members, and adds its own xref stream — nothing is
    // merely re-stated.
    assert_eq!(update.changes.len(), 5);
    assert!(
        update
            .changes
            .iter()
            .all(|change| change.change != ObjectChangeKind::RepeatedIdentically),
        "a minimal update re-states nothing"
    );
    let content = change(update, "11 0 R");
    assert_eq!(content.change, ObjectChangeKind::Redefined);
    assert_eq!(content.role, ObjectRole::Contents);
    assert_eq!(content.pages, vec![0]);

    // The redefinition changed the bytes, not just the location.
    let base_content = change(base, "11 0 R");
    assert_ne!(base_content.raw_sha256, content.raw_sha256);
    assert_ne!(base_content.semantic_sha256, content.semantic_sha256);

    // The compressed font hashes through its container: stored bytes for
    // the raw hash, the parsed member for the semantic one.
    let font = change(update, "101 0 R");
    assert_eq!(font.change, ObjectChangeKind::Added);
    assert_eq!(
        font.location,
        ObjectLocationFacts::InObjectStream {
            container: 100,
            index: 0
        }
    );
    assert!(font.raw_sha256.is_some());
    assert!(font.semantic_sha256.is_some());
    assert_eq!(font.role, ObjectRole::Font);
    assert_eq!(font.pages, vec![0]);

    let container = change(update, "100 0 R");
    assert_eq!(container.change, ObjectChangeKind::Added);
    assert_eq!(container.role, ObjectRole::Other);
    assert!(container.raw_sha256.is_some());
    assert!(container.semantic_sha256.is_some());

    // The reverse map is keyed by the latest view: the content stream's
    // single entry serves page 0 in both revisions' records.
    assert_eq!(history.object_pages.get("11 0 R"), Some(&vec![0]));
    assert_eq!(base_content.pages, vec![0]);
}

#[test]
fn hybrid_save_merges_both_sections_members() {
    let bytes = builder::hybrid_fixture(1);
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    let revision = &history.revisions[0];
    // Classic table members plus the auxiliary stream's: free head,
    // catalog, pages, page, content, container, font member, xref stream.
    assert_eq!(revision.changes.len(), 8);

    let content = change(revision, "11 0 R");
    assert_eq!(content.change, ObjectChangeKind::Added);
    assert!(matches!(content.location, ObjectLocationFacts::Offset(_)));

    let font = change(revision, "101 0 R");
    assert_eq!(font.change, ObjectChangeKind::Added);
    assert!(matches!(
        font.location,
        ObjectLocationFacts::InObjectStream { .. }
    ));
    assert!(font.raw_sha256.is_some());
    assert!(font.semantic_sha256.is_some());
    assert_eq!(font.role, ObjectRole::Font);
}

/// The header-relative offset of the `N 0 obj` header in `bytes`.
fn object_offset(bytes: &[u8], number: u32) -> u64 {
    let needle = format!("{number} 0 obj");
    bytes
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())
        .unwrap() as u64
}

/// The value of the last `startxref` in `bytes`.
fn last_startxref(bytes: &[u8]) -> u64 {
    let at = bytes
        .windows(9)
        .rposition(|window| window == b"startxref")
        .unwrap();
    let tail = &bytes[at + 9..];
    let digits = tail.iter().position(|byte| byte.is_ascii_digit()).unwrap();
    let len = tail[digits..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    std::str::from_utf8(&tail[digits..digits + len])
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn restated_entry_classifies_as_repeated_identically() {
    let base = builder::multipage_classic(1);
    let prev = last_startxref(&base);
    let catalog_at = object_offset(&base, 1);

    // An incremental update that adds one dummy object and re-states the
    // catalog entry at its unchanged offset.
    let mut bytes = base;
    let dummy_at = bytes.len() as u64;
    bytes.extend_from_slice(b"500 0 obj\n<</Kind(dummy)>>\nendobj\n");
    let table_at = bytes.len() as u64;
    let table = format!(
        "xref\n1 1\n{catalog_at:010} 00000 n\r\n\
         500 1\n{dummy_at:010} 00000 n\r\n\
         trailer\n<</Size 501/Root 1 0 R/Prev {prev}>>\n\
         startxref\n{table_at}\n%%EOF\n"
    );
    bytes.extend_from_slice(table.as_bytes());

    let report = audit(&bytes);
    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);
    let update = &history.revisions[1];
    assert_eq!(update.changes.len(), 2);

    let dummy = change(update, "500 0 R");
    assert_eq!(dummy.change, ObjectChangeKind::Added);
    assert_eq!(dummy.role, ObjectRole::Other);
    assert!(dummy.raw_sha256.is_some());
    assert!(dummy.semantic_sha256.is_some());

    let catalog = change(update, "1 0 R");
    assert_eq!(catalog.change, ObjectChangeKind::RepeatedIdentically);
    assert_eq!(catalog.role, ObjectRole::Other);
    // Same bytes, so both hashes agree with the base save's.
    let base_catalog = change(&history.revisions[0], "1 0 R");
    assert_eq!(base_catalog.raw_sha256, catalog.raw_sha256);
    assert_eq!(base_catalog.semantic_sha256, catalog.semantic_sha256);
}

#[test]
fn freed_entry_classifies_as_freed() {
    let base = builder::multipage_classic(1);
    let prev = last_startxref(&base);

    // An incremental update that frees the font object.
    let mut bytes = base;
    let table_at = bytes.len() as u64;
    let table = format!(
        "xref\n101 1\n0000000000 00001 f\r\n\
         trailer\n<</Size 102/Root 1 0 R/Prev {prev}>>\n\
         startxref\n{table_at}\n%%EOF\n"
    );
    bytes.extend_from_slice(table.as_bytes());

    let report = audit(&bytes);
    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);

    let base_font = change(&history.revisions[0], "101 0 R");
    assert_eq!(base_font.change, ObjectChangeKind::Added);

    let update = &history.revisions[1];
    assert_eq!(update.changes.len(), 1);
    let freed = change(update, "101 1 R");
    assert_eq!(freed.change, ObjectChangeKind::Freed);
    assert_eq!(freed.location, ObjectLocationFacts::Free);
    assert_eq!(freed.raw_sha256, None);
    assert_eq!(freed.semantic_sha256, None);
    // Roles are latest-view and by number: the page still references the
    // freed number as its font, so the record says what was freed.
    assert_eq!(freed.role, ObjectRole::Font);
    assert_eq!(freed.pages, vec![0]);
}

#[test]
fn structure_only_run_keeps_structural_classification() {
    // AES-256 declines to open, but the section entries are structural
    // facts a structure-only run still reaches.
    let bytes = builder::aes256_encrypted_fixture();
    let report = audit(&bytes);

    assert!(matches!(
        report.document.parse.state,
        pdf_forensics::OpenState::Failed { .. }
    ));
    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    let revision = &history.revisions[0];
    assert!(!revision.changes.is_empty());

    // Classification, raw spans, and non-stream semantics survive:
    // every live member is added, uncompressed spans parse without
    // decryption, and this fixture's members are all plain dictionaries,
    // whose canonical form needs no stream decode.
    for member in &revision.changes {
        match member.location {
            ObjectLocationFacts::Free => {
                assert_eq!(member.change, ObjectChangeKind::Freed);
                assert_eq!(member.raw_sha256, None);
                assert_eq!(member.semantic_sha256, None);
            }
            ObjectLocationFacts::Offset(_) => {
                assert_eq!(member.change, ObjectChangeKind::Added);
                assert!(member.raw_sha256.is_some(), "{}", member.object);
                assert!(member.semantic_sha256.is_some(), "{}", member.object);
            }
            ObjectLocationFacts::InObjectStream { .. } => {
                assert_eq!(member.change, ObjectChangeKind::Added);
            }
        }
        // Roles and page associations need a resolved document, which
        // this run never had.
        assert_eq!(member.role, ObjectRole::Unknown, "{}", member.object);
        assert!(member.pages.is_empty());
    }

    // The reverse map still lists every classified object, with no pages.
    assert_eq!(history.object_pages.len(), revision.changes.len());
    assert!(
        history.object_pages.values().all(Vec::is_empty),
        "no page associations without a resolved document"
    );
}
