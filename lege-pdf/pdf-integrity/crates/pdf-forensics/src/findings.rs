//! The findings engine: every detector's findings, one verdict, and the PNG
//! evidence behind them.
//!
//! [`collect_evidence`] runs the six detectors — signature byte ranges, CMS
//! digests, post-signature DocMDP/FieldMDP policy, revision text diffs,
//! annotation overlays, and flattened occlusion — over a full audit and keeps
//! their outcomes (findings plus the renders behind them) in one
//! [`AuditEvidence`] bundle. [`decide_disposition`] turns the collected
//! findings into the document-level verdict.
//! [`write_evidence_pngs`] exports the bundle's renders as deterministic PNG
//! files for `--evidence-dir`.
//!
//! Detectors run only in [`AnalysisMode::Full`]. A structure-only run cannot
//! reach decrypted stream contents, so every detector finding — each needing
//! [`AnalysisMode::Full`] — would exceed its mode; the bundle stays empty and
//! the verdict follows from the pending inventory instead. Per-page skips
//! stay on the bundle rather than entering the report: the report's parse
//! assessment and section availability already mark how far the run reached.

use std::collections::BTreeSet;
use std::path::Path;

use crate::byterange::check_signature_byte_ranges;
use crate::digest::{SIGNATURE_DIGEST_MISMATCH, check_signature_digests};
use crate::docmdp::check_post_signature_policies;
use crate::document::{DocumentFacts, PendingSection};
use crate::evidence::{EvidenceError, EvidenceRender, highlight_changed_regions, write_png};
use crate::occlusion::{OcclusionOptions, OcclusionOutcome, detect_flattened_occlusion};
use crate::overlays::{OverlayOptions, OverlayOutcome, detect_annotation_overlays};
use crate::report::{AnalysisMode, Disposition, Finding};
use crate::revision_diff::{RevisionTextDiffOptions, RevisionTextDiffOutcome, diff_revision_text};

/// Every detector outcome behind one audit: findings plus the renders the
/// PNG evidence is exported from.
///
/// Outcomes keep their detectors' own order — byte ranges, digests, and
/// post-signature policy in signature order, revision pairs in pair order,
/// overlays in page order, occlusions in page then painter order — so
/// [`AuditEvidence::findings`] and [`write_evidence_pngs`] are deterministic.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvidence {
    /// Signature `/ByteRange` violations, in signature order. Structural
    /// facts with no renders behind them.
    pub byte_ranges: Vec<Finding>,
    /// CMS digest mismatches, in signature order. Cryptographic facts with
    /// no renders behind them.
    pub digests: Vec<Finding>,
    /// Post-signature DocMDP/FieldMDP policy findings, in signature order:
    /// at most one [`POST_SIGNATURE_REVISION`](crate::docmdp::POST_SIGNATURE_REVISION)
    /// plus one [`DOCMDP_POLICY_VIOLATION`](crate::docmdp::DOCMDP_POLICY_VIOLATION)
    /// per signature. Structural and derived facts with no renders behind
    /// them.
    pub docmdp: Vec<Finding>,
    /// Semantic text comparison of every consecutive revision pair.
    pub revision_text: RevisionTextDiffOutcome,
    /// Annotations painting over existing page text.
    pub overlays: OverlayOutcome,
    /// Page-content paint covering earlier text.
    pub occlusions: OcclusionOutcome,
}

impl AuditEvidence {
    /// An empty bundle: what a structure-only run collects.
    fn empty() -> Self {
        Self {
            byte_ranges: Vec::new(),
            digests: Vec::new(),
            docmdp: Vec::new(),
            revision_text: RevisionTextDiffOutcome {
                changes: Vec::new(),
                skipped: Vec::new(),
            },
            overlays: OverlayOutcome {
                overlays: Vec::new(),
                skipped: Vec::new(),
            },
            occlusions: OcclusionOutcome {
                occlusions: Vec::new(),
                skipped: Vec::new(),
            },
        }
    }

    /// Every finding in the bundle, in canonical order: byte ranges, digests,
    /// and post-signature policy first, then revision text changes, annotation
    /// overlays, and flattened occlusions.
    #[must_use]
    pub fn findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        findings.extend(self.byte_ranges.iter().cloned());
        findings.extend(self.digests.iter().cloned());
        findings.extend(self.docmdp.iter().cloned());
        findings.extend(
            self.revision_text
                .changes
                .iter()
                .map(|change| change.finding.clone()),
        );
        findings.extend(
            self.overlays
                .overlays
                .iter()
                .map(|overlay| overlay.finding.clone()),
        );
        findings.extend(
            self.occlusions
                .occlusions
                .iter()
                .map(|occlusion| occlusion.finding.clone()),
        );
        findings
    }
}

