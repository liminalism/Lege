//! Styles, masters, templates, research records, and bibliography.
//!
//! A chapter stores the name of its template. Editing the template edits every
//! chapter that names it.

use crate::error::ModelError;
use crate::ids::{BlockId, ChapterId};
use crate::tree::{BlockKind, Book, Chapter, Position};

/// Paragraph style. Appearance lives here; the block stores the style name.
#[derive(Clone, Debug, PartialEq)]
pub struct ParagraphStyle {
    pub name: String,
    pub size_pt: f32,
    pub leading_pt: f32,
    pub first_indent_pt: f32,
    pub hyphenate: bool,
    pub small_caps: bool,
    pub oldstyle_figures: bool,
    pub drop_cap_lines: u8,
}

/// Page master: geometry, folio, running head.
#[derive(Clone, Debug, PartialEq)]
pub struct PageMaster {
    pub name: String,
    pub width_pt: f32,
    pub height_pt: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_inner: f32,
    pub margin_outer: f32,
    pub folio: bool,
    pub running_head: bool,
    pub facing: bool,
}

/// How a chapter opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChapterStart {
    NextPage,
    NextRecto,
}

/// Chapter template. Chapters reference it by name.
#[derive(Clone, Debug, PartialEq)]
pub struct ChapterTemplate {
    pub name: String,
    pub master: String,
    pub opener: String,
    pub title_style: String,
    pub body_style: String,
    pub first_paragraph_style: String,
    pub start: ChapterStart,
    pub show_opener_folio: bool,
}

/// A passage captured from a source PDF.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceNote {
    pub id: String,
    pub document: String,
    pub page: u32,
    /// `[left, top, right, bottom]` rectangles of the passage.
    pub rects: Vec<[f32; 4]>,
    pub passage: String,
    pub citation: String,
    pub annotation: String,
}

/// One bibliography record, also the CSL/BibTeX shape we import.
#[derive(Clone, Debug, PartialEq)]
pub struct BibliographyEntry {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub author: String,
    pub issued: String,
}

/// CSL JSON object we accept and emit.
#[derive(Clone, Debug, PartialEq)]
pub struct CslRecord {
    pub id: String,
    pub type_name: String,
    pub title: String,
    pub author: String,
    pub issued: String,
}

/// An index term anchored in a block.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexTerm {
    pub term: String,
    pub block: BlockId,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stylesheet {
    pub paragraphs: Vec<ParagraphStyle>,
    pub masters: Vec<PageMaster>,
    pub templates: Vec<ChapterTemplate>,
    pub sources: Vec<SourceNote>,
    pub bibliography: Vec<BibliographyEntry>,
    pub index: Vec<IndexTerm>,
    pub saved_position: Option<Position>,
}

impl Stylesheet {
    pub(crate) fn standard() -> Self {
        Self {
            paragraphs: vec![
                ParagraphStyle {
                    name: "Body".into(),
                    size_pt: 12.0,
                    leading_pt: 16.0,
                    first_indent_pt: 12.0,
                    hyphenate: true,
                    small_caps: false,
                    oldstyle_figures: false,
                    drop_cap_lines: 0,
                },
                ParagraphStyle {
                    name: "First Paragraph".into(),
                    size_pt: 12.0,
                    leading_pt: 16.0,
                    first_indent_pt: 0.0,
                    hyphenate: true,
                    small_caps: false,
                    oldstyle_figures: false,
                    drop_cap_lines: 2,
                },
                ParagraphStyle {
                    name: "Chapter Title".into(),
                    size_pt: 22.0,
                    leading_pt: 26.0,
                    first_indent_pt: 0.0,
                    hyphenate: false,
                    small_caps: true,
                    oldstyle_figures: false,
                    drop_cap_lines: 0,
                },
            ],
            masters: vec![
                PageMaster {
                    name: "Right Body".into(),
                    width_pt: 432.0,
                    height_pt: 648.0,
                    margin_top: 54.0,
                    margin_bottom: 54.0,
                    margin_inner: 54.0,
                    margin_outer: 36.0,
                    folio: true,
                    running_head: true,
                    facing: true,
                },
                PageMaster {
                    name: "Left Body".into(),
                    width_pt: 432.0,
                    height_pt: 648.0,
                    margin_top: 54.0,
                    margin_bottom: 54.0,
                    margin_inner: 54.0,
                    margin_outer: 36.0,
                    folio: true,
                    running_head: true,
                    facing: true,
                },
            ],
            templates: vec![ChapterTemplate {
                name: "Chapter".into(),
                master: "Right Body".into(),
                opener: "Chapter".into(),
                title_style: "Chapter Title".into(),
                body_style: "Body".into(),
                first_paragraph_style: "First Paragraph".into(),
                start: ChapterStart::NextRecto,
                show_opener_folio: false,
            }],
            sources: Vec::new(),
            bibliography: Vec::new(),
            index: Vec::new(),
            saved_position: None,
        }
    }
}

