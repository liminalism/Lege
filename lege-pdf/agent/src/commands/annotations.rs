//! Links, comments and form fields: the parts of a PDF that are not page
//! content and that text extraction never shows.
//!
//! A link is reported with the text it sits on and where it goes — a page of
//! this document or a URL — which is how a contents page, a citation or a
//! "see section 4" is followed. A comment is reported with its author and
//! text. A form field is reported with its name and current value.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use pdf_document::{DocumentLinkTarget, DocumentSnapshot, PageIndex};
use pdf_text::TextPageOptions;
use serde::Serialize;

use crate::bounds::Bounds;
use crate::commands::text::{self, OcrMode, TextLayout};
use crate::commands::{PageOutcome, for_pages, parse_context, resolve_snapshot};
use crate::objects::{self, FormField};
use crate::open::DocumentIdentity;
use crate::pages::{PageZero, parse_scan_range};
use crate::schema::{Envelope, OutputMode};

/// Which of the three kinds to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kinds {
    pub links: bool,
    pub comments: bool,
    pub forms: bool,
}

impl Kinds {
    pub const ALL: Self = Self {
        links: true,
        comments: true,
        forms: true,
    };

    /// The flags as given; none given means all.
    pub fn from_flags(links: bool, comments: bool, forms: bool) -> Self {
        if links || comments || forms {
            Self {
                links,
                comments,
                forms,
            }
        } else {
            Self::ALL
        }
    }
}

