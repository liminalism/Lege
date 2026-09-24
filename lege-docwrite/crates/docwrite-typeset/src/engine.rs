//! Shaping, line breaking and incremental pagination.

use std::collections::HashMap;
use std::sync::Arc;

use harfrust::{Feature, FontRef, GlyphBuffer, ShapeOptions, ShaperData, Tag, UnicodeBuffer};
use hyphenation::{Hyphenator, Language, Load, Standard};
use unicode_linebreak::{BreakOpportunity, linebreaks};

use crate::error::TypesetError;

/// A loaded face. Shaping scales font units by `size / units_per_em`.
pub struct Face {
    data: Vec<u8>,
    upem: i32,
    shaper: ShaperData,
}

impl Face {
    /// Parse `bytes` as a font collection index 0.
    pub fn parse(bytes: Vec<u8>) -> Result<Self, TypesetError> {
        let font =
            FontRef::from_index(&bytes, 0).map_err(|err| TypesetError::Font(err.to_string()))?;
        let shaper = ShaperData::new(&font);
        let upem = shaper.shaper(&font).build().units_per_em();
        if upem <= 0 {
            return Err(TypesetError::Font("units per em is zero".into()));
        }
        Ok(Self {
            data: bytes,
            upem,
            shaper,
        })
    }

    /// Shape `text` at `size_px`. `features` are OpenType tags such as `smcp`.
    pub fn shape(
        &self,
        text: &str,
        size_px: f32,
        features: &[Feature],
    ) -> Result<Vec<Glyph>, TypesetError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let font = FontRef::from_index(&self.data, 0)
            .map_err(|err| TypesetError::Font(err.to_string()))?;
        let shaper = self.shaper.shaper(&font).build();
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        let glyphs = shaper.shape(buffer, ShapeOptions::new().features(features));
        Ok(scale_glyphs(&glyphs, size_px, self.upem))
    }

    /// A second face over the same font bytes. Pagination owns one; the
    /// window keeps the other so a frame can rasterize without rebuilding
    /// the shaper from scratch on the next key.
    pub fn duplicate(&self) -> Result<Self, TypesetError> {
        Self::parse(self.data.clone())
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.data
    }

    pub(crate) fn upem(&self) -> i32 {
        self.upem
    }
}

fn scale_glyphs(glyphs: &GlyphBuffer, size_px: f32, upem: i32) -> Vec<Glyph> {
    let scale = size_px / upem as f32;
    let infos = glyphs.glyph_infos();
    let positions = glyphs.glyph_positions();
    infos
        .iter()
        .zip(positions)
        .map(|(info, pos)| Glyph {
            id: info.glyph_id as u16,
            cluster: info.cluster,
            x_advance: pos.x_advance as f32 * scale,
            x_offset: pos.x_offset as f32 * scale,
            y_offset: pos.y_offset as f32 * scale,
            em: size_px,
        })
        .collect()
}

/// One shaped line, tied back to the paragraph it was broken from.
#[derive(Clone, Debug)]
pub struct PaintedLine {
    /// Index into the paragraph list passed to [`Document::new`].
    pub paragraph: usize,
    /// Glyphs in visual order.
    pub glyphs: Vec<Glyph>,
    /// First-line indent in geometry units. Other lines are zero.
    pub indent: f32,
}

/// One shaped glyph in pixels.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub id: u16,
    pub cluster: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    /// Em size used to draw this glyph. A drop cap is larger than the body.
    pub em: f32,
}

/// OpenType features requested by a paragraph style.
pub fn features_for(small_caps: bool, oldstyle: bool) -> Vec<Feature> {
    let mut features = vec![
        Feature::new(Tag::new(b"kern"), 1, ..),
        Feature::new(Tag::new(b"liga"), 1, ..),
    ];
    if small_caps {
        features.push(Feature::new(Tag::new(b"smcp"), 1, ..));
    }
    if oldstyle {
        features.push(Feature::new(Tag::new(b"onum"), 1, ..));
    }
    features
}

/// Page and type size. Lengths are in pixels (1px = 1pt at 72dpi for tests).
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub page_width: f32,
    pub page_height: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_inner: f32,
    pub margin_outer: f32,
    pub font_size: f32,
    pub leading: f32,
}

impl Geometry {
    pub fn content_width(&self) -> f32 {
        (self.page_width - self.margin_inner - self.margin_outer).max(1.0)
    }

    pub fn content_height(&self) -> f32 {
        (self.page_height - self.margin_top - self.margin_bottom).max(self.leading)
    }

    /// One short line per page. Used to build a large book quickly.
    pub fn one_line_pages() -> Self {
        Self {
            page_width: 400.0,
            page_height: 40.0,
            margin_top: 4.0,
            margin_bottom: 4.0,
            margin_inner: 12.0,
            margin_outer: 12.0,
            font_size: 16.0,
            leading: 32.0,
        }
    }
}

/// How a paragraph participates in pagination.
#[derive(Clone, Debug, PartialEq)]
pub struct ParagraphStyle {
    pub hyphenate: bool,
    pub keep_with_next: bool,
    pub keep_lines: bool,
    pub widow_orphan: bool,
    pub drop_cap_lines: u8,
    pub small_caps: bool,
    pub oldstyle_figures: bool,
    pub space_before: f32,
    pub space_after: f32,
    /// Zero uses the document geometry.
    pub font_size: f32,
    /// Zero uses the document geometry.
    pub leading: f32,
    /// First-line indent. Later lines use the full measure.
    pub first_indent: f32,
    /// Stable name such as `Body` or `Chapter Title`. Empty uses the geometry's size.
    pub name: String,
}

impl Default for ParagraphStyle {
    fn default() -> Self {
        Self {
            hyphenate: false,
            keep_with_next: false,
            keep_lines: false,
            widow_orphan: false,
            drop_cap_lines: 0,
            small_caps: false,
            oldstyle_figures: false,
            space_before: 0.0,
            space_after: 0.0,
            font_size: 0.0,
            leading: 0.0,
            first_indent: 0.0,
            name: "Body".into(),
        }
    }
}

/// A paragraph the paginator can see. `id` is the model's block id as a number
/// so this crate does not need to own the model type.
#[derive(Clone, Debug, PartialEq)]
pub struct Paragraph {
    pub id: u64,
    pub text: String,
    pub style: ParagraphStyle,
    /// Footnote or endnote body anchored to this paragraph, if any.
    pub note: Option<String>,
    pub note_is_endnote: bool,
}

