//! Preflight: is this PDF sound?
//!
//! Made for the moment after a PDF has been generated, when the question is
//! not "what does it say" but "did it come out right": pages that fail or
//! only half-draw, text that runs off the page or cannot be searched because
//! its font has no Unicode mapping, fonts that are not embedded, images too
//! coarse to print, pages that are blank. Each finding names its pages, so
//! the next step is a render of exactly those.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use pdf_document::{DocumentSnapshot, PageIndex};
use pdf_read::{CompileStatus, OpenOutcome};
use pdf_text::TextPageOptions;
use serde::Serialize;

use crate::bounds::Bounds;
use crate::commands::text::{self, OcrMode, TextLayout};
use crate::commands::{PageOutcome, for_pages, resolve_snapshot};
use crate::open::DocumentIdentity;
use crate::pages::{PageZero, parse_scan_range};
use crate::schema::{Envelope, OutputMode, Status};

/// Pages examined per worker job.
const CHUNK: usize = 8;
/// Findings of one kind listed one by one before the rest are counted.
const MAX_PER_CODE: usize = 12;

#[derive(Debug)]
pub struct CheckArgs<'a> {
    pub path: &'a Path,
    pub password: Option<&'a str>,
    pub pages: Option<&'a str>,
    /// Images below this many pixels per inch on the page are reported.
    pub min_dpi: u32,
    /// Exit non-zero on warnings as well as errors.
    pub strict: bool,
    pub bounds: Bounds,
    pub output: OutputMode,
    pub snapshot: Option<Arc<DocumentSnapshot>>,
    pub identity: Option<DocumentIdentity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// `error` (content is lost or wrong), `warning` (likely a defect), or
    /// `note` (worth knowing, often intended).
    pub severity: &'static str,
    pub code: &'static str,
    /// The pages it applies to, as a page-range string; empty for a finding
    /// about the document.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pages: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckData {
    pub page_count: u32,
    pub pages_selected: usize,
    pub pages_checked: usize,
    /// Selected pages were left unchecked: a page limit, or the time ran out.
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<u32>,
    pub errors: usize,
    pub warnings: usize,
    pub notes: usize,
    pub findings: Vec<Finding>,
}

pub fn run(args: CheckArgs<'_>) -> Result<i32> {
    let (identity, snapshot) =
        resolve_snapshot(args.path, args.password, args.snapshot, args.identity)?;
    let document = identity.display_path();
    let (data, warnings) = check_document(&snapshot, args.pages, args.min_dpi, &args.bounds)?;
    let failed = data.errors > 0 || (args.strict && data.warnings > 0);
    match args.output {
        OutputMode::Human => {
            for line in lines(&data) {
                println!("{line}");
            }
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
        }
        OutputMode::Json | OutputMode::Jsonl => {
            let env = Envelope::ok(&document, serde_json::to_value(&data)?)
                .with_warnings(warnings)
                .set_status(if failed { Status::Failed } else { Status::Ok });
            if matches!(args.output, OutputMode::Json) {
                env.write_json()?;
            } else {
                env.write_jsonl()?;
            }
        }
    }
    Ok(i32::from(failed))
}

/// What the text of one page says about its soundness.
struct TextFacts {
    unmapped: usize,
    outside: usize,
    outside_sample: Vec<String>,
    spacing_lost: bool,
}