#[derive(Debug)]
pub struct AnnotationsArgs<'a> {
    pub path: &'a Path,
    pub password: Option<&'a str>,
    pub pages: Option<&'a str>,
    pub kinds: Kinds,
    pub bounds: Bounds,
    pub output: OutputMode,
    pub snapshot: Option<Arc<DocumentSnapshot>>,
    pub identity: Option<DocumentIdentity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnnotationsData {
    pub page_count: u32,
    pub pages_selected: usize,
    pub link_count: usize,
    pub comment_count: usize,
    pub form_field_count: usize,
    /// A page limit left selected pages unread.
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<u32>,
    /// Pages that carry a link or a comment; the others are left out.
    pub pages: Vec<PageAnnotations>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub form_fields: Vec<FormField>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageAnnotations {
    pub page: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<CommentView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkView {
    #[serde(serialize_with = "crate::views::rounded_box")]
    pub rect: [f64; 4],
    /// The page text under the link.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// One-based page of this document the link goes to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommentView {
    /// `Text` (a sticky note), `Highlight`, `FreeText`, `Stamp`, …
    pub subtype: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::views::rounded_box_opt"
    )]
    pub rect: Option<[f64; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// For a markup annotation (highlight, underline, strike-out), the page
    /// text it marks.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub marked_text: String,
}

pub fn run(args: AnnotationsArgs<'_>) -> Result<i32> {
    let (identity, snapshot) =
        resolve_snapshot(args.path, args.password, args.snapshot, args.identity)?;
    let document = identity.display_path();
    let (data, warnings) = collect(&snapshot, args.pages, args.kinds, &args.bounds)?;
    match args.output {
        OutputMode::Human => {
            for line in lines(&data) {
                println!("{line}");
            }
            eprintln!("{}", summary_line(&data));
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
        }
        OutputMode::Json => Envelope::ok(&document, serde_json::to_value(&data)?)
            .with_warnings(warnings)
            .write_json()?,
        OutputMode::Jsonl => Envelope::ok(&document, serde_json::to_value(&data)?)
            .with_warnings(warnings)
            .write_jsonl()?,
    }
    Ok(0)
}

/// Annotation subtypes that are reported elsewhere or carry nothing to read.
fn is_comment(subtype: &str) -> bool {
    !matches!(
        subtype,
        "Link" | "Widget" | "Popup" | "PrinterMark" | "TrapNet"
    )
}

/// Everything the selected pages carry, and the document's form fields.
pub fn collect(
    snapshot: &DocumentSnapshot,
    pages: Option<&str>,
    kinds: Kinds,
    bounds: &Bounds,
) -> Result<(AnnotationsData, Vec<String>)> {
    let scan = parse_scan_range(pages, snapshot.page_count(), bounds.scan_pages)?;
    let (selection, mut warnings) = (scan.pages, scan.warnings);
    let mut by_page: BTreeMap<u32, PageAnnotations> = BTreeMap::new();
    fn entry(by_page: &mut BTreeMap<u32, PageAnnotations>, index: u32) -> &mut PageAnnotations {
        by_page.entry(index).or_insert_with(|| PageAnnotations {
            page: index + 1,
            links: Vec::new(),
            comments: Vec::new(),
        })
    }
    let selected: std::collections::HashSet<u32> = selection.iter().map(|page| page.0).collect();
    let item_limit = match bounds.max_items {
        0 => usize::MAX,
        limit => limit as usize,
    };
    let mut cut_pages = 0usize;

    if kinds.links {
        let mut ctx = parse_context();
        let links = snapshot.links(&mut ctx);
        for (index, page_links) in links.pages.iter().enumerate() {
            let index = index as u32;
            if !selected.contains(&index) || page_links.is_empty() {
                continue;
            }
            if page_links.len() > item_limit {
                cut_pages += 1;
            }
            entry(&mut by_page, index).links = page_links
                .iter()
                .take(item_limit)
                .map(|link| {
                    let (target_page, uri) = match &link.target {
                        DocumentLinkTarget::Internal(destination) => {
                            (Some(destination.page.0 + 1), None)
                        }
                        DocumentLinkTarget::Uri(uri) => (None, Some(uri.to_string())),
                    };
                    LinkView {
                        rect: link.rect,
                        text: String::new(),
                        target_page,
                        uri,
                    }
                })
                .collect();
        }
    }

    if kinds.comments {
        // Document-wide and cheap: no page is compiled for it.
        let report = pdf_read::examine_snapshot(snapshot, Some(&[]));
        let mut nth_on_page: BTreeMap<u32, usize> = BTreeMap::new();
        for record in &report.annotations.annotations {
            let nth = nth_on_page.entry(record.page).or_default();
            let position = *nth;
            *nth += 1;
            let subtype = record
                .subtype
                .clone()
                .unwrap_or_else(|| "Unknown".to_owned());
            if !selected.contains(&record.page) || !is_comment(&subtype) {
                continue;
            }
            // The rectangle lives on the page's annotation list: matched by
            // object where there is one, by position otherwise.
            let rect = snapshot.page(PageIndex(record.page)).ok().and_then(|page| {
                page.annotations
                    .iter()
                    .find(|annotation| {
                        record.object.is_some() && annotation.source == record.object
                    })
                    .or_else(|| page.annotations.get(position))
                    .map(|annotation| annotation.rect)
            });
            let comments = &mut entry(&mut by_page, record.page).comments;
            if comments.len() >= item_limit {
                continue;
            }
            comments.push(CommentView {
                subtype,
                rect,
                contents: record
                    .contents
                    .clone()
                    .filter(|text| !text.trim().is_empty()),
                author: record.author.clone(),
                subject: record.subject.clone(),
                modified: record.modified.clone(),
                marked_text: String::new(),
            });
        }
    }

    // The text under each link and each markup, from the pages that have any.
    let with_boxes: Vec<PageZero> = by_page.keys().map(|index| PageZero(*index)).collect();
    for_pages(
        &with_boxes,
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
            warnings.clear();
            Ok(extracted.words.unwrap_or_default())
        },
        |_, page, outcome, _| {
            let PageOutcome::Done(words) = outcome else {
                // The link is still worth reporting without its text.
                return ControlFlow::Continue(());
            };
            let under = |rect: [f64; 4]| -> String {
                let (x0, x1) = (rect[0].min(rect[2]), rect[0].max(rect[2]));
                let (y0, y1) = (rect[1].min(rect[3]), rect[1].max(rect[3]));
                let inside: Vec<&str> = words
                    .iter()
                    .filter(|word| {
                        let x = (word.bbox[0] + word.bbox[2]) * 0.5;
                        let y = (word.bbox[1] + word.bbox[3]) * 0.5;
                        x >= x0 && x <= x1 && y >= y0 && y <= y1
                    })
                    .map(|word| word.text.as_str())
                    .collect();
                let text = crate::views::tidy_line(&inside.join(" "));
                match text.char_indices().nth(MAX_UNDER_TEXT) {
                    Some((at, _)) => format!("{}…", &text[..at]),
                    None => text,
                }
            };
            if let Some(entry) = by_page.get_mut(&page.0) {
                for link in &mut entry.links {
                    link.text = under(link.rect);
                }
                for comment in &mut entry.comments {
                    let markup = matches!(
                        comment.subtype.as_str(),
                        "Highlight" | "Underline" | "StrikeOut" | "Squiggly"
                    );
                    if let (true, Some(rect)) = (markup, comment.rect) {
                        comment.marked_text = under(rect);
                    }
                }
            }
            ControlFlow::Continue(())
        },
    );

    let mut form_fields = Vec::new();
    if kinds.forms {
        let (fields, cut) = objects::form_fields(snapshot, item_limit);
        if cut {
            warnings.push(format!(
                "form fields truncated to max-items={}",
                bounds.max_items
            ));
        }
        form_fields = fields;
    }
    if cut_pages > 0 {
        warnings.push(format!(
            "links truncated to max-items={} on {cut_pages} page(s)",
            bounds.max_items
        ));
    }

    let pages: Vec<PageAnnotations> = by_page.into_values().collect();
    Ok((
        AnnotationsData {
            page_count: snapshot.page_count(),
            pages_selected: scan.selected,
            truncated: scan.next_page.is_some(),
            next_page: scan.next_page,
            link_count: pages.iter().map(|page| page.links.len()).sum(),
            comment_count: pages.iter().map(|page| page.comments.len()).sum(),
            form_field_count: form_fields.len(),
            pages,
            form_fields,
        },
        warnings,
    ))
}

