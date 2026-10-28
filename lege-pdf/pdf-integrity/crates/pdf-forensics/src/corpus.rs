//! Provenance-labeled legitimate-corpus measurement, before M5 heuristics ship.
//!
//! A manifest labels operations and producers; filenames and inferred producer
//! strings cannot establish legitimacy. Duplicate bytes, missing files, failed
//! detectors and partial audits never become clean samples in the denominator.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{AnalysisMode, CompileOutcome, EvidenceStrength, audit_bytes_with_evidence};

/// Version of both the manifest and sweep-report schemas.
pub const CORPUS_SCHEMA_VERSION: u32 = 1;
/// Minimum independent samples per producer family in the M5 decision.
pub const MIN_DOCUMENTS: usize = 200;
/// Minimum producer families in the M5 decision.
pub const MIN_PRODUCERS: usize = 8;

/// Explicit provenance for every legitimate input, in `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusManifest {
    /// Must be [`CORPUS_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// All producer families in scope, including families not yet collected.
    pub producer_families: Vec<String>,
    /// Collected samples. Paths are relative to the corpus directory.
    pub documents: Vec<CorpusDocument>,
}

/// A labeled sample; provenance is supplied by the corpus curator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusDocument {
    /// Relative PDF path inside the corpus directory.
    pub path: PathBuf,
    /// One of the manifest's declared producer families.
    pub producer_family: String,
    /// Known producing operation/source, never an inferred fraud judgment.
    pub provenance: String,
    /// Expected SHA-256 of the original bytes, lowercase hex.
    pub sha256: String,
}

/// What happened to one sample. Invalid samples remain visible in the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleMeasurement {
    /// The curator's label and expected identity.
    pub document: CorpusDocument,
    /// Actual identity when the file could be read.
    pub actual_sha256: Option<String>,
    /// Why this is excluded from rate denominators, if anything.
    pub exclusion: Option<String>,
    /// Codes emitted at any strength, once per document.
    pub finding_codes: Vec<String>,
    /// Codes strong enough to recommend review, once per document.
    pub review_codes: Vec<String>,
}

/// Per-producer rates use documents, never number of finding instances.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProducerMeasurement {
    /// Manifest sample count, including unusable samples.
    pub labeled_documents: usize,
    /// Unique, fully evaluated documents.
    pub evaluated_documents: usize,
    /// Samples excluded from the denominator.
    pub excluded_documents: usize,
    /// Documents with at least one review-strength finding.
    pub review_documents: usize,
    /// Absent when there are no evaluated documents.
    pub review_rate: Option<f64>,
    /// Review-strength documents per code; repeats on one page count once.
    pub review_documents_by_code: BTreeMap<String, usize>,
    /// Every emitted code, including weak signals, counted per document.
    pub documents_by_code: BTreeMap<String, usize>,
    /// Directly measured budget: at most one review document per 200.
    /// `None` means the sample is too small to assess the budget.
    pub within_review_budget: Option<bool>,
}

/// A sweep is a measurement, never evidence of authenticity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusSweepReport {
    /// Schema version, independent of the ordinary audit-report schema.
    pub schema_version: u32,
    /// Rates in stable producer-family order.
    pub producers: BTreeMap<String, ProducerMeasurement>,
    /// Input outcomes in manifest order.
    pub samples: Vec<SampleMeasurement>,
    /// Corpus collection gate; requires eight families of 200 usable samples
    /// and no excluded inputs. It says nothing about heuristic effectiveness.
    pub corpus_ready: bool,
    /// The M5 heuristic layer evaluated by this build. Empty until heuristics
    /// are implemented and validated; baseline M1–M4 findings are still counted.
    pub evaluated_heuristics: Vec<String>,
    /// Always false while the heuristic registry is empty. Zero findings from
    /// an absent detector must never satisfy M5's acceptance check.
    pub m5_false_positive_acceptance_satisfied: bool,
    /// Reasons M5 cannot yet be claimed complete.
    pub limitations: Vec<String>,
}