pub fn check_document(
    snapshot: &DocumentSnapshot,
    pages: Option<&str>,
    min_dpi: u32,
    bounds: &Bounds,
) -> Result<(CheckData, Vec<String>)> {
    let scan = parse_scan_range(pages, snapshot.page_count(), bounds.scan_pages)?;
    let (selection, mut warnings) = (scan.pages, scan.warnings);
    let mut found = Findings::default();

    // ── structure and drawing: one examiner report per chunk of pages ──
    let chunks: Vec<Vec<u32>> = selection
        .chunks(CHUNK)
        .map(|chunk| chunk.iter().map(|page| page.0).collect())
        .collect();
    let mut checked = 0usize;
    let mut timed_out = false;
    let mut unembedded: BTreeMap<String, ()> = BTreeMap::new();
    let mut sizes: BTreeMap<(u64, u64), Vec<u32>> = BTreeMap::new();
    let mut document_done = false;
    crate::parallel::ordered(
        &chunks,
        crate::parallel::worker_count(bounds.threads, chunks.len()),
        || (),
        |(), _, chunk| {
            (!crate::deadline::expired()).then(|| pdf_read::examine_snapshot(snapshot, Some(chunk)))
        },
        |_, chunk, report| {
            let Some(report) = report else {
                timed_out = true;
                return ControlFlow::Break(());
            };
            if !std::mem::replace(&mut document_done, true) {
                document_findings(&report, &mut found);
            }
            for font in report.fonts.iter().filter(|font| !font.embedded) {
                let name = match font.base_font.as_str() {
                    "" => format!("(unnamed {} font)", font.subtype),
                    name => name.to_owned(),
                };
                unembedded.insert(name, ());
            }
            for status in &report.pages {
                let page = status.index + 1;
                match &status.compile {
                    CompileStatus::Failed { error } => found.page(
                        "error",
                        "page-failed",
                        page,
                        format!("the page could not be read: {error}"),
                    ),
                    CompileStatus::Degraded { detail, .. } => found.page(
                        "warning",
                        "page-degraded",
                        page,
                        format!("part of the page could not be drawn: {detail}"),
                    ),
                    CompileStatus::Ok { .. } => {}
                }
                let Some(metrics) = &status.metrics else {
                    continue;
                };
                let drawn = metrics.text_runs
                    + metrics.images
                    + metrics.path_paints
                    + metrics.shading_paints;
                if drawn == 0 {
                    found.pages("note", "blank-page", page);
                } else if metrics.text_runs == 0 && metrics.images > 0 {
                    found.pages("note", "no-text-layer", page);
                }
                if let Some(dpi) = metrics.effective_dpi.filter(|dpi| *dpi < min_dpi) {
                    found.pages("warning", "low-resolution-image", page);
                    found.lowest_dpi = Some(found.lowest_dpi.map_or(dpi, |low| low.min(dpi)));
                }
            }
            for index in chunk {
                let (width, height) = crate::commands::map::page_size(snapshot, PageZero(*index));
                sizes
                    .entry((width.round() as u64, height.round() as u64))
                    .or_default()
                    .push(index + 1);
            }
            checked += chunk.len();
            ControlFlow::Continue(())
        },
    );

    // ── text: what can be read back out ──
    if !timed_out {
        for_pages(
            &selection,
            bounds.threads,
            || text::PageState::new(false),
            |state, page, warnings| {
                let extracted = text::extract_page(
                    snapshot,
                    &state.compiler,
                    page,
                    &TextPageOptions::default(),
                    TextLayout::Words,
                    None,
                    OcrMode::Never,
                    "",
                    &Bounds {
                        max_items: 0,
                        ..*bounds
                    },
                    warnings,
                    &mut state.ocr_backend,
                    &mut state.glyphs,
                )?;
                let crop = snapshot
                    .page(PageIndex(page.0))
                    .map(|page| page.crop_box)
                    .unwrap_or([
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                        f64::INFINITY,
                        f64::INFINITY,
                    ]);
                let (x0, x1) = (crop[0].min(crop[2]) - 1.0, crop[0].max(crop[2]) + 1.0);
                let (y0, y1) = (crop[1].min(crop[3]) - 1.0, crop[1].max(crop[3]) + 1.0);
                let words = extracted.words.unwrap_or_default();
                let outside: Vec<&crate::views::text::WordView> = words
                    .iter()
                    .filter(|word| {
                        word.bbox[2] > word.bbox[0]
                            && (word.bbox[0] < x0
                                || word.bbox[2] > x1
                                || word.bbox[1] < y0
                                || word.bbox[3] > y1)
                    })
                    .collect();
                Ok(TextFacts {
                    unmapped: extracted
                        .text
                        .as_deref()
                        .unwrap_or_default()
                        .chars()
                        .filter(|character| *character == '\u{fffd}')
                        .count(),
                    outside: outside.len(),
                    outside_sample: outside
                        .iter()
                        .take(3)
                        .map(|word| word.text.clone())
                        .collect(),
                    spacing_lost: warnings
                        .iter()
                        .any(|warning| warning.contains("almost no word breaks")),
                })
            },
            |_, page, outcome, _| {
                match outcome {
                    PageOutcome::TimedOut => {
                        timed_out = true;
                        return ControlFlow::Break(());
                    }
                    // Already reported from the examiner's side.
                    PageOutcome::Failed(_) => {}
                    PageOutcome::Done(facts) => {
                        let page = page.one_based();
                        if facts.unmapped > 0 {
                            found.pages("warning", "unmapped-text", page);
                            found.unmapped += facts.unmapped;
                        }
                        if facts.outside > 0 {
                            found.pages("warning", "text-outside-page", page);
                            found.outside += facts.outside;
                            if found.outside_sample.len() < 6 {
                                found.outside_sample.extend(facts.outside_sample);
                            }
                        }
                        if facts.spacing_lost {
                            found.pages("warning", "word-spacing-lost", page);
                        }
                    }
                }
                ControlFlow::Continue(())
            },
        );
    }
    if timed_out {
        warnings.push(format!(
            "{}; the findings cover only what was checked before it ran out",
            crate::deadline::message(checked, selection.len())
        ));
    }

    if !unembedded.is_empty() {
        let names: Vec<&str> = unembedded.keys().map(String::as_str).collect();
        let shown = names.len().min(MAX_PER_CODE);
        found.document(
            "warning",
            "font-not-embedded",
            format!(
                "{} font{} not embedded, so the text is drawn with whatever the viewer substitutes: {}{}",
                names.len(),
                if names.len() == 1 { " is" } else { "s are" },
                names[..shown].join(", "),
                if names.len() > shown { ", …" } else { "" }
            ),
        );
    }
    if sizes.len() > 1 {
        let mut by_count: Vec<(&(u64, u64), &Vec<u32>)> = sizes.iter().collect();
        by_count.sort_by_key(|entry| std::cmp::Reverse(entry.1.len()));
        let listed: Vec<String> = by_count
            .iter()
            .take(4)
            .map(|((width, height), pages)| {
                format!(
                    "{width}x{height} pt on {} page{}",
                    pages.len(),
                    if pages.len() == 1 { "" } else { "s" }
                )
            })
            .collect();
        found.document(
            "note",
            "mixed-page-sizes",
            format!(
                "{} different page sizes: {}{}",
                sizes.len(),
                listed.join(", "),
                if sizes.len() > 4 { ", …" } else { "" }
            ),
        );
    }

    let findings = found.finish(min_dpi);
    let count = |severity: &str| {
        findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .count()
    };
    Ok((
        CheckData {
            page_count: snapshot.page_count(),
            pages_selected: scan.selected,
            pages_checked: checked,
            truncated: timed_out || scan.next_page.is_some(),
            next_page: scan.next_page,
            errors: count("error"),
            warnings: count("warning"),
            notes: count("note"),
            findings,
        },
        warnings,
    ))
}