impl Book {
    pub(crate) fn stylesheet(&self) -> &Stylesheet {
        &self.stylesheet
    }

    pub(crate) fn stylesheet_mut(&mut self) -> &mut Stylesheet {
        &mut self.stylesheet
    }

    /// Paragraph styles in definition order.
    pub fn paragraph_styles(&self) -> &[ParagraphStyle] {
        &self.stylesheet.paragraphs
    }

    /// Replace the named paragraph style. Every block that uses the name
    /// picks the new metrics up on the next layout.
    pub fn set_paragraph_style(&mut self, style: ParagraphStyle) -> Result<(), ModelError> {
        let Some(slot) = self
            .stylesheet
            .paragraphs
            .iter_mut()
            .find(|existing| existing.name == style.name)
        else {
            self.stylesheet.paragraphs.push(style);
            return Ok(());
        };
        *slot = style;
        Ok(())
    }

    pub fn page_masters(&self) -> &[PageMaster] {
        &self.stylesheet.masters
    }

    /// Replace the named page master. The next layout uses its geometry.
    pub fn set_page_master(&mut self, master: PageMaster) -> Result<(), ModelError> {
        let Some(slot) = self
            .stylesheet
            .masters
            .iter_mut()
            .find(|existing| existing.name == master.name)
        else {
            self.stylesheet.masters.push(master);
            return Ok(());
        };
        *slot = master;
        Ok(())
    }

    pub fn chapter_templates(&self) -> &[ChapterTemplate] {
        &self.stylesheet.templates
    }

    /// Edit a chapter template. Every chapter that names it changes with it.
    pub fn set_chapter_template(&mut self, template: ChapterTemplate) -> Result<(), ModelError> {
        let Some(slot) = self
            .stylesheet
            .templates
            .iter_mut()
            .find(|existing| existing.name == template.name)
        else {
            return Err(ModelError::Inconsistent("unknown template"));
        };
        *slot = template;
        Ok(())
    }

    /// Opener text of every chapter, in book order, from its template.
    pub fn chapter_openers(&self) -> Vec<String> {
        let mut openers = Vec::new();
        let mut number = 1u32;
        for part in self.parts() {
            for chapter in part.chapters() {
                let opener = self
                    .stylesheet
                    .templates
                    .iter()
                    .find(|template| template.name == chapter.template())
                    .map(|template| template.opener.as_str())
                    .unwrap_or("Chapter");
                openers.push(format!("{opener} {number}"));
                number += 1;
            }
        }
        openers
    }