/// Validate a manifest before reading any PDFs.
pub fn validate_manifest(manifest: &CorpusManifest) -> Result<(), String> {
    if manifest.schema_version != CORPUS_SCHEMA_VERSION {
        return Err(format!("unsupported corpus schema {}", manifest.schema_version));
    }
    let families: BTreeSet<_> = manifest.producer_families.iter().collect();
    if families.len() != manifest.producer_families.len()
        || families.is_empty()
        || families.iter().any(|s| s.trim().is_empty())
    {
        return Err("producer_families must be nonempty, nonblank and unique".to_owned());
    }
    let mut paths = BTreeSet::new();
    for doc in &manifest.documents {
        if doc.path.as_os_str().is_empty()
            || doc.path.components().any(|c| !matches!(c, Component::Normal(_)))
            || !paths.insert(&doc.path)
        {
            return Err(format!("invalid or duplicate relative path: {}", doc.path.display()));
        }
        if !families.contains(&doc.producer_family) || doc.provenance.trim().is_empty() {
            return Err(format!("missing producer label or provenance for {}", doc.path.display()));
        }
        if doc.sha256.len() != 64
            || !doc.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(format!("expected lowercase SHA-256 for {}", doc.path.display()));
        }
    }
    Ok(())
}

/// Read `root/manifest.json` and audit every labeled sample, read-only.
///
/// Invalid manifests and root I/O errors return errors. Per-file failures are
/// recorded as exclusions so a failed file cannot silently lower a rate.
pub fn sweep_corpus(root: &Path) -> Result<CorpusSweepReport, String> {
    let manifest_path = root.join("manifest.json");
    let manifest: CorpusManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?,
    ).map_err(|e| format!("invalid {}: {e}", manifest_path.display()))?;
    validate_manifest(&manifest)?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut seen_hashes = BTreeSet::new();
    let mut samples = Vec::new();
    for doc in &manifest.documents {
        let mut sample = SampleMeasurement {
            document: doc.clone(), actual_sha256: None, exclusion: None,
            finding_codes: Vec::new(), review_codes: Vec::new(),
        };
        let result = measure_sample(&root, doc, &mut seen_hashes, &mut sample);
        if let Err(reason) = result {
            sample.exclusion = Some(reason);
        }
        samples.push(sample);
    }
    Ok(summarize(&manifest.producer_families, samples))
}