fn document_findings(report: &pdf_read::DocumentReport, found: &mut Findings) {
    if let OpenOutcome::Recovered { how } = &report.open {
        found.document(
            "warning",
            "repaired-on-open",
            format!(
                "the file structure is damaged and was repaired to read it: {}",
                how.join("; ")
            ),
        );
    }
    if let Some(encryption) = &report.encryption {
        found.document(
            "note",
            "encrypted",
            format!(
                "the document is encrypted ({}){}",
                encryption.method,
                if encryption.user_password_empty {
                    "; it opens without a password"
                } else {
                    ""
                }
            ),
        );
    }
    if report.features.has_javascript {
        found.document(
            "warning",
            "javascript",
            "the document carries JavaScript".to_owned(),
        );
    }
    if report.features.has_xfa {
        found.document(
            "warning",
            "xfa-form",
            "the form is XFA, which most viewers no longer show".to_owned(),
        );
    }
    if !report.identity.info.contains_key("Title") {
        found.document(
            "note",
            "no-title",
            "the document has no title in its metadata".to_owned(),
        );
    }
}

/// Findings as they accumulate: per-page ones with their own message, and
/// per-code ones that only collect pages.
#[derive(Default)]
struct Findings {
    document: Vec<Finding>,
    /// (severity, code, message) → pages.
    with_message: BTreeMap<(&'static str, &'static str, String), Vec<u32>>,
    /// (severity, code) → pages.
    by_code: BTreeMap<(&'static str, &'static str), Vec<u32>>,
    unmapped: usize,
    outside: usize,
    outside_sample: Vec<String>,
    lowest_dpi: Option<u32>,
}

impl Findings {
    fn document(&mut self, severity: &'static str, code: &'static str, message: String) {
        self.document.push(Finding {
            severity,
            code,
            pages: String::new(),
            message,
        });
    }

    fn page(&mut self, severity: &'static str, code: &'static str, page: u32, message: String) {
        self.with_message
            .entry((severity, code, message))
            .or_default()
            .push(page);
    }

    fn pages(&mut self, severity: &'static str, code: &'static str, page: u32) {
        let pages = self.by_code.entry((severity, code)).or_default();
        if pages.last() != Some(&page) {
            pages.push(page);
        }
    }

