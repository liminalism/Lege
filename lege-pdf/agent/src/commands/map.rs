//! One line per page: what is on it and how to read it.
//!
//! Before reading a long document an agent needs to know where the text is,
//! which pages are scans, where the figures and the front matter are, and
//! what the printed page numbers are. `inspect` answers for the document;
//! this answers for every page, cheaply enough to run on all of them.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use pdf_document::{DocumentSnapshot, PageIndex};
use pdf_text::TextPageOptions;
use serde::Serialize;

use crate::bounds::Bounds;
use crate::commands::text::{self, OcrMode, TextLayout};
use crate::commands::{PageOutcome, for_pages, resolve_snapshot};
use crate::objects;
use crate::open::DocumentIdentity;
use crate::pages::{PageZero, parse_scan_range};
use crate::schema::{Envelope, OutputMode, Status};

/// Characters of a page's first line kept in the map.
const FIRST_LINE_CHARS: usize = 80;

#[derive(Debug)]
pub struct MapArgs<'a> {
    pub path: &'a Path,
    pub password: Option<&'a str>,
    pub pages: Option<&'a str>,
    pub system_fonts: bool,
    pub bounds: Bounds,
    pub output: OutputMode,
    pub snapshot: Option<Arc<DocumentSnapshot>>,
    pub identity: Option<DocumentIdentity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MapPage {
    pub page: u32,
    /// The page number printed on the page, when the document declares
    /// labels and it differs from `page`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Size as displayed, in points, after rotation.
    pub width: f64,
    pub height: f64,
    /// `native-text`, `hybrid` (text and images), `scanned-image` (no text
    /// layer: needs OCR), `render-required` (text that cannot be trusted or
    /// nothing but vector art), `blank` (nothing drawn), or `failed`.
    pub kind: &'static str,
    pub chars: usize,
    pub words: usize,
    pub images: usize,
    /// The first line of text on the page, shortened.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub first_line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MapData {
    pub page_count: u32,
    pub pages_selected: usize,
    /// How many of the mapped pages are of each kind.
    pub kinds: BTreeMap<&'static str, usize>,
    pub chars: usize,
    pub truncated: bool,
    /// The run stopped because its time ran out, not at a page limit.
    #[serde(skip)]
    pub timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<u32>,
    pub pages: Vec<MapPage>,
}

pub fn run(args: MapArgs<'_>) -> Result<i32> {
    let (identity, snapshot) =
        resolve_snapshot(args.path, args.password, args.snapshot, args.identity)?;
    let document = identity.display_path();
    let (data, warnings) = map_document(&snapshot, args.pages, args.system_fonts, &args.bounds)?;
    let failed = data.timed_out || data.pages.iter().any(|page| page.error.is_some());
    match args.output {
        OutputMode::Human => {
            for page in &data.pages {
                println!("{}", page_line(page));
            }
            eprintln!("{}", summary_line(&data));
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
        }
        OutputMode::Json | OutputMode::Jsonl => {
            let env = Envelope::ok(&document, serde_json::to_value(&data)?)
                .with_warnings(warnings)
                .set_status(if data.truncated {
                    Status::Truncated
                } else {
                    Status::Ok
                });
            if matches!(args.output, OutputMode::Json) {
                env.write_json()?;
            } else {
                env.write_jsonl()?;
            }
        }
    }
    Ok(i32::from(failed))
}

/// Map a page selection. Only a page limit the caller asked for applies.
pub fn map_document(
    snapshot: &DocumentSnapshot,
    pages: Option<&str>,
    system_fonts: bool,
    bounds: &Bounds,
) -> Result<(MapData, Vec<String>)> {
    let scan = parse_scan_range(pages, snapshot.page_count(), bounds.scan_pages)?;
    let (selection, mut warnings) = (scan.pages, scan.warnings);
    let labels = objects::page_labels(snapshot);
    let mut data = MapData {
        page_count: snapshot.page_count(),
        pages_selected: scan.selected,
        kinds: BTreeMap::new(),
        chars: 0,
        truncated: false,
        timed_out: false,
        next_page: None,
        pages: Vec::with_capacity(selection.len()),
    };

    for_pages(
        &selection,
        bounds.threads,
        || text::PageState::new(system_fonts),
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
                // The map counts words; it does not return them.
                &Bounds {
                    max_items: 0,
                    ..*bounds
                },
                warnings,
                &mut state.ocr_backend,
                &mut state.glyphs,
            )?;
            // Trust warnings are what `kind` already says.
            warnings.clear();
            Ok((
                extracted.provenance.page_content_kind,
                extracted.provenance.native_char_count,
                extracted.words.map_or(0, |words| words.len()),
                extracted.provenance.image_count,
                first_line(extracted.text.as_deref().unwrap_or_default()),
            ))
        },
        |index, page, outcome, _| {
            let (width, height) = page_size(snapshot, page);
            let mut entry = MapPage {
                page: page.one_based(),
                label: objects::label_for(&labels, page.0)
                    .filter(|label| !label.is_empty() && *label != page.one_based().to_string()),
                width,
                height,
                kind: "failed",
                chars: 0,
                words: 0,
                images: 0,
                first_line: String::new(),
                error: None,
            };
            match outcome {
                PageOutcome::TimedOut => {
                    data.truncated = true;
                    data.timed_out = true;
                    data.next_page = Some(page.one_based());
                    warnings.push(format!(
                        "{}; pages from {} on were not mapped",
                        crate::deadline::message(index, selection.len()),
                        page.one_based()
                    ));
                    return ControlFlow::Break(());
                }
                PageOutcome::Failed(error) => entry.error = Some(format!("{error:#}")),
                PageOutcome::Done((kind, chars, words, images, first_line)) => {
                    entry.kind = kind;
                    entry.chars = chars;
                    entry.words = words;
                    entry.images = images;
                    entry.first_line = first_line;
                }
            }
            *data.kinds.entry(entry.kind).or_default() += 1;
            data.chars += entry.chars;
            data.pages.push(entry);
            ControlFlow::Continue(())
        },
    );
    if let (false, Some(next)) = (data.truncated, scan.next_page) {
        data.truncated = true;
        data.next_page = Some(next);
    }
    Ok((data, warnings))
}