/// A position in the line stream: which paragraph, which of its lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub paragraph: usize,
    pub line: u32,
}

#[derive(Clone, Debug)]
struct FlowLine {
    paragraph: usize,
    line: u32,
    height: f32,
    is_first: bool,
    is_last: bool,
    keep_with_next: bool,
    keep_together: bool,
    widow_orphan: bool,
    /// Glyph ids of this line, for the atlas and for PDF.
    /// Shared so a repeated paragraph does not allocate a new buffer per page.
    glyphs: Arc<Vec<Glyph>>,
    /// Advance width.
    width: f32,
    text: String,
    /// Drop-cap glyph is larger than the body size when set.
    drop_cap: bool,
    /// First-line indent in the same units as the geometry.
    indent: f32,
}

#[derive(Clone, Debug)]
struct Page {
    start: Cursor,
    lines: Vec<FlowLine>,
    /// Footnote text placed on this page, possibly a slice of a longer note.
    footnotes: Vec<String>,
    /// Tail of a note that did not fit, carried onto the next page.
    note_carry: Option<String>,
    /// Note bodies that start on this page but could not be placed yet.
    note_waiting: Vec<String>,
    /// A verso left empty so the next chapter can open on a recto.
    blank: bool,
    folio: Option<String>,
    running_head: Option<String>,
    /// Left inset of the text block. Verso pages mirror inner and outer.
    content_inset: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NoteFlow {
    carry: Option<String>,
    waiting: Vec<String>,
}

/// What the next page is, before any line is placed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PageStep {
    /// Empty verso. `outgoing` is the incoming flow, notes unconsumed.
    Blank { outgoing: NoteFlow },
    /// Body is finished. Place whatever notes remain.
    NotesOnly,
    /// Place body lines, then notes.
    Fill,
}

/// A blank verso is required when the next page is even and the cursor is the
/// first line of a recto chapter. A pending note does not cancel that blank;
/// the blank carries the same flow forward.
fn page_decision(built: usize, recto_opener: bool, at_end: bool, flow: &NoteFlow) -> PageStep {
    let next_is_verso = (built + 1).is_multiple_of(2);
    if recto_opener && next_is_verso {
        PageStep::Blank {
            outgoing: flow.clone(),
        }
    } else if at_end {
        PageStep::NotesOnly
    } else {
        PageStep::Fill
    }
}