    fn finish(self, min_dpi: u32) -> Vec<Finding> {
        let mut out = Vec::new();
        let mut per_code: BTreeMap<&'static str, usize> = BTreeMap::new();
        for ((severity, code, message), mut pages) in self.with_message {
            pages.sort_unstable();
            let listed = per_code.entry(code).or_default();
            *listed += 1;
            if *listed > MAX_PER_CODE {
                continue;
            }
            out.push(Finding {
                severity,
                code,
                pages: ranges(&pages),
                message,
            });
        }
        for (code, listed) in per_code {
            if listed > MAX_PER_CODE {
                out.push(Finding {
                    severity: "note",
                    code,
                    pages: String::new(),
                    message: format!(
                        "{} more distinct {code} findings not listed",
                        listed - MAX_PER_CODE
                    ),
                });
            }
        }
        for ((severity, code), mut pages) in self.by_code {
            pages.sort_unstable();
            let count = pages.len();
            let plural = if count == 1 { "" } else { "s" };
            let message = match code {
                "blank-page" => format!("{count} page{plural} with nothing drawn"),
                "no-text-layer" => format!(
                    "{count} page{plural} of images with no text layer: text and search need OCR"
                ),
                "low-resolution-image" => format!(
                    "images below {min_dpi} dpi on {count} page{plural} (lowest {} dpi): they will look coarse in print",
                    self.lowest_dpi.unwrap_or(0)
                ),
                "unmapped-text" => format!(
                    "{} character{} on {count} page{plural} have no Unicode mapping: the text displays but cannot be searched, copied or extracted",
                    self.unmapped,
                    if self.unmapped == 1 { "" } else { "s" }
                ),
                "text-outside-page" => format!(
                    "{} word{} on {count} page{plural} lie outside or across the page edge and are cut off (for example {})",
                    self.outside,
                    if self.outside == 1 { "" } else { "s" },
                    self.outside_sample
                        .iter()
                        .take(4)
                        .map(|word| format!("{word:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "word-spacing-lost" => format!(
                    "text on {count} page{plural} extracts with almost no word breaks: spaces are missing from the text layer"
                ),
                other => format!("{other} on {count} page{plural}"),
            };
            out.push(Finding {
                severity,
                code,
                pages: ranges(&pages),
                message,
            });
        }
        out.extend(self.document);
        let rank = |severity: &str| match severity {
            "error" => 0,
            "warning" => 1,
            _ => 2,
        };
        out.sort_by_key(|finding| rank(finding.severity));
        out
    }
}

/// One-based pages as a page-range string. Up to [`MAX_RUNS`] runs it is
/// usable as a `pages` argument as it stands; past that the rest is counted.
fn ranges(pages: &[u32]) -> String {
    /// Runs written out before the rest is summarised.
    const MAX_RUNS: usize = 16;
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &page in pages {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == page || *end == page => *end = page,
            _ => runs.push((page, page)),
        }
    }
    let mut out: Vec<String> = runs
        .iter()
        .take(MAX_RUNS)
        .map(|(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}-{end}")
            }
        })
        .collect();
    if runs.len() > MAX_RUNS {
        let more: u32 = runs[MAX_RUNS..]
            .iter()
            .map(|(start, end)| end - start + 1)
            .sum();
        out.push(format!("… and {more} more"));
    }
    out.join(",")
}

/// The findings as lines, worst first, under a one-line verdict.
pub(crate) fn lines(data: &CheckData) -> Vec<String> {
    let mut out = vec![format!(
        "{} of {} pages checked: {} error{}, {} warning{}, {} note{}",
        data.pages_checked,
        data.page_count,
        data.errors,
        if data.errors == 1 { "" } else { "s" },
        data.warnings,
        if data.warnings == 1 { "" } else { "s" },
        data.notes,
        if data.notes == 1 { "" } else { "s" },
    )];
    for finding in &data.findings {
        out.push(if finding.pages.is_empty() {
            format!("{} {}: {}", finding.severity, finding.code, finding.message)
        } else {
            format!(
                "{} {} p.{}: {}",
                finding.severity, finding.code, finding.pages, finding.message
            )
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn findings_are_grouped_by_pages_and_ordered_worst_first() {
        let mut found = Findings::default();
        found.document("note", "no-title", "no title".into());
        for page in [3, 4, 5, 9] {
            found.pages("note", "blank-page", page);
        }
        found.pages("warning", "unmapped-text", 2);
        found.unmapped = 7;
        found.page(
            "error",
            "page-failed",
            6,
            "the page could not be read: x".into(),
        );
        found.page(
            "error",
            "page-failed",
            8,
            "the page could not be read: x".into(),
        );

        let findings = found.finish(100);
        let codes: Vec<&str> = findings.iter().map(|finding| finding.code).collect();
        assert_eq!(
            codes,
            ["page-failed", "unmapped-text", "blank-page", "no-title"]
        );
        assert_eq!(findings[0].pages, "6,8");
        assert_eq!(findings[2].pages, "3-5,9");
        assert!(findings[1].message.starts_with("7 characters on 1 page "));
    }

    #[test]
    fn many_distinct_failures_are_counted_not_listed() {
        let mut found = Findings::default();
        for page in 1..=20 {
            found.page("error", "page-failed", page, format!("failure {page:02}"));
        }
        let findings = found.finish(100);
        assert_eq!(findings.len(), MAX_PER_CODE + 1);
        assert!(
            findings
                .iter()
                .any(|finding| finding.message == "8 more distinct page-failed findings not listed")
        );
    }
}