fn measure_sample(
    root: &Path,
    doc: &CorpusDocument,
    seen_hashes: &mut BTreeSet<String>,
    sample: &mut SampleMeasurement,
) -> Result<(), String> {
    let path = root.join(&doc.path).canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(root) {
        return Err("sample resolves outside corpus directory".to_owned());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let hash = crate::intake::hex_sha256(&bytes);
    sample.actual_sha256 = Some(hash.clone());
    if hash != doc.sha256 {
        return Err("sample SHA-256 differs from manifest".to_owned());
    }
    if !seen_hashes.insert(hash) {
        return Err("duplicate PDF bytes; cannot count as an independent sample".to_owned());
    }
    let (report, evidence) = audit_bytes_with_evidence(&bytes, None);
    let codes: BTreeSet<_> = report.findings.iter().map(|f| f.code.to_string()).collect();
    let review: BTreeSet<_> = report.findings.iter()
        .filter(|f| !matches!(f.strength, EvidenceStrength::WeakHeuristic))
        .map(|f| f.code.to_string()).collect();
    sample.finding_codes = codes.into_iter().collect();
    sample.review_codes = review.into_iter().collect();
    if report.analysis_mode != AnalysisMode::Full {
        return Err("structure-only audit; stream-dependent detectors did not run".to_owned());
    }
    if report.document.parse.rests_on_recovered_structure() {
        return Err("recovered structure; not a clean baseline sample".to_owned());
    }
    if report.document.pages.value().is_none_or(|pages| pages.is_empty()
        || pages.iter().any(|p| !matches!(p.compile, CompileOutcome::Ok { .. })))
    {
        return Err("page compilation incomplete or degraded".to_owned());
    }
    if !evidence.revision_text.skipped.is_empty()
        || !evidence.overlays.skipped.is_empty()
        || !evidence.occlusions.skipped.is_empty()
    {
        return Err("one or more evidence detectors skipped a page or revision".to_owned());
    }
    Ok(())
}

fn summarize(families: &[String], samples: Vec<SampleMeasurement>) -> CorpusSweepReport {
    let mut producers: BTreeMap<_, ProducerMeasurement> = families.iter()
        .map(|family| (family.clone(), ProducerMeasurement::default())).collect();
    for sample in &samples {
        let stats = producers.entry(sample.document.producer_family.clone()).or_default();
        stats.labeled_documents += 1;
        if sample.exclusion.is_some() {
            stats.excluded_documents += 1;
            continue;
        }
        stats.evaluated_documents += 1;
        stats.review_documents += usize::from(!sample.review_codes.is_empty());
        for code in sample.finding_codes.iter().collect::<BTreeSet<_>>() {
            *stats.documents_by_code.entry(code.clone()).or_default() += 1;
        }
        for code in sample.review_codes.iter().collect::<BTreeSet<_>>() {
            *stats.review_documents_by_code.entry(code.clone()).or_default() += 1;
        }
    }
    for stats in producers.values_mut() {
        stats.review_rate = (stats.evaluated_documents > 0)
            .then(|| stats.review_documents as f64 / stats.evaluated_documents as f64);
        stats.within_review_budget = (stats.evaluated_documents >= MIN_DOCUMENTS)
            .then_some(stats.review_documents <= stats.evaluated_documents / MIN_DOCUMENTS);
    }
    let corpus_ready = producers.len() >= MIN_PRODUCERS && producers.values().all(|s|
        s.evaluated_documents >= MIN_DOCUMENTS && s.excluded_documents == 0);
    let mut limitations = vec![
        "No M5 heuristics are registered in this build; baseline M1–M4 rates do not validate absent heuristics.".to_owned(),
        "Provenance labels are curator assertions. This sweep does not establish legitimacy or measure seeded-edit recall.".to_owned(),
    ];
    if !corpus_ready {
        limitations.push("Need at least eight declared producer families with 200 unique, fully evaluated PDFs each, without excluded samples.".to_owned());
    }
    CorpusSweepReport {
        schema_version: CORPUS_SCHEMA_VERSION, producers, samples, corpus_ready,
        evaluated_heuristics: Vec::new(), m5_false_positive_acceptance_satisfied: false, limitations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_per_producer_per_document_and_never_passes_an_absent_heuristic() {
        let families = vec!["office".to_owned(), "scanner".to_owned()];
        let mut samples = Vec::new();
        for family in &families {
            for i in 0..200 {
                let review = i < if family == "office" { 1 } else { 2 };
                samples.push(SampleMeasurement {
                    document: CorpusDocument { path: format!("{family}/{i}.pdf").into(),
                        producer_family: family.clone(), provenance: "test aggregation".into(), sha256: String::new() },
                    actual_sha256: None, exclusion: None,
                    finding_codes: if review { vec!["TEST".into(); 3] } else { Vec::new() },
                    review_codes: if review { vec!["TEST".into(); 3] } else { Vec::new() },
                });
            }
        }
        let report = summarize(&families, samples);
        assert_eq!(report.producers["office"].review_documents_by_code["TEST"], 1);
        assert_eq!(report.producers["office"].within_review_budget, Some(true));
        assert_eq!(report.producers["scanner"].within_review_budget, Some(false));
        assert!(!report.corpus_ready);
        assert!(!report.m5_false_positive_acceptance_satisfied);
    }
}
