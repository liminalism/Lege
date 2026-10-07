//! M2 historical-replay tests: revision prefixes reopen as documents.
//!
//! Each test audits an edited (two-revision) fixture and replays one
//! revision through [`pdf_forensics::open_revision`] — the ordinary open
//! path over a [`PrefixSource`] truncated at the revision's established
//! end. Revision 0 opens, compiles through the same path a render uses,
//! and carries the pre-edit text; the latest revision carries the
//! post-edit text; a revision with no established end refuses to replay
//! rather than opening a guessed prefix.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed unwrap is a failed test"
)]

use std::sync::Arc;

use pdf_document::{DocumentLimits, DocumentSnapshot, PageIndex, ParseContext};
use pdf_forensics::{AuditReport, ReplayError, RevisionHistoryFacts, audit_bytes, open_revision};
use pdf_object::PdfObject;
use pdf_source::{OwnedBytesSource, PdfSource, PrefixSource};
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

/// The decoded content-stream bytes of one page of a replayed snapshot:
/// the page's text-showing source as that revision stored it.
fn page_content_bytes(snapshot: &DocumentSnapshot, index: u32) -> Vec<u8> {
    let mut ctx = ParseContext::new();
    let page = snapshot.page(PageIndex(index)).unwrap();
    let contents = page.contents.clone().unwrap();
    let PdfObject::Reference(id) = contents else {
        panic!("fixture pages carry a single indirect content stream");
    };
    let resolved = snapshot.objects().resolve(snapshot, id, &mut ctx).unwrap();
    let PdfObject::Stream(stream) = resolved.as_ref() else {
        panic!("page content resolves to a stream");
    };
    snapshot.decode_stream_data(stream, &mut ctx).unwrap()
}

#[test]
fn revision_zero_of_edited_fixture_replays_pre_edit_content() {
    let bytes = builder::incremental_redefined_content();
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);

    // Revision 0 opens as a document in its own right.
    let first = &history.revisions[0];
    let snapshot = open_revision(&bytes, first, None).unwrap();
    assert_eq!(snapshot.page_count(), 1);

    // It compiles through the same path a render uses.
    let full: Arc<dyn PdfSource> = Arc::new(OwnedBytesSource::new(bytes.clone()));
    let prefix: Arc<dyn PdfSource> = Arc::new(PrefixSource::new(full, first.end.unwrap()));
    let examined = pdf_read::examine(prefix, DocumentLimits::default());
    assert_eq!(examined.pages.len(), 1);
    assert!(
        matches!(
            examined.pages[0].compile,
            pdf_read::CompileStatus::Ok { .. }
        ),
        "revision-0 page compiles cleanly: {:?}",
        examined.pages[0].compile
    );

    // And its extracted content is the pre-edit text.
    let content = page_content_bytes(&snapshot, 0);
    let text = String::from_utf8_lossy(&content);
    assert!(text.contains("(Page 0)"), "pre-edit text: {text}");
    assert!(!text.contains("Updated"), "no post-edit text: {text}");
}

#[test]
fn latest_revision_replays_post_edit_content() {
    let bytes = builder::incremental_redefined_content();
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 2);
    let latest = &history.revisions[1];
    let snapshot = open_revision(&bytes, latest, None).unwrap();

    assert_eq!(snapshot.page_count(), 1);
    let content = page_content_bytes(&snapshot, 0);
    let text = String::from_utf8_lossy(&content);
    assert!(text.contains("(Updated page 0)"), "post-edit text: {text}");
}

#[test]
fn replay_without_an_established_end_is_refused() {
    // The tail is gone, so the revision's end is unestablished: replay
    // refuses rather than opening a guessed prefix.
    let bytes = builder::without_startxref(builder::multipage_classic(1));
    let report = audit(&bytes);

    let history = history(&report);
    assert_eq!(history.revisions.len(), 1);
    let revision = &history.revisions[0];
    assert_eq!(revision.end, None);
    assert_eq!(
        open_revision(&bytes, revision, None).unwrap_err(),
        ReplayError::NoEstablishedEnd { index: 0 }
    );
}