    /// Drag-reorder. `from` and `to` are chapter indexes in reading order.
    ///
    /// The moved chapter joins the part that owns the destination index.
    /// Every other chapter stays in its part.
    pub fn reorder_chapter(&mut self, from: usize, to: usize) -> Result<(), ModelError> {
        let count: usize = self.parts().iter().map(|part| part.chapters().len()).sum();
        if from >= count || to >= count {
            return Err(ModelError::Inconsistent("chapter index"));
        }
        let mut located: Vec<(usize, Chapter)> = Vec::new();
        for (part_index, part) in self.parts_mut().iter_mut().enumerate() {
            for chapter in part.chapters_mut().drain(..) {
                located.push((part_index, chapter));
            }
        }
        let (mut part_index, chapter) = located.remove(from);
        if let Some((dest_part, _)) = located.get(to) {
            part_index = *dest_part;
        }
        located.insert(to, (part_index, chapter));
        let part_count = self.parts().len();
        let mut buckets: Vec<Vec<Chapter>> = (0..part_count).map(|_| Vec::new()).collect();
        for (part, chapter) in located {
            if let Some(bucket) = buckets.get_mut(part) {
                bucket.push(chapter);
            }
        }
        for (part, chapters) in self.parts_mut().iter_mut().zip(buckets) {
            *part.chapters_mut() = chapters;
        }
        self.reindex();
        Ok(())
    }

    pub fn add_source(&mut self, source: SourceNote) {
        self.stylesheet.sources.push(source);
    }

    pub fn source(&self, id: &str) -> Option<&SourceNote> {
        self.stylesheet
            .sources
            .iter()
            .find(|source| source.id == id)
    }

    /// Insert the source's passage as a quotation and remember the citation key on the caret block.
    pub fn cite(&mut self, id: &str) -> Result<(), ModelError> {
        let source = self
            .source(id)
            .ok_or(ModelError::Inconsistent("unknown source"))?
            .clone();
        let block = self.selection().focus.block;
        self.insert(&source.passage)?;
        // The citation key is the source id. Runs stay direct marks; the block note
        // is the footnote slot, so the citation is recorded on the stylesheet index
        // of citations by storing it as an index term with the key prefixed.
        self.stylesheet.index.push(IndexTerm {
            term: format!("cite:{id}"),
            block,
        });
        let _ = source;
        Ok(())
    }

    pub fn citation_target(&self, id: &str) -> Option<&SourceNote> {
        self.source(id)
    }

    pub fn add_bibliography(&mut self, entry: BibliographyEntry) {
        self.stylesheet.bibliography.push(entry);
    }

    pub fn bibliography(&self) -> &[BibliographyEntry] {
        &self.stylesheet.bibliography
    }

    pub fn add_index_term(&mut self, term: impl Into<String>, block: BlockId) {
        self.stylesheet.index.push(IndexTerm {
            term: term.into(),
            block,
        });
    }

    pub fn index_terms(&self) -> &[IndexTerm] {
        &self.stylesheet.index
    }

    /// Terms whose block sits in `chapter`.
    pub fn sources_in_chapter(&self, chapter: ChapterId) -> Vec<&SourceNote> {
        let block_ids: Vec<BlockId> = self
            .parts()
            .iter()
            .flat_map(|part| part.chapters())
            .filter(|item| item.id() == chapter)
            .flat_map(|item| item.sections())
            .flat_map(|section| section.blocks())
            .map(|block| block.id())
            .collect();
        let cited: Vec<&str> = self
            .stylesheet
            .index
            .iter()
            .filter(|term| term.term.starts_with("cite:") && block_ids.contains(&term.block))
            .map(|term| term.term.trim_start_matches("cite:"))
            .collect();
        self.stylesheet
            .sources
            .iter()
            .filter(|source| cited.contains(&source.id.as_str()))
            .collect()
    }

    pub fn import_csl_json(&mut self, json: &str) -> Result<usize, ModelError> {
        let records = parse_csl(json).map_err(|_| ModelError::Inconsistent("csl json"))?;
        let count = records.len();
        for record in records {
            self.stylesheet.bibliography.push(BibliographyEntry {
                key: record.id,
                kind: record.type_name,
                title: record.title,
                author: record.author,
                issued: record.issued,
            });
        }
        Ok(count)
    }

    pub fn export_csl_json(&self) -> String {
        let mut out = String::from("[\n");
        for (index, entry) in self.stylesheet.bibliography.iter().enumerate() {
            if index > 0 {
                out.push_str(",\n");
            }
            out.push_str(&format!(
                "  {{\"id\":{},\"type\":{},\"title\":{},\"author\":{},\"issued\":{}}}",
                json_string(&entry.key),
                json_string(&entry.kind),
                json_string(&entry.title),
                json_string(&entry.author),
                json_string(&entry.issued),
            ));
        }
        out.push_str("\n]\n");
        out
    }

