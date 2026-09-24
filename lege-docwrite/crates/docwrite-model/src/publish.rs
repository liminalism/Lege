//! Styles, masters, templates, research records, and bibliography.
//!
//! A chapter stores the name of its template. Editing the template edits every
//! chapter that names it.

use crate::error::ModelError;
use crate::ids::{BlockId, ChapterId};
use crate::runs::RunMarks;
use crate::tree::{BlockKind, Book, Chapter, Position};

/// How a paragraph's lines sit in the measure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Align {
    /// Flush left, ragged right.
    Left,
    /// Both edges flush; the last line flush left.
    #[default]
    Justify,
    /// Centered.
    Center,
    /// Flush right.
    Right,
}

/// Paragraph style. Appearance lives here; a block's kind picks its style.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ParagraphStyle {
    /// Stable name, such as `Body` or `Block Quote`.
    pub name: String,
    /// Type size in points.
    pub size_pt: f32,
    /// Baseline-to-baseline distance in points.
    pub leading_pt: f32,
    /// First-line indent in points.
    pub first_indent_pt: f32,
    /// Hyphenate at line ends.
    pub hyphenate: bool,
    /// Set in small capitals (OpenType `smcp`).
    pub small_caps: bool,
    /// Old-style figures (OpenType `onum`).
    pub oldstyle_figures: bool,
    /// Lines a drop cap spans; zero or one for none.
    pub drop_cap_lines: u8,
    /// Alignment of the lines.
    pub align: Align,
    /// Indent from the left of the measure, every line, in points.
    pub left_indent_pt: f32,
    /// Indent from the right of the measure, every line, in points.
    pub right_indent_pt: f32,
    /// Space above the paragraph, in points.
    pub space_before_pt: f32,
    /// Space below the paragraph, in points.
    pub space_after_pt: f32,
    /// Set in the family's bold face.
    pub bold: bool,
    /// Set in the family's italic face; italic runs inside turn roman.
    pub italic: bool,
}

impl Default for ParagraphStyle {
    fn default() -> Self {
        Self {
            name: "Body".into(),
            size_pt: 12.0,
            leading_pt: 16.0,
            first_indent_pt: 12.0,
            hyphenate: true,
            small_caps: false,
            oldstyle_figures: false,
            drop_cap_lines: 0,
            align: Align::Justify,
            left_indent_pt: 0.0,
            right_indent_pt: 0.0,
            space_before_pt: 0.0,
            space_after_pt: 0.0,
            bold: false,
            italic: false,
        }
    }
}

/// Page master: geometry, folio, running head.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChapterStart {
    NextPage,
    NextRecto,
}

/// Chapter template. Chapters reference it by name.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexTerm {
    pub term: String,
    pub block: BlockId,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Stylesheet {
    pub paragraphs: Vec<ParagraphStyle>,
    pub masters: Vec<PageMaster>,
    pub templates: Vec<ChapterTemplate>,
    pub sources: Vec<SourceNote>,
    pub bibliography: Vec<BibliographyEntry>,
    pub index: Vec<IndexTerm>,
    pub saved_position: Option<Position>,
    /// Set a table of contents before the first chapter.
    #[serde(default)]
    pub contents: bool,
}