/// Run every detector over a full audit and bundle their outcomes.
///
/// `facts` and `mode` come from the same intake run over `bytes`; `password`
/// is tried exactly as the audit tried it. In [`AnalysisMode::StructureOnly`]
/// no detector runs and the bundle is empty — every detector finding needs
/// [`AnalysisMode::Full`] evidence, so emitting any would break the schema's
/// mode rule.
///
/// Never fails and never panics: each detector already converts its own
/// failures into skip entries.
#[must_use]
pub fn collect_evidence(
    bytes: &[u8],
    password: Option<&str>,
    facts: &DocumentFacts,
    mode: AnalysisMode,
) -> AuditEvidence {
    if mode != AnalysisMode::Full {
        return AuditEvidence::empty();
    }
    let revision_text = match facts.revisions.value() {
        Some(history) => diff_revision_text(
            bytes,
            history,
            &facts.parse,
            password,
            &RevisionTextDiffOptions::default(),
        ),
        None => RevisionTextDiffOutcome {
            changes: Vec::new(),
            skipped: Vec::new(),
        },
    };
    let overlays =
        detect_annotation_overlays(bytes, &facts.parse, password, &OverlayOptions::default());
    let occlusions =
        detect_flattened_occlusion(bytes, &facts.parse, password, &OcclusionOptions::default());
    let byte_ranges = check_signature_byte_ranges(facts);
    let digests = check_signature_digests(facts);
    let docmdp = check_post_signature_policies(bytes, password, facts);
    AuditEvidence {
        byte_ranges,
        digests,
        docmdp,
        revision_text,
        overlays,
        occlusions,
    }
}

/// Decide the document-level verdict from the collected findings.
///
/// A CMS digest mismatch fails on the document's own terms, so it reports
/// [`Disposition::IntegrityFailure`]; any other finding recommends review.
/// With none, an outstanding [`PendingSection`] means the audit did not finish
/// looking and the honest answer is inconclusive. Only a complete, empty audit
/// — nothing pending, nothing found — reports that no review indicator was
/// found.
///
/// The policy keeps [`AuditReport::validate`](crate::report::AuditReport::validate)
/// green by construction: a problem verdict always has findings behind it,
/// and the all-clear requires an empty pending list.
#[must_use]
pub fn decide_disposition(findings: &[Finding], pending: &[PendingSection]) -> Disposition {
    if findings.is_empty() {
        if pending.is_empty() {
            Disposition::NoReviewIndicatorFound
        } else {
            Disposition::Inconclusive
        }
    } else if findings
        .iter()
        .any(|finding| finding.code == SIGNATURE_DIGEST_MISMATCH)
    {
        Disposition::IntegrityFailure
    } else {
        Disposition::ReviewRecommended
    }
}

/// Export the bundle's renders as deterministic PNGs into `dir`.
///
/// Creates `dir` (with parents) when needed. One file per render, with
/// fixed encoder settings, so the same document always yields the same
/// bytes under the same names:
///
/// * `rev{prev}-to-rev{rev}-page-{page}-before.png` and `-after.png` for
///   every revision text change, plus `-diff.png` — the after render with
///   the changed regions boxed — unless the page geometry itself changed.
/// * `page-{page}-annot-off.png`, `-on.png`, and `-diff.png` for every page
///   carrying a covering annotation: the page without appearances, with
///   them, and the boxed difference. One triple per page however many
///   annotations it carries, since the renders are per-page.
/// * `page-{page}-content.png` for every page carrying flattened
///   occlusion: the page render the cover was measured against, again one
///   per page.
///
/// Returns the written file names (not paths), sorted, for the terminal
/// report to list. An empty bundle writes nothing and returns an empty
/// list — still creating the directory, so the flag's effect is
/// predictable.
///
/// # Errors
///
/// Returns [`EvidenceError::PngFailed`] when the directory cannot be
/// created or a PNG cannot be encoded or written.
pub fn write_evidence_pngs(
    evidence: &AuditEvidence,
    dir: &Path,
) -> Result<Vec<String>, EvidenceError> {
    std::fs::create_dir_all(dir).map_err(|err| EvidenceError::PngFailed {
        error: format!("cannot create evidence directory {}: {err}", dir.display()),
    })?;
    let mut written = Vec::new();
    for change in &evidence.revision_text.changes {
        let stem = format!(
            "rev{:02}-to-rev{:02}-page-{:03}",
            change.previous_revision, change.revision, change.page
        );
        write_named(
            dir,
            &format!("{stem}-before.png"),
            &change.before,
            &mut written,
        )?;
        write_named(
            dir,
            &format!("{stem}-after.png"),
            &change.after,
            &mut written,
        )?;
        if let Some(diff) = &change.diff {
            let overlay = highlight_changed_regions(&change.after, diff)?;
            write_named(dir, &format!("{stem}-diff.png"), &overlay, &mut written)?;
        }
    }
    let mut overlay_pages = BTreeSet::new();
    for overlay in &evidence.overlays.overlays {
        if overlay_pages.insert(overlay.page) {
            let stem = format!("page-{:03}-annot", overlay.page);
            write_named(dir, &format!("{stem}-off.png"), &overlay.off, &mut written)?;
            write_named(dir, &format!("{stem}-on.png"), &overlay.on, &mut written)?;
            let diff_image = highlight_changed_regions(&overlay.on, &overlay.diff)?;
            write_named(dir, &format!("{stem}-diff.png"), &diff_image, &mut written)?;
        }
    }
    let mut occlusion_pages = BTreeSet::new();
    for occlusion in &evidence.occlusions.occlusions {
        if occlusion_pages.insert(occlusion.page) {
            write_named(
                dir,
                &format!("page-{:03}-content.png", occlusion.page),
                &occlusion.render,
                &mut written,
            )?;
        }
    }
    written.sort();
    Ok(written)
}

/// Write one render under `name` in `dir` and record the name.
fn write_named(
    dir: &Path,
    name: &str,
    render: &EvidenceRender,
    written: &mut Vec<String>,
) -> Result<(), EvidenceError> {
    write_png(&dir.join(name), render)?;
    written.push(name.to_owned());
    Ok(())
}