    pub fn import_bibtex(&mut self, bibtex: &str) -> Result<usize, ModelError> {
        let records = parse_bibtex(bibtex);
        let count = records.len();
        if count == 0 && bibtex.contains('@') {
            return Err(ModelError::Inconsistent("bibtex"));
        }
        self.stylesheet.bibliography.extend(records);
        Ok(count)
    }

    pub fn export_bibtex(&self) -> String {
        let mut out = String::new();
        for entry in &self.stylesheet.bibliography {
            out.push_str(&format!(
                "@{}{{{},\n  title = {},\n  author = {},\n  year = {}\n}}\n",
                if entry.kind.is_empty() {
                    "book"
                } else {
                    &entry.kind
                },
                entry.key,
                bib_brace(&entry.title),
                bib_brace(&entry.author),
                bib_brace(&entry.issued),
            ));
        }
        out
    }

    pub fn remember_position(&mut self) {
        self.stylesheet.saved_position = Some(self.selection().focus);
    }

    pub fn saved_position(&self) -> Option<Position> {
        self.stylesheet.saved_position
    }

    /// Block kind counts, used by export tests.
    pub fn blocks_of_kind(&self, kind: &BlockKind) -> usize {
        self.blocks().filter(|block| block.kind() == kind).count()
    }
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn bib_brace(value: &str) -> String {
    format!("{{{value}}}")
}

fn parse_csl(json: &str) -> Result<Vec<CslRecord>, ()> {
    let trimmed = json.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(());
    }
    let mut records = Vec::new();
    for object in split_objects(trimmed) {
        records.push(CslRecord {
            id: field(&object, "id")?,
            type_name: field(&object, "type")?,
            title: field(&object, "title")?,
            author: field(&object, "author").unwrap_or_default(),
            issued: field(&object, "issued").unwrap_or_default(),
        });
    }
    Ok(records)
}

fn split_objects(json: &str) -> Vec<String> {
    let mut objects = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (index, ch) in json.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0
                    && let Some(start) = start.take()
                {
                    objects.push(json[start..=index].to_string());
                }
            }
            _ => {}
        }
    }
    objects
}

fn field(object: &str, name: &str) -> Result<String, ()> {
    let needle = format!("\"{name}\"");
    let Some(at) = object.find(&needle) else {
        return Err(());
    };
    let rest = &object[at + needle.len()..];
    let Some(colon) = rest.find(':') else {
        return Err(());
    };
    let rest = rest[colon + 1..].trim_start();
    if !rest.starts_with('"') {
        return Err(());
    }
    let mut out = String::new();
    let mut chars = rest[1..].chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                out.push(escaped);
            }
        } else if ch == '"' {
            return Ok(out);
        } else {
            out.push(ch);
        }
    }
    Err(())
}

fn parse_bibtex(bibtex: &str) -> Vec<BibliographyEntry> {
    let mut entries = Vec::new();
    for chunk in bibtex.split('@').skip(1) {
        let Some((kind, rest)) = chunk.split_once('{') else {
            continue;
        };
        let Some((key, body)) = rest.split_once(',') else {
            continue;
        };
        let take = |name: &str| -> String {
            let needle = format!("{name} =");
            body.find(&needle)
                .and_then(|at| {
                    let after = body[at + needle.len()..].trim_start();
                    if let Some(braced) = after.strip_prefix('{') {
                        braced.split('}').next().map(str::to_string)
                    } else {
                        after
                            .split([',', '\n'])
                            .next()
                            .map(|s| s.trim().to_string())
                    }
                })
                .unwrap_or_default()
        };
        entries.push(BibliographyEntry {
            key: key.trim().to_string(),
            kind: kind.trim().to_ascii_lowercase(),
            title: take("title"),
            author: take("author"),
            issued: take("year"),
        });
    }
    entries
}