impl Stylesheet {
    pub(crate) fn standard() -> Self {
        Self {
            paragraphs: standard_styles(),
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
            contents: false,
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

    /// Define a new chapter template. Its name must be new.
    pub fn add_chapter_template(&mut self, template: ChapterTemplate) -> Result<(), ModelError> {
        if self
            .stylesheet
            .templates
            .iter()
            .any(|existing| existing.name == template.name)
        {
            return Err(ModelError::Inconsistent("template name already used"));
        }
        self.stylesheet.templates.push(template);
        Ok(())
    }

    /// Define a new page master. Its name must be new.
    pub fn add_page_master(&mut self, master: PageMaster) -> Result<(), ModelError> {
        if self
            .stylesheet
            .masters
            .iter()
            .any(|existing| existing.name == master.name)
        {
            return Err(ModelError::Inconsistent("master name already used"));
        }
        self.stylesheet.masters.push(master);
        Ok(())
    }

    /// Set chapter `chapter` from the template named `template`.
    pub fn apply_template(&mut self, chapter: ChapterId, template: &str) -> Result<(), ModelError> {
        if !self
            .stylesheet
            .templates
            .iter()
            .any(|existing| existing.name == template)
        {
            return Err(ModelError::Inconsistent("unknown template"));
        }
        let target = self
            .parts_mut()
            .iter_mut()
            .flat_map(|part| part.chapters_mut().iter_mut())
            .find(|candidate| candidate.id() == chapter)
            .ok_or(ModelError::UnknownChapter(chapter))?;
        target.set_template(template.to_string());
        Ok(())
    }

    /// Whether the book sets a table of contents before its first chapter.
    pub fn has_contents(&self) -> bool {
        self.stylesheet.contents
    }

    /// Turn the table of contents on or off.
    pub fn set_contents(&mut self, on: bool) {
        self.stylesheet.contents = on;
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

    /// Replace the source with the same id. Returns false when none has it.
    pub fn update_source(&mut self, source: SourceNote) -> bool {
        match self
            .stylesheet
            .sources
            .iter_mut()
            .find(|existing| existing.id == source.id)
        {
            Some(slot) => {
                *slot = source;
                true
            }
            None => false,
        }
    }

    /// Every source note, in the order they were captured.
    pub fn sources(&self) -> &[SourceNote] {
        &self.stylesheet.sources
    }

    pub fn source(&self, id: &str) -> Option<&SourceNote> {
        self.stylesheet
            .sources
            .iter()
            .find(|source| source.id == id)
    }

    /// Insert a citation of source `id` at the caret: its citation text
    /// (or "document, p. N" when it has none) in parentheses, marked with
    /// the source's id so it leads back to the source.
    pub fn cite(&mut self, id: &str) -> Result<(), ModelError> {
        let source = self
            .source(id)
            .ok_or(ModelError::Inconsistent("unknown source"))?
            .clone();
        let label = if source.citation.trim().is_empty() {
            let name = std::path::Path::new(&source.document)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| source.document.clone());
            format!("{name}, p. {}", source.page + 1)
        } else {
            source.citation.clone()
        };
        let marks = RunMarks {
            citation: Some(source.id.clone()),
            ..crate::runs::marks_before(
                self.block(self.selection().focus.block)?.runs(),
                self.selection().focus.offset,
            )
        };
        self.insert_with_marks(&format!("({label})"), marks)
    }

    /// Quote source `id` at the caret, in quotation marks, then cite it.
    pub fn quote(&mut self, id: &str) -> Result<(), ModelError> {
        let passage = self
            .source(id)
            .ok_or(ModelError::Inconsistent("unknown source"))?
            .passage
            .clone();
        self.insert(&format!("\u{201c}{}\u{201d} ", passage.trim()))?;
        self.cite(id)
    }

    /// The source a citation at the caret (or just before it) points to.
    pub fn citation_at_caret(&self) -> Option<&SourceNote> {
        let focus = self.selection().focus;
        let block = self.block(focus.block).ok()?;
        let id = block
            .runs()
            .iter()
            .find(|run| {
                run.start <= focus.offset && focus.offset <= run.end && run.marks.citation.is_some()
            })
            .and_then(|run| run.marks.citation.clone())?;
        self.source(&id)
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
        // Citations are marked runs; books from before that recorded them as
        // "cite:" index terms, which still count.
        let mut cited: Vec<String> = self
            .stylesheet
            .index
            .iter()
            .filter(|term| term.term.starts_with("cite:") && block_ids.contains(&term.block))
            .map(|term| term.term.trim_start_matches("cite:").to_string())
            .collect();
        for id in &block_ids {
            if let Ok(block) = self.block(*id) {
                cited.extend(
                    block
                        .runs()
                        .iter()
                        .filter_map(|run| run.marks.citation.clone()),
                );
            }
        }
        self.stylesheet
            .sources
            .iter()
            .filter(|source| cited.contains(&source.id))
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

/// The styles a new book starts with, one for each kind of block.
pub(crate) fn standard_styles() -> Vec<ParagraphStyle> {
    let body = ParagraphStyle::default();
    vec![
        body.clone(),
        ParagraphStyle {
            name: "First Paragraph".into(),
            first_indent_pt: 0.0,
            drop_cap_lines: 2,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Chapter Title".into(),
            size_pt: 22.0,
            leading_pt: 26.0,
            first_indent_pt: 0.0,
            hyphenate: false,
            small_caps: true,
            align: Align::Left,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Subhead".into(),
            size_pt: 13.0,
            leading_pt: 16.0,
            first_indent_pt: 0.0,
            hyphenate: false,
            align: Align::Left,
            space_before_pt: 16.0,
            space_after_pt: 4.0,
            bold: true,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Block Quote".into(),
            size_pt: 11.0,
            leading_pt: 14.0,
            first_indent_pt: 0.0,
            left_indent_pt: 24.0,
            right_indent_pt: 24.0,
            space_before_pt: 8.0,
            space_after_pt: 8.0,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Epigraph".into(),
            size_pt: 11.0,
            leading_pt: 14.0,
            first_indent_pt: 0.0,
            hyphenate: false,
            align: Align::Left,
            left_indent_pt: 96.0,
            space_after_pt: 16.0,
            italic: true,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Verse".into(),
            first_indent_pt: 0.0,
            hyphenate: false,
            align: Align::Left,
            left_indent_pt: 36.0,
            space_before_pt: 8.0,
            space_after_pt: 8.0,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Caption".into(),
            size_pt: 10.0,
            leading_pt: 13.0,
            first_indent_pt: 0.0,
            hyphenate: false,
            align: Align::Center,
            space_after_pt: 8.0,
            italic: true,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Scene Break".into(),
            first_indent_pt: 0.0,
            hyphenate: false,
            align: Align::Center,
            space_before_pt: 8.0,
            space_after_pt: 8.0,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Contents Entry".into(),
            first_indent_pt: 0.0,
            hyphenate: false,
            space_after_pt: 4.0,
            ..body.clone()
        },
        ParagraphStyle {
            name: "Bibliography Entry".into(),
            size_pt: 11.0,
            leading_pt: 14.0,
            first_indent_pt: -18.0,
            left_indent_pt: 18.0,
            align: Align::Left,
            ..body
        },
    ]
}