/// Width and height of a page as displayed, in points.
pub(crate) fn page_size(snapshot: &DocumentSnapshot, page: PageZero) -> (f64, f64) {
    let Ok(page) = snapshot.page(PageIndex(page.0)) else {
        return (0.0, 0.0);
    };
    let [x0, y0, x1, y1] = page.crop_box;
    let (width, height) = ((x1 - x0).abs(), (y1 - y0).abs());
    let round = |value: f64| (value * 10.0).round() / 10.0;
    if page.rotate % 180 == 90 {
        (round(height), round(width))
    } else {
        (round(width), round(height))
    }
}

/// The first line that says something: a line of a lone page number or a
/// rule of dots is passed over when a better one follows.
fn first_line(text: &str) -> String {
    let mut fallback = None;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.chars().filter(|c| c.is_alphabetic()).count() >= 3 {
            return shorten(line);
        }
        fallback.get_or_insert(line);
    }
    fallback.map(shorten).unwrap_or_default()
}

fn shorten(line: &str) -> String {
    let line = &crate::views::tidy_line(line);
    let mut out: String = line.chars().take(FIRST_LINE_CHARS).collect();
    if line.chars().count() > FIRST_LINE_CHARS {
        out.push('…');
    }
    out
}

/// `p.12 (xii) 612x792 native-text 2345 chars 3 img | First line`
pub(crate) fn page_line(page: &MapPage) -> String {
    let mut line = format!("p.{}", page.page);
    if let Some(label) = &page.label {
        line.push_str(&format!(" ({label})"));
    }
    line.push_str(&format!(" {}x{} {}", page.width, page.height, page.kind));
    if let Some(error) = &page.error {
        line.push_str(&format!(": {error}"));
        return line;
    }
    line.push_str(&format!(" {} chars", page.chars));
    if page.images > 0 {
        line.push_str(&format!(" {} img", page.images));
    }
    if !page.first_line.is_empty() {
        line.push_str(&format!(" | {}", page.first_line));
    }
    line
}

pub(crate) fn summary_line(data: &MapData) -> String {
    let kinds: Vec<String> = data
        .kinds
        .iter()
        .map(|(kind, count)| format!("{count} {kind}"))
        .collect();
    format!(
        "{} of {} pages mapped: {}; {} characters of text",
        data.pages.len(),
        data.page_count,
        if kinds.is_empty() {
            "none".to_owned()
        } else {
            kinds.join(", ")
        },
        data.chars
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_first_line_is_the_first_one_with_words() {
        assert_eq!(first_line("\n  12 \n\nChapter One\nmore"), "Chapter One");
        assert_eq!(first_line("12\n....."), "12");
        assert_eq!(first_line(""), "");
        let long = "word ".repeat(40);
        let shortened = first_line(&long);
        assert_eq!(shortened.chars().count(), FIRST_LINE_CHARS + 1);
        assert!(shortened.ends_with('…'));
    }

    #[test]
    fn a_page_line_says_what_matters_and_no_more() {
        let mut page = MapPage {
            page: 12,
            label: Some("xii".into()),
            width: 612.0,
            height: 792.0,
            kind: "hybrid",
            chars: 2345,
            words: 400,
            images: 3,
            first_line: "Contents".into(),
            error: None,
        };
        assert_eq!(
            page_line(&page),
            "p.12 (xii) 612x792 hybrid 2345 chars 3 img | Contents"
        );
        page.label = None;
        page.images = 0;
        page.first_line.clear();
        page.kind = "scanned-image";
        page.chars = 0;
        assert_eq!(page_line(&page), "p.12 612x792 scanned-image 0 chars");
    }
}