impl NoteFlow {
    fn pending(&self) -> bool {
        self.carry.is_some() || !self.waiting.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutHints {
    pub recto_at: Vec<bool>,
    pub heads: Vec<String>,
    pub folio: bool,
    pub hide_opener_folio: bool,
    pub running_head: bool,
    pub facing: bool,
}

impl Default for LayoutHints {
    fn default() -> Self {
        Self {
            recto_at: Vec::new(),
            heads: Vec::new(),
            folio: false,
            hide_opener_folio: false,
            running_head: false,
            facing: false,
        }
    }
}

/// What an edit did to pagination. Page numbers are 1-based.
#[derive(Clone, Debug)]
pub struct EditReport {
    pub pages_laid_out: Vec<u32>,
    pub page_count: u32,
    /// Paragraphs shaped and line-broken again by this edit.
    pub paragraphs_shaped: usize,
}

/// A shaped, paginated document.
pub struct Document {
    face: Face,
    geometry: Geometry,
    paragraphs: Vec<Paragraph>,
    /// Cached lines per paragraph index.
    lines: Vec<Vec<FlowLine>>,
    pages: Vec<Page>,
    hyphenator: Option<Standard>,
    hints: LayoutHints,
}

impl Document {
    pub fn new(
        face: Face,
        geometry: Geometry,
        paragraphs: Vec<Paragraph>,
    ) -> Result<Self, TypesetError> {
        Self::new_with(face, geometry, paragraphs, LayoutHints::default())
    }

    pub(crate) fn new_with(
        face: Face,
        geometry: Geometry,
        paragraphs: Vec<Paragraph>,
        hints: LayoutHints,
    ) -> Result<Self, TypesetError> {
        let hyphenator = Standard::from_embedded(Language::EnglishUS).ok();
        let mut doc = Self {
            face,
            geometry,
            paragraphs,
            lines: Vec::new(),
            pages: Vec::new(),
            hyphenator,
            hints,
        };
        doc.reshape_all()?;
        doc.paginate_all();
        Ok(doc)
    }

    /// Page number printed when the master asks for a folio. 1-based `page`.
    pub fn page_folio(&self, page: u32) -> Option<String> {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .and_then(|page| page.folio.clone())
    }

    /// Left inset of 1-based `page`. Facing versos use the outer margin.
    pub fn page_content_inset(&self, page: u32) -> f32 {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .map(|page| page.content_inset)
            .unwrap_or(self.geometry.margin_inner)
    }

    /// Running head printed when the master asks for one.
    pub fn page_running_head(&self, page: u32) -> Option<String> {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .and_then(|page| page.running_head.clone())
    }

    /// True when `page` was inserted so the next chapter could open on a recto.
    pub fn is_blank_page(&self, page: u32) -> bool {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .is_some_and(|page| page.blank)
    }

    /// First-line indents of 1-based `page`, in geometry units.
    pub fn line_indents(&self, page: u32) -> Vec<f32> {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .map(|page| page.lines.iter().map(|line| line.indent).collect())
            .unwrap_or_default()
    }

    /// Em size of the first glyph of paragraph `index`.
    pub fn paragraph_em(&self, index: usize) -> Option<f32> {
        self.lines
            .get(index)?
            .iter()
            .find_map(|line| line.glyphs.first().map(|glyph| glyph.em))
    }

    /// The 1-based page showing byte `byte` of paragraph index `paragraph`
    /// (its index in reading order), or `None` past the end of the book.
    pub fn page_of(&self, paragraph: usize, byte: usize) -> Option<u32> {
        let first = self
            .pages
            .partition_point(|page| page.start.paragraph < paragraph)
            .saturating_sub(1);
        let mut found = None;
        for (index, page) in self.pages.iter().enumerate().skip(first) {
            if page.start.paragraph > paragraph {
                break;
            }
            for line in page.lines.iter().filter(|line| line.paragraph == paragraph) {
                let starts_at = line
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.cluster as usize)
                    .min()
                    .unwrap_or(0);
                if line.is_first || starts_at <= byte {
                    found = Some(index as u32 + 1);
                }
            }
        }
        found
    }

    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    pub fn face(&self) -> &Face {
        &self.face
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Lines of 1-based `page`, as their paragraph text slices.
    pub fn page_texts(&self, page: u32) -> Vec<String> {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .map(|page| page.lines.iter().map(|line| line.text.clone()).collect())
            .unwrap_or_default()
    }

    pub fn page_footnotes(&self, page: u32) -> Vec<String> {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .map(|page| page.footnotes.clone())
            .unwrap_or_default()
    }

    /// Glyph ids on the first line of `page`, for feature and drop-cap checks.
    pub fn page_glyph_ids(&self, page: u32) -> Vec<u16> {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .and_then(|page| page.lines.first())
            .map(|line| line.glyphs.iter().map(|glyph| glyph.id).collect())
            .unwrap_or_default()
    }

    /// Shaped lines of 1-based `page`, in reading order.
    pub fn page_line_glyphs(&self, page: u32) -> Vec<Vec<Glyph>> {
        self.page_painted_lines(page)
            .into_iter()
            .map(|line| line.glyphs)
            .collect()
    }

    /// Shaped lines of 1-based `page`, with the paragraph index each line came from.
    pub fn page_painted_lines(&self, page: u32) -> Vec<PaintedLine> {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .map(|page| {
                page.lines
                    .iter()
                    .map(|line| PaintedLine {
                        paragraph: line.paragraph,
                        glyphs: line.glyphs.as_ref().clone(),
                        indent: line.indent,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Em size of the first drop-cap glyph, when a paragraph asked for one.
    pub fn drop_cap_em(&self) -> Option<f32> {
        self.pages
            .iter()
            .flat_map(|page| page.lines.iter())
            .find_map(|line| {
                if line.drop_cap {
                    line.glyphs.first().map(|glyph| glyph.em)
                } else {
                    None
                }
            })
    }

    /// A page opens on the last line of a paragraph that started earlier.
    pub fn opens_with_widow(&self) -> bool {
        self.pages.iter().any(|page| {
            page.lines
                .first()
                .is_some_and(|line| line.is_last && !line.is_first)
        })
    }

    /// A page that is not the last ends on the first line of a paragraph
    /// that still has more lines.
    pub fn ends_with_orphan(&self) -> bool {
        let last = self.pages.len().saturating_sub(1);
        self.pages.iter().enumerate().any(|(index, page)| {
            index < last
                && page
                    .lines
                    .last()
                    .is_some_and(|line| line.is_first && !line.is_last)
        })
    }

    /// Widow, orphan, keep-with-next, and keep-lines breaks the styles asked for.
    pub fn rule_violations(&self) -> Vec<String> {
        let mut violations = Vec::new();
        let last = self.pages.len().saturating_sub(1);
        for (index, page) in self.pages.iter().enumerate() {
            let page_no = index as u32 + 1;
            if let Some(line) = page.lines.first() {
                let asked = self
                    .paragraphs
                    .get(line.paragraph)
                    .is_some_and(|paragraph| paragraph.style.widow_orphan);
                if asked && line.is_last && !line.is_first {
                    violations.push(format!("widow at the top of page {page_no}"));
                }
            }
            if let Some(line) = page.lines.last() {
                let asked = self
                    .paragraphs
                    .get(line.paragraph)
                    .is_some_and(|paragraph| paragraph.style.widow_orphan);
                if asked && line.is_first && !line.is_last && index < last {
                    violations.push(format!("orphan at the bottom of page {page_no}"));
                }
                if line.keep_with_next && index < last {
                    violations.push(format!(
                        "keep-with-next stranded at the end of page {page_no}"
                    ));
                }
            }
        }
        let mut home = vec![None; self.paragraphs.len()];
        for (index, page) in self.pages.iter().enumerate() {
            for line in &page.lines {
                if !line.keep_together {
                    continue;
                }
                let Some(slot) = home.get_mut(line.paragraph) else {
                    continue;
                };
                if let Some(first) = *slot {
                    if first != index {
                        violations.push(format!(
                            "keep-lines split paragraph {} across pages",
                            line.paragraph
                        ));
                    }
                } else {
                    *slot = Some(index);
                }
            }
        }
        violations.sort();
        violations.dedup();
        violations
    }

    pub fn first_line_is_drop_cap(&self, page: u32) -> bool {
        self.pages
            .get(page.saturating_sub(1) as usize)
            .and_then(|page| page.lines.first())
            .is_some_and(|line| line.drop_cap)
    }

    /// Insert `extra` at the start of the paragraph that opens 1-based `page`.
    /// Returns which pages were actually rebuilt.
    pub fn edit_page(&mut self, page: u32, extra: &str) -> Result<EditReport, TypesetError> {
        let index = page.saturating_sub(1) as usize;
        let Some(paragraph) = self.pages.get(index).map(|page| page.start.paragraph) else {
            return Err(TypesetError::UnknownBlock(page as u64));
        };
        self.paragraphs[paragraph].text.insert_str(0, extra);
        self.reshape_one(paragraph)?;
        let laid = self.repaginate_from(index.saturating_sub(1), None, paragraph);
        Ok(EditReport {
            pages_laid_out: laid,
            page_count: self.page_count(),
            paragraphs_shaped: 1,
        })
    }

    /// Bring the layout up to date with `paragraphs`, the book in reading
    /// order as the paginator sees it. Paragraphs are matched by `id`; one
    /// whose text, style and note are unchanged keeps its shaped lines, even
    /// if it moved. Pagination restarts one page before the first change and
    /// stops where the old page breaks line up again.
    pub(crate) fn apply(
        &mut self,
        geometry: Geometry,
        paragraphs: Vec<Paragraph>,
        hints: LayoutHints,
    ) -> Result<EditReport, TypesetError> {
        let same_geometry = geometry_eq(&self.geometry, &geometry);
        let same_page_rules = self.hints.folio == hints.folio
            && self.hints.hide_opener_folio == hints.hide_opener_folio
            && self.hints.running_head == hints.running_head
            && self.hints.facing == hints.facing;
        if !same_geometry || !same_page_rules || self.pages.is_empty() {
            self.geometry = geometry;
            self.paragraphs = paragraphs;
            self.hints = hints;
            self.reshape_all()?;
            self.paginate_all();
            return Ok(EditReport {
                pages_laid_out: (1..=self.page_count()).collect(),
                page_count: self.page_count(),
                paragraphs_shaped: self.paragraphs.len(),
            });
        }
        let old_index: HashMap<u64, usize> = self
            .paragraphs
            .iter()
            .enumerate()
            .map(|(index, paragraph)| (paragraph.id, index))
            .collect();
        let mut remap = vec![None; self.paragraphs.len()];
        let mut old_lines = std::mem::take(&mut self.lines);
        let old_paragraphs = std::mem::replace(&mut self.paragraphs, paragraphs);
        let mut lines = Vec::with_capacity(self.paragraphs.len());
        let mut first_change: Option<usize> = None;
        let mut last_change = 0;
        let mut shaped = 0;
        let mut mark = |index: usize, first: &mut Option<usize>| {
            *first = Some(first.map_or(index, |first| first.min(index)));
            last_change = last_change.max(index);
        };
        // Old index of the previous paragraph, whether kept or not. A kept
        // paragraph is in place only when it directly follows the same
        // predecessor it followed before; otherwise its old page breaks
        // cannot be trusted.
        let mut previous_old: Option<usize> = None;
        let mut previous_new_was_kept = true;
        for index in 0..self.paragraphs.len() {
            let reuse = old_index
                .get(&self.paragraphs[index].id)
                .copied()
                .filter(|old| {
                    old_paragraphs.get(*old) == self.paragraphs.get(index)
                        && self.hints.recto_at.get(*old) == hints.recto_at.get(index)
                });
            match reuse {
                Some(old) => {
                    remap[old] = Some(index);
                    let in_place = previous_new_was_kept
                        && match previous_old {
                            Some(previous) => old == previous + 1,
                            None => old == 0,
                        };
                    // A new paragraph just before this one already marked the
                    // change; this one only needs to follow it.
                    let after_insert = !previous_new_was_kept
                        && previous_old.map_or(old == 0, |previous| old == previous + 1);
                    if !in_place && !after_insert {
                        mark(index, &mut first_change);
                    }
                    let mut kept = std::mem::take(&mut old_lines[old]);
                    if old != index {
                        for line in &mut kept {
                            line.paragraph = index;
                        }
                    }
                    lines.push(kept);
                    previous_old = Some(old);
                    previous_new_was_kept = true;
                }
                None => {
                    mark(index, &mut first_change);
                    lines.push(Vec::new());
                    previous_new_was_kept = false;
                }
            }
        }
        // Old paragraphs that no longer exist change the layout at the place
        // they used to occupy: right after their surviving predecessor.
        let mut place = 0;
        for old in 0..old_paragraphs.len() {
            match remap[old] {
                Some(index) => place = index + 1,
                None => mark(place, &mut first_change),
            }
        }
        self.hints = hints;
        self.lines = lines;
        for index in 0..self.paragraphs.len() {
            if self.lines[index].is_empty() {
                self.lines[index] = self.shape_paragraph(index)?;
                shaped += 1;
            }
        }
        self.link_images();
        let Some(first_change) = first_change else {
            self.dress_pages();
            return Ok(EditReport {
                pages_laid_out: Vec::new(),
                page_count: self.page_count(),
                paragraphs_shaped: 0,
            });
        };
        let first_page = self
            .pages
            .iter()
            .rposition(|page| {
                page.start.paragraph < first_change
                    || page.start.paragraph == first_change && page.start.line == 0
            })
            .unwrap_or(0);
        let laid = self.repaginate_from(first_page.saturating_sub(1), Some(&remap), last_change);
        Ok(EditReport {
            pages_laid_out: laid,
            page_count: self.page_count(),
            paragraphs_shaped: shaped,
        })
    }

    /// Replace every paragraph's style fields that tests name, then lay out
    /// from scratch. Returns the fresh page texts so a caller can compare.
    pub fn restyle_body(
        &mut self,
        font_size: f32,
        leading: f32,
    ) -> Result<Vec<Vec<String>>, TypesetError> {
        self.geometry.font_size = font_size;
        self.geometry.leading = leading;
        self.reshape_all()?;
        self.paginate_all();
        Ok((1..=self.page_count())
            .map(|page| self.page_texts(page))
            .collect())
    }

    pub fn paragraphs(&self) -> &[Paragraph] {
        &self.paragraphs
    }

    /// Chapter-title paragraphs and the 1-based page each one landed on.
    pub fn contents(&self) -> Vec<(String, u32)> {
        let mut entries = Vec::new();
        for (index, page) in self.pages.iter().enumerate() {
            for line in &page.lines {
                if line.is_first
                    && self
                        .paragraphs
                        .get(line.paragraph)
                        .is_some_and(|paragraph| paragraph.style.name == "Chapter Title")
                {
                    entries.push((line.text.clone(), (index as u32) + 1));
                }
            }
        }
        entries
    }

    /// Insert `count` body paragraphs before paragraph `before`, then repaginate.
    pub fn insert_paragraphs_before(
        &mut self,
        before: usize,
        count: u32,
        text: &str,
    ) -> Result<(), TypesetError> {
        let style = ParagraphStyle::default();
        for offset in 0..count {
            self.paragraphs.insert(
                before + offset as usize,
                Paragraph {
                    id: 10_000 + offset as u64,
                    text: text.to_string(),
                    style: style.clone(),
                    note: None,
                    note_is_endnote: false,
                },
            );
        }
        self.reshape_all()?;
        self.paginate_all();
        Ok(())
    }

    fn reshape_all(&mut self) -> Result<(), TypesetError> {
        self.lines.clear();
        self.lines.reserve(self.paragraphs.len());
        for index in 0..self.paragraphs.len() {
            if index > 0 && same_shape(&self.paragraphs[index - 1], &self.paragraphs[index]) {
                let mut lines = self.lines[index - 1].clone();
                for line in &mut lines {
                    line.paragraph = index;
                }
                self.lines.push(lines);
                continue;
            }
            self.lines.push(self.shape_paragraph(index)?);
        }
        self.link_images();
        Ok(())
    }

    fn link_images(&mut self) {
        if !self
            .paragraphs
            .iter()
            .any(|paragraph| paragraph.style.name == "Image")
        {
            return;
        }
        for index in 0..self.paragraphs.len().saturating_sub(1) {
            let image = self.paragraphs[index].style.name == "Image";
            let caption = self.paragraphs[index + 1].style.name == "Caption";
            if image && caption {
                if let Some(last) = self.lines.get_mut(index).and_then(|lines| lines.last_mut()) {
                    last.keep_with_next = true;
                }
            }
        }
    }

    fn reshape_one(&mut self, index: usize) -> Result<(), TypesetError> {
        let lines = self.shape_paragraph(index)?;
        self.lines[index] = lines;
        self.link_images();
        Ok(())
    }

    fn shape_paragraph(&self, index: usize) -> Result<Vec<FlowLine>, TypesetError> {
        let paragraph = &self.paragraphs[index];
        let features = features_for(paragraph.style.small_caps, paragraph.style.oldstyle_figures);
        let size = if paragraph.style.font_size > 0.0 {
            paragraph.style.font_size
        } else {
            self.geometry.font_size
        };
        let leading = if paragraph.style.leading > 0.0 {
            paragraph.style.leading
        } else {
            self.geometry.leading
        };
        let indent = paragraph.style.first_indent.max(0.0);
        let glyphs = self.face.shape(&paragraph.text, size, &features)?;
        let mut broken = break_lines(
            &paragraph.text,
            &glyphs,
            self.geometry.content_width(),
            indent,
            paragraph.style.hyphenate,
            self.hyphenator.as_ref(),
        );
        if broken.is_empty() {
            broken.push(Broken {
                glyphs: Vec::new(),
                width: 0.0,
                text: String::new(),
            });
        }
        let last = broken.len().saturating_sub(1) as u32;
        let keep_together = paragraph.style.keep_lines && broken.len() <= 3;
        let drop = paragraph.style.drop_cap_lines > 0;
        let factor = (paragraph.style.drop_cap_lines as f32).max(1.0);
        Ok(broken
            .into_iter()
            .enumerate()
            .map(|(line_index, mut line)| {
                let mut height = leading
                    + if line_index == 0 {
                        paragraph.style.space_before
                    } else {
                        0.0
                    }
                    + if line_index as u32 == last {
                        paragraph.style.space_after
                    } else {
                        0.0
                    };
                let drop_cap = drop && line_index == 0 && !line.glyphs.is_empty();
                if drop_cap {
                    if let Some(glyph) = line.glyphs.first_mut() {
                        glyph.x_advance *= factor;
                        glyph.em *= factor;
                    }
                    height += leading * (factor - 1.0);
                }
                FlowLine {
                    paragraph: index,
                    line: line_index as u32,
                    height,
                    is_first: line_index == 0,
                    is_last: line_index as u32 == last,
                    keep_with_next: paragraph.style.keep_with_next && line_index as u32 == last,
                    keep_together,
                    widow_orphan: paragraph.style.widow_orphan,
                    drop_cap,
                    glyphs: Arc::new(line.glyphs),
                    width: line.width,
                    text: line.text,
                    indent: if line_index == 0 { indent } else { 0.0 },
                }
            })
            .collect())
    }

    fn paginate_all(&mut self) {
        self.pages.clear();
        let mut cursor = Cursor {
            paragraph: 0,
            line: 0,
        };
        let mut flow = NoteFlow::default();
        if self.lines.is_empty() {
            return;
        }
        if self.each_paragraph_is_one_page() {
            self.pages = self
                .lines
                .iter()
                .enumerate()
                .map(|(index, lines)| Page {
                    start: Cursor {
                        paragraph: index,
                        line: 0,
                    },
                    lines: lines.clone(),
                    footnotes: Vec::new(),
                    note_carry: None,
                    blank: false,
                    folio: None,
                    running_head: None,
                    note_waiting: Vec::new(),
                    content_inset: 0.0,
                })
                .collect();
            self.dress_pages();
            return;
        }
        while !self.at_end(cursor) || flow.pending() {
            let built = self.pages.len();
            let (page, next, filled) = self.take_page(built, cursor, flow);
            flow = next;
            let page = if filled {
                avoid_widow(&mut self.pages, page)
            } else {
                page
            };
            if filled {
                cursor = next_cursor(&page);
            }
            self.pages.push(page);
            if self.pages.len() > self.paragraphs.len().saturating_mul(8).max(8) {
                break;
            }
        }
        self.dress_pages();
    }

    /// Rebuild from `start_page` until a break matches the cached layout.
    ///
    /// `remap` maps each old paragraph index to its new index (`None` when
    /// the paragraph is gone); `None` means indices did not move. Paragraphs
    /// after `last_change` are unchanged, so once a page break lands after
    /// it, at the same page index and with no notes in flight, the old
    /// pages from there on are moved over with their indices fixed up.
    /// Returns the 1-based pages that were laid out again.
    fn repaginate_from(
        &mut self,
        start_page: usize,
        remap: Option<&[Option<usize>]>,
        last_change: usize,
    ) -> Vec<u32> {
        if self.pages.is_empty() {
            self.paginate_all();
            return (1..=self.page_count()).collect();
        }
        let start_page = start_page.min(self.pages.len().saturating_sub(1));
        let mut old = std::mem::take(&mut self.pages);
        let mut old_tail = old.split_off(start_page);
        let mut pages = old;
        let map = |paragraph: usize| match remap {
            Some(remap) => remap.get(paragraph).copied().flatten(),
            None => Some(paragraph),
        };
        let cached: Vec<Option<Cursor>> = old_tail
            .iter()
            .map(|page| {
                map(page.start.paragraph).map(|paragraph| Cursor {
                    paragraph,
                    line: page.start.line,
                })
            })
            .collect();
        // The kept pages end before the first change, so the line after
        // them is where layout resumes. The old start of `start_page` is not:
        // its paragraph may itself have moved.
        let mut cursor = pages.last().map(next_cursor).unwrap_or(Cursor {
            paragraph: 0,
            line: 0,
        });
        let mut flow = pages
            .last()
            .map(|previous| NoteFlow {
                carry: previous.note_carry.clone(),
                waiting: previous.note_waiting.clone(),
            })
            .unwrap_or_default();
        let mut rebuilt = Vec::new();
        loop {
            if self.at_end(cursor) && !flow.pending() {
                break;
            }
            rebuilt.push((pages.len() + 1) as u32);
            let built = pages.len();
            let (page, next, filled) = self.take_page(built, cursor, flow);
            flow = next;
            let page = if filled {
                avoid_widow(&mut pages, page)
            } else {
                page
            };
            let following = filled.then(|| next_cursor(&page));
            pages.push(page);
            if let Some(following) = following {
                let at = pages.len() - start_page;
                let old_quiet = at
                    .checked_sub(1)
                    .and_then(|index| old_tail.get(index))
                    .is_some_and(|page| page.note_carry.is_none() && page.note_waiting.is_empty());
                if !flow.pending()
                    && old_quiet
                    && following.paragraph > last_change
                    && cached.get(at).copied().flatten() == Some(following)
                {
                    for mut page in old_tail.drain(at..) {
                        if remap.is_some() {
                            let Some(paragraph) = map(page.start.paragraph) else {
                                break;
                            };
                            page.start.paragraph = paragraph;
                            for line in &mut page.lines {
                                if let Some(paragraph) = map(line.paragraph) {
                                    line.paragraph = paragraph;
                                }
                            }
                        }
                        pages.push(page);
                    }
                    break;
                }
                cursor = following;
            }
            if pages.len() > self.paragraphs.len().saturating_mul(8).max(8) {
                break;
            }
        }
        self.pages = pages;
        self.dress_pages();
        rebuilt
    }

    fn at_end(&self, cursor: Cursor) -> bool {
        match self.lines.get(cursor.paragraph) {
            None => true,
            Some(lines) if (cursor.line as usize) < lines.len() => false,
            Some(_) => cursor.paragraph + 1 >= self.lines.len(),
        }
    }

    fn fill_page(&self, start: Cursor, flow: NoteFlow, built: usize) -> (Page, NoteFlow) {
        let mut lines = Vec::new();
        let mut used = 0.0;
        let mut cursor = start;
        let limit = self.geometry.content_height();
        let verso = (built + 1).is_multiple_of(2);
        let reserve_note = self.note_on_paragraph(start.paragraph);
        let note_reserve = if reserve_note {
            self.geometry.leading
        } else {
            0.0
        };
        while !self.at_end(cursor) {
            let Some(line) = self.line_at(cursor) else {
                break;
            };
            let style = &self.paragraphs[line.paragraph].style;
            if self.refuses_recto(line, !lines.is_empty(), verso) {
                break;
            }
            if used + line.height + note_reserve > limit && !lines.is_empty() {
                break;
            }
            if style.widow_orphan && line.is_first && !line.is_last {
                let rest = self.paragraph_rest_height(cursor);
                if used > 0.0 && used + line.height + note_reserve <= limit && used + rest > limit {
                    // Orphan: do not leave the first line of a paragraph alone
                    // at the bottom of the page.
                    break;
                }
            }
            if style.widow_orphan && !lines.is_empty() && !line.is_last {
                let next = step(cursor, &self.lines);
                if let Some(next_line) = self.line_at(next) {
                    if next_line.is_last
                        && next_line.paragraph == line.paragraph
                        && used + line.height + next_line.height + note_reserve > limit
                    {
                        // Widow: keep the last two lines together on the next page.
                        break;
                    }
                }
            }
            if line.keep_with_next && !lines.is_empty() {
                let next = step(cursor, &self.lines);
                let next_height = self
                    .line_at(next)
                    .map(|next_line| next_line.height)
                    .unwrap_or(self.geometry.leading);
                if used + line.height + next_height + note_reserve > limit {
                    break;
                }
            }
            if line.keep_together && line.is_first {
                let rest = self.paragraph_rest_height(cursor);
                if used > 0.0 && used + rest > limit {
                    break;
                }
            }
            used += line.height;
            let next = step(cursor, &self.lines);
            lines.push(line.clone());
            cursor = next;
        }
        if lines.is_empty() {
            if let Some(line) = self.line_at(start) {
                // Do not undo a recto refusal: an empty verso must stay empty
                // of the chapter that is waiting for an odd page.
                if !self.refuses_recto(line, false, verso) {
                    lines.push(line.clone());
                }
            }
        }
        let (footnotes, flow) = self.place_notes(&lines, flow);
        (
            Page {
                start,
                lines,
                footnotes,
                note_carry: flow.carry.clone(),
                note_waiting: flow.waiting.clone(),
                blank: false,
                folio: None,
                running_head: None,
                content_inset: 0.0,
            },
            flow,
        )
    }

    fn place_notes(&self, lines: &[FlowLine], mut flow: NoteFlow) -> (Vec<String>, NoteFlow) {
        let mut queue = Vec::new();
        if let Some(rest) = flow.carry.take() {
            queue.push(rest);
        }
        queue.append(&mut flow.waiting);
        for line in lines.iter().filter(|line| line.is_first) {
            if let Some(note) = self
                .paragraphs
                .get(line.paragraph)
                .and_then(|paragraph| paragraph.note.clone())
            {
                queue.push(note);
            }
        }
        let mut footnotes = Vec::new();
        let mut carry = None;
        let mut waiting = Vec::new();
        for note in queue {
            if carry.is_some() {
                waiting.push(note);
                continue;
            }
            let (head, tail) = split_note(&note);
            footnotes.push(head);
            carry = tail;
        }
        (footnotes, NoteFlow { carry, waiting })
    }

    fn each_paragraph_is_one_page(&self) -> bool {
        if self.hints.recto_at.iter().any(|flag| *flag) {
            return false;
        }
        if self.paragraphs.iter().any(|paragraph| {
            paragraph.note.is_some()
                || paragraph.style.keep_with_next
                || paragraph.style.name == "Image"
                || paragraph.style.name == "Caption"
        }) {
            return false;
        }
        // One line per page only when no two lines could share one: a
        // one-line paragraph is otherwise just a short paragraph.
        let limit = self.geometry.content_height();
        self.lines.iter().all(|lines| {
            lines.len() == 1
                && lines[0].height <= limit
                && lines[0].height * 2.0 > limit
                && !lines[0].keep_with_next
                && !lines[0].keep_together
        })
    }

    /// One page from the shared decision. `filled` is body placement: the
    /// caller then applies widow control and advances the cursor. A blank
    /// verso and a notes-only page leave the cursor where it is.
    fn take_page(&self, built: usize, cursor: Cursor, flow: NoteFlow) -> (Page, NoteFlow, bool) {
        match page_decision(
            built,
            self.is_recto_opener(cursor),
            self.at_end(cursor),
            &flow,
        ) {
            PageStep::Blank { outgoing } => {
                let page = self.blank_page(cursor, &outgoing);
                (page, outgoing, false)
            }
            PageStep::NotesOnly => {
                let (footnotes, next) = self.place_notes(&[], flow);
                let page = self.note_page(cursor, footnotes, &next);
                (page, next, false)
            }
            PageStep::Fill => {
                let (page, next) = self.fill_page(cursor, flow, built);
                (page, next, true)
            }
        }
    }

    fn is_recto_opener(&self, cursor: Cursor) -> bool {
        self.line_at(cursor).is_some_and(|line| {
            line.line == 0
                && self
                    .hints
                    .recto_at
                    .get(line.paragraph)
                    .copied()
                    .unwrap_or(false)
        })
    }

    /// A recto chapter does not join a page that already has lines, and does
    /// not open on an empty verso. [`page_decision`] is what emits the blank.
    fn refuses_recto(&self, line: &FlowLine, page_has_lines: bool, verso: bool) -> bool {
        if !line.is_first {
            return false;
        }
        let recto = self
            .hints
            .recto_at
            .get(line.paragraph)
            .copied()
            .unwrap_or(false);
        recto && (page_has_lines || verso)
    }

    fn note_page(&self, start: Cursor, footnotes: Vec<String>, flow: &NoteFlow) -> Page {
        Page {
            start,
            lines: Vec::new(),
            footnotes,
            note_carry: flow.carry.clone(),
            note_waiting: flow.waiting.clone(),
            blank: false,
            folio: None,
            running_head: None,
            content_inset: 0.0,
        }
    }

    fn blank_page(&self, start: Cursor, flow: &NoteFlow) -> Page {
        Page {
            start,
            lines: Vec::new(),
            footnotes: Vec::new(),
            // The blank consumes no notes. The next page, and a repaginate
            // that reloads this page, receive the same pending flow.
            note_carry: flow.carry.clone(),
            note_waiting: flow.waiting.clone(),
            blank: true,
            folio: None,
            running_head: None,
            content_inset: 0.0,
        }
    }

    fn dress_pages(&mut self) {
        let inner = self.geometry.margin_inner;
        let outer = self.geometry.margin_outer;
        let facing = self.hints.facing;
        let mut carried = String::new();
        for (index, page) in self.pages.iter_mut().enumerate() {
            let page_no = index as u32 + 1;
            let verso = facing && page_no.is_multiple_of(2);
            page.content_inset = if verso { outer } else { inner };
            if !self.hints.folio && !self.hints.running_head {
                continue;
            }
            if let Some(line) = page.lines.first() {
                if let Some(head) = self.hints.heads.get(line.paragraph) {
                    if !head.is_empty() {
                        carried = head.clone();
                    }
                }
            }
            let opener = page.lines.first().is_some_and(|line| {
                line.is_first
                    && self
                        .hints
                        .recto_at
                        .get(line.paragraph)
                        .copied()
                        .unwrap_or(false)
            }) || (index == 0 && self.hints.hide_opener_folio);
            if self.hints.folio && !(opener && self.hints.hide_opener_folio) {
                page.folio = Some(page_no.to_string());
            }
            if self.hints.running_head && !carried.is_empty() && !opener {
                page.running_head = Some(carried.clone());
            }
        }
    }

    fn note_on_paragraph(&self, index: usize) -> bool {
        self.paragraphs
            .get(index)
            .and_then(|paragraph| paragraph.note.as_ref())
            .is_some()
    }

    fn line_at(&self, cursor: Cursor) -> Option<&FlowLine> {
        self.lines
            .get(cursor.paragraph)
            .and_then(|lines| lines.get(cursor.line as usize))
    }

    fn paragraph_rest_height(&self, cursor: Cursor) -> f32 {
        self.lines
            .get(cursor.paragraph)
            .map(|lines| {
                lines
                    .iter()
                    .skip(cursor.line as usize)
                    .map(|line| line.height)
                    .sum()
            })
            .unwrap_or(0.0)
    }
}

/// Notes longer than one footnote line continue on the next page.
fn split_note(note: &str) -> (String, Option<String>) {
    const BUDGET: usize = 48;
    let count = note.chars().count();
    if count <= BUDGET {
        return (note.to_string(), None);
    }
    let head: String = note.chars().take(BUDGET).collect();
    let tail: String = note.chars().skip(BUDGET).collect();
    (head, Some(tail))
}

fn step(cursor: Cursor, lines: &[Vec<FlowLine>]) -> Cursor {
    if lines
        .get(cursor.paragraph)
        .is_some_and(|set| (cursor.line as usize) + 1 < set.len())
    {
        Cursor {
            paragraph: cursor.paragraph,
            line: cursor.line + 1,
        }
    } else {
        Cursor {
            paragraph: cursor.paragraph + 1,
            line: 0,
        }
    }
}

fn next_cursor(page: &Page) -> Cursor {
    match page.lines.last() {
        Some(line) => {
            if line.is_last {
                Cursor {
                    paragraph: line.paragraph + 1,
                    line: 0,
                }
            } else {
                Cursor {
                    paragraph: line.paragraph,
                    line: line.line + 1,
                }
            }
        }
        None => page.start,
    }
}

struct Broken {
    glyphs: Vec<Glyph>,
    width: f32,
    text: String,
}

fn break_lines(
    text: &str,
    glyphs: &[Glyph],
    measure: f32,
    first_indent: f32,
    hyphenate: bool,
    hyphenator: Option<&Standard>,
) -> Vec<Broken> {
    if glyphs.is_empty() {
        return Vec::new();
    }
    let mut breaks = HashMap::<usize, BreakOpportunity>::new();
    for (byte, opportunity) in linebreaks(text) {
        breaks.insert(byte, opportunity);
    }
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut width = 0.0;
    let mut last_break: Option<usize> = None;
    let mut limit = (measure - first_indent).max(1.0);
    for (index, glyph) in glyphs.iter().enumerate() {
        let cluster = glyph.cluster as usize;
        if index > start {
            if let Some(opportunity) = breaks.get(&cluster) {
                if matches!(
                    opportunity,
                    BreakOpportunity::Mandatory | BreakOpportunity::Allowed
                ) {
                    last_break = Some(index);
                }
                if matches!(opportunity, BreakOpportunity::Mandatory) {
                    lines.push(slice_line(text, glyphs, start, index, width, false));
                    start = index;
                    width = 0.0;
                    last_break = None;
                    limit = measure;
                }
            }
        }
        if width + glyph.x_advance > limit && index > start {
            let (at, hyphen) = if let Some(at) = last_break.filter(|at| *at > start) {
                (at, false)
            } else if let Some(at) = hyphen_point(text, glyphs, start, index, hyphenate, hyphenator)
            {
                (at, true)
            } else {
                (index, false)
            };
            let at = if at <= start { index } else { at };
            lines.push(slice_line(
                text,
                glyphs,
                start,
                at,
                width_of(&glyphs[start..at]),
                hyphen,
            ));
            limit = measure;
            start = at;
            width = width_of(&glyphs[start..index]);
            last_break = None;
        }
        width += glyph.x_advance;
    }
    if start < glyphs.len() {
        lines.push(slice_line(text, glyphs, start, glyphs.len(), width, false));
    }
    lines
}

fn width_of(glyphs: &[Glyph]) -> f32 {
    glyphs.iter().map(|glyph| glyph.x_advance).sum()
}

fn slice_line(
    text: &str,
    glyphs: &[Glyph],
    start: usize,
    end: usize,
    width: f32,
    hyphen: bool,
) -> Broken {
    let byte_start = glyphs
        .get(start)
        .map(|glyph| glyph.cluster as usize)
        .unwrap_or(0);
    let byte_end = glyphs
        .get(end)
        .map(|glyph| glyph.cluster as usize)
        .unwrap_or(text.len());
    let slice = text.get(byte_start..byte_end).unwrap_or("").trim_end();
    let mut text = slice.to_string();
    if hyphen {
        text.push('-');
    }
    Broken {
        glyphs: glyphs[start..end].to_vec(),
        width,
        text,
    }
}

/// When a page would open on a paragraph's last line, pull the previous
/// line onto it if that line is still sitting at the bottom of the page
/// we just closed.
fn avoid_widow(pages: &mut Vec<Page>, mut page: Page) -> Page {
    let widow = page
        .lines
        .first()
        .is_some_and(|line| line.widow_orphan && line.is_last && !line.is_first);
    if !widow {
        return page;
    }
    let Some(prev) = pages.last_mut() else {
        return page;
    };
    if prev.lines.len() < 2 {
        return page;
    }
    let Some(stolen) = prev.lines.pop() else {
        return page;
    };
    let adjacent = page
        .lines
        .first()
        .is_some_and(|line| stolen.paragraph == line.paragraph && stolen.line + 1 == line.line);
    if !adjacent {
        prev.lines.push(stolen);
        return page;
    }
    let mut lines = Vec::with_capacity(page.lines.len() + 1);
    lines.push(stolen);
    lines.append(&mut page.lines);
    let start = Cursor {
        paragraph: lines[0].paragraph,
        line: lines[0].line,
    };
    Page {
        start,
        lines,
        footnotes: page.footnotes,
        note_carry: page.note_carry,
        blank: page.blank,
        folio: page.folio,
        running_head: page.running_head,
        note_waiting: page.note_waiting,
        content_inset: page.content_inset,
    }
}

fn geometry_eq(left: &Geometry, right: &Geometry) -> bool {
    left.page_width == right.page_width
        && left.page_height == right.page_height
        && left.margin_top == right.margin_top
        && left.margin_bottom == right.margin_bottom
        && left.margin_inner == right.margin_inner
        && left.margin_outer == right.margin_outer
        && left.font_size == right.font_size
        && left.leading == right.leading
}

fn same_shape(left: &Paragraph, right: &Paragraph) -> bool {
    left.text == right.text
        && left.style.small_caps == right.style.small_caps
        && left.style.oldstyle_figures == right.style.oldstyle_figures
        && left.style.font_size == right.style.font_size
        && left.style.leading == right.style.leading
        && left.style.first_indent == right.style.first_indent
        && left.style.drop_cap_lines == right.style.drop_cap_lines
        && left.style.hyphenate == right.style.hyphenate
        && left.style.space_before == right.style.space_before
        && left.style.space_after == right.style.space_after
}

fn hyphen_point(
    text: &str,
    glyphs: &[Glyph],
    start: usize,
    end: usize,
    hyphenate: bool,
    hyphenator: Option<&Standard>,
) -> Option<usize> {
    if !hyphenate {
        return None;
    }
    let hyphenator = hyphenator?;
    let byte_start = glyphs.get(start)?.cluster as usize;
    let byte_end = glyphs
        .get(end)
        .map(|glyph| glyph.cluster as usize)
        .unwrap_or(text.len());
    let word = text
        .get(byte_start..byte_end)?
        .split_whitespace()
        .next_back()?;
    let hyphenated = hyphenator.hyphenate(word);
    let breaks = hyphenated.breaks;
    let last = breaks.last().copied()?;
    let target = byte_start + last;
    glyphs
        .iter()
        .position(|glyph| glyph.cluster as usize >= target)
        .filter(|index| *index > start && *index <= end)
}

/// `pages` copies of one shaped line. Opening this book shapes once.
pub fn book_of_repeated_line(face: Face, pages: u32, text: &str) -> Result<Document, TypesetError> {
    let paragraphs = (0..pages)
        .map(|index| Paragraph {
            id: index as u64 + 1,
            text: text.to_string(),
            style: ParagraphStyle::default(),
            note: None,
            note_is_endnote: false,
        })
        .collect();
    Document::new(face, Geometry::one_line_pages(), paragraphs)
}

/// Paragraphs of `seed`, one per page, for `pages` pages.
pub fn book_of_pages(face: Face, pages: u32, seed: &str) -> Result<Document, TypesetError> {
    let paragraphs = (0..pages)
        .map(|index| Paragraph {
            id: index as u64 + 1,
            text: format!("{seed} {index}"),
            style: ParagraphStyle::default(),
            note: None,
            note_is_endnote: false,
        })
        .collect();
    Document::new(face, Geometry::one_line_pages(), paragraphs)
}

#[cfg(test)]
mod page_decision_tests {
    use super::{NoteFlow, PageStep, page_decision};

    #[test]
    fn a_verso_stays_blank_while_a_note_carry_is_pending() {
        let pending = NoteFlow {
            carry: Some("N".to_string()),
            waiting: vec!["Second note body.".to_string()],
        };
        let idle = NoteFlow::default();
        let cases = [
            (0usize, true, false, &idle, PageStep::Fill),
            (
                1,
                true,
                false,
                &idle,
                PageStep::Blank {
                    outgoing: idle.clone(),
                },
            ),
            (
                1,
                true,
                false,
                &pending,
                PageStep::Blank {
                    outgoing: pending.clone(),
                },
            ),
            (2, true, false, &pending, PageStep::Fill),
            (1, false, true, &pending, PageStep::NotesOnly),
            (1, false, false, &pending, PageStep::Fill),
            (
                3,
                true,
                false,
                &pending,
                PageStep::Blank {
                    outgoing: pending.clone(),
                },
            ),
            (0, false, false, &idle, PageStep::Fill),
        ];
        for (built, recto, at_end, flow, expected) in cases {
            assert_eq!(
                page_decision(built, recto, at_end, flow),
                expected,
                "built {built} recto {recto} at_end {at_end}"
            );
        }
    }
}