/// Characters of page text reported under one link or markup.
const MAX_UNDER_TEXT: usize = 200;

fn boxed(rect: [f64; 4]) -> String {
    rect.map(|edge| crate::views::round2(edge).to_string())
        .join(",")
}

/// The data as lines a person or a model reads top to bottom.
/// How many of each kind were found, and where a page limit stopped.
pub(crate) fn summary_line(data: &AnnotationsData) -> String {
    let count =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut line = format!(
        "{}, {}, {}",
        count(data.link_count, "link", "links"),
        count(data.comment_count, "comment", "comments"),
        count(data.form_field_count, "form field", "form fields"),
    );
    if let Some(next) = data.next_page {
        line.push_str(&format!("; pages from {next} on were not read"));
    }
    line
}

pub(crate) fn lines(data: &AnnotationsData) -> Vec<String> {
    let mut out = Vec::new();
    for page in &data.pages {
        for link in &page.links {
            let target = match (&link.uri, link.target_page) {
                (Some(uri), _) => uri.clone(),
                (None, Some(page)) => format!("p.{page}"),
                (None, None) => "?".to_owned(),
            };
            out.push(format!(
                "p.{} link [{}] {:?} -> {target}",
                page.page,
                boxed(link.rect),
                link.text
            ));
        }
        for comment in &page.comments {
            let mut line = format!("p.{} {}", page.page, comment.subtype);
            if let Some(rect) = comment.rect {
                line.push_str(&format!(" [{}]", boxed(rect)));
            }
            if let Some(author) = &comment.author {
                line.push_str(&format!(" by {author}"));
            }
            if let Some(modified) = &comment.modified {
                line.push_str(&format!(" ({modified})"));
            }
            if !comment.marked_text.is_empty() {
                line.push_str(&format!(" on {:?}", comment.marked_text));
            }
            if let Some(contents) = &comment.contents {
                line.push_str(&format!(": {contents}"));
            }
            out.push(line);
        }
    }
    for field in &data.form_fields {
        let mut line = format!("field {:?} ({})", field.name, field.kind);
        match &field.value {
            Some(value) => line.push_str(&format!(" = {value:?}")),
            None => line.push_str(" (empty)"),
        }
        if let Some(label) = &field.label {
            line.push_str(&format!(" label={label:?}"));
        }
        if !field.options.is_empty() {
            line.push_str(&format!(" options={:?}", field.options));
        }
        if field.required {
            line.push_str(" required");
        }
        if field.read_only {
            line.push_str(" read-only");
        }
        if let (Some(page), Some(rect)) = (field.page, field.rect) {
            line.push_str(&format!(" p.{page} [{}]", boxed(rect)));
        }
        out.push(line);
    }
    out
}
