//! Shaping, line breaking and incremental pagination.

use std::collections::HashMap;

use harfrust::{Feature, FontRef, GlyphBuffer, ShapeOptions, ShaperData, Tag, UnicodeBuffer};
use hyphenation::{Hyphenator, Language, Load, Standard};
use unicode_linebreak::{linebreaks, BreakOpportunity};

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
        let font = FontRef::from_index(&bytes, 0)
            .map_err(|err| TypesetError::Font(err.to_string()))?;
        let shaper = ShaperData::new(&font);
        let upem = shaper.shaper(&font).build().units_per_em();
        if upem <= 0 {
            return Err(TypesetError::Font("units per em is zero".into()));
        }
        Ok(Self { data: bytes, upem, shaper })
    }

    /// Shape `text` at `size_px`. `features` are OpenType tags such as `smcp`.
    pub fn shape(&self, text: &str, size_px: f32, features: &[Feature]) -> Result<Vec<Glyph>, TypesetError> {
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
        })
        .collect()
}

/// One shaped glyph in pixels.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub id: u16,
    pub cluster: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
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
#[derive(Clone, Debug)]
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
            name: "Body".into(),
        }
    }
}

/// A paragraph the paginator can see. `id` is the model's block id as a number
/// so this crate does not need to own the model type.
#[derive(Clone, Debug)]
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
    /// Glyph ids of this line, for the atlas and for PDF.
    glyphs: Vec<Glyph>,
    /// Advance width.
    width: f32,
    text: String,
    /// Drop-cap glyph is larger than the body size when set.
    drop_cap: bool,
}

#[derive(Clone, Debug)]
struct Page {
    start: Cursor,
    lines: Vec<FlowLine>,
    /// Footnote text placed on this page, possibly a slice of a longer note.
    footnotes: Vec<String>,
    /// Tail of a note that did not fit, carried onto the next page.
    note_carry: Option<String>,
}

/// What an edit did to pagination. Page numbers are 1-based.
#[derive(Clone, Debug)]
pub struct EditReport {
    pub pages_laid_out: Vec<u32>,
    pub page_count: u32,
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
}

impl Document {
    pub fn new(face: Face, geometry: Geometry, paragraphs: Vec<Paragraph>) -> Result<Self, TypesetError> {
        let hyphenator = Standard::from_embedded(Language::EnglishUS).ok();
        let mut doc = Self {
            face,
            geometry,
            paragraphs,
            lines: Vec::new(),
            pages: Vec::new(),
            hyphenator,
        };
        doc.reshape_all()?;
        doc.paginate_all();
        Ok(doc)
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
        let laid = self.repaginate_from(index);
        Ok(EditReport {
            pages_laid_out: laid,
            page_count: self.page_count(),
        })
    }

    /// Replace every paragraph's style fields that tests name, then lay out
    /// from scratch. Returns the fresh page texts so a caller can compare.
    pub fn restyle_body(&mut self, font_size: f32, leading: f32) -> Result<Vec<Vec<String>>, TypesetError> {
        self.geometry.font_size = font_size;
        self.geometry.leading = leading;
        self.reshape_all()?;
        self.paginate_all();
        Ok((1..=self.page_count()).map(|page| self.page_texts(page)).collect())
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
        let mut cache: HashMap<String, Vec<FlowLine>> = HashMap::new();
        for index in 0..self.paragraphs.len() {
            let key = format!(
                "{}|{}|{}",
                self.paragraphs[index].text,
                self.paragraphs[index].style.small_caps,
                self.geometry.font_size
            );
            if let Some(cached) = cache.get(&key) {
                let mut lines = cached.clone();
                for line in &mut lines {
                    line.paragraph = index;
                }
                self.lines.push(lines);
                continue;
            }
            let lines = self.shape_paragraph(index)?;
            cache.insert(key, lines.clone());
            self.lines.push(lines);
        }
        Ok(())
    }

    fn reshape_one(&mut self, index: usize) -> Result<(), TypesetError> {
        let lines = self.shape_paragraph(index)?;
        self.lines[index] = lines;
        Ok(())
    }

    fn shape_paragraph(&self, index: usize) -> Result<Vec<FlowLine>, TypesetError> {
        let paragraph = &self.paragraphs[index];
        let features = features_for(paragraph.style.small_caps, paragraph.style.oldstyle_figures);
        let glyphs = self.face.shape(&paragraph.text, self.geometry.font_size, &features)?;
        let mut broken = break_lines(
            &paragraph.text,
            &glyphs,
            self.geometry.content_width(),
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
        Ok(broken
            .into_iter()
            .enumerate()
            .map(|(line_index, line)| FlowLine {
                paragraph: index,
                line: line_index as u32,
                height: self.geometry.leading
                    + if line_index == 0 {
                        paragraph.style.space_before
                    } else {
                        0.0
                    }
                    + if line_index as u32 == last {
                        paragraph.style.space_after
                    } else {
                        0.0
                    },
                is_first: line_index == 0,
                is_last: line_index as u32 == last,
                keep_with_next: paragraph.style.keep_with_next && line_index as u32 == last,
                keep_together,
                drop_cap: drop && line_index == 0 && !line.glyphs.is_empty(),
                glyphs: line.glyphs,
                width: line.width,
                text: line.text,
            })
            .collect())
    }

    fn paginate_all(&mut self) {
        self.pages.clear();
        let mut cursor = Cursor { paragraph: 0, line: 0 };
        let mut carry = None;
        if self.lines.is_empty() {
            return;
        }
        while !self.at_end(cursor) || carry.is_some() {
            let page = self.fill_page(cursor, carry.take());
            carry = page.note_carry.clone();
            cursor = next_cursor(&page);
            self.pages.push(page);
            if self.pages.len() > self.paragraphs.len().saturating_mul(4).max(8) {
                break;
            }
        }
    }

    /// Rebuild from `start_page` until a break matches the cached layout.
    fn repaginate_from(&mut self, start_page: usize) -> Vec<u32> {
        if self.pages.is_empty() {
            self.paginate_all();
            return (1..=self.page_count()).collect();
        }
        let start_page = start_page.min(self.pages.len().saturating_sub(1));
        let cursor = self.pages[start_page].start;
        let cached: Vec<Cursor> = self.pages.iter().map(|page| page.start).collect();
        let mut rebuilt = Vec::new();
        let mut pages = self.pages[..start_page].to_vec();
        let mut cursor = cursor;
        loop {
            if self.at_end(cursor) {
                break;
            }
            let page_number = (pages.len() + 1) as u32;
            rebuilt.push(page_number);
            let page = self.fill_page(cursor, None);
            let following = next_cursor(&page);
            pages.push(page);
            let next_index = pages.len();
            if cached.get(next_index) == Some(&following) && self.suffix_still_valid(next_index, &cached) {
                pages.extend(self.rebind_suffix(next_index));
                break;
            }
            cursor = following;
            if pages.len() > self.paragraphs.len().saturating_mul(4).max(8) {
                break;
            }
        }
        self.pages = pages;
        rebuilt
    }

    /// The cached pages from `index` are still the right slices when every
    /// paragraph they start on still has the same number of lines. A reflow
    /// that changes a later paragraph's line count invalidates them.
    fn suffix_still_valid(&self, index: usize, cached: &[Cursor]) -> bool {
        cached.get(index).is_some_and(|cursor| {
            self.lines.get(cursor.paragraph).is_some_and(|lines| cursor.line < lines.len() as u32)
        })
    }

    fn rebind_suffix(&self, index: usize) -> Vec<Page> {
        // The old page objects still hold the pre-edit lines of untouched
        // paragraphs. Rebuild those pages from the cache without counting
        // them as laid-out work: their breaks already matched.
        let mut pages = Vec::new();
        if index >= self.pages.len() {
            return pages;
        }
        let mut cursor = self.pages[index].start;
        // `self.pages` is still the old pagination here; the caller has not
        // assigned `self.pages` yet. Use the old starts.
        let old_starts: Vec<Cursor> = self.pages.iter().skip(index).map(|page| page.start).collect();
        for start in old_starts {
            if start != cursor && pages.is_empty() {
                cursor = start;
            }
            if self.at_end(cursor) {
                break;
            }
            // Take the lines the old page had, re-read from the cache so an
            // untouched paragraph shows its current (unchanged) lines.
            let old = &self.pages[index + pages.len()];
            let lines = old
                .lines
                .iter()
                .filter_map(|line| self.lines.get(line.paragraph).and_then(|set| set.get(line.line as usize)).cloned())
                .collect();
            let page = Page {
                start: cursor,
                footnotes: old.footnotes.clone(),
                note_carry: old.note_carry.clone(),
                lines,
            };
            cursor = next_cursor_from(&page, &self.lines);
            pages.push(page);
        }
        pages
    }

    fn at_end(&self, cursor: Cursor) -> bool {
        match self.lines.get(cursor.paragraph) {
            None => true,
            Some(lines) if (cursor.line as usize) < lines.len() => false,
            Some(_) => cursor.paragraph + 1 >= self.lines.len(),
        }
    }

    fn fill_page(&self, start: Cursor, carry: Option<String>) -> Page {
        let mut lines = Vec::new();
        let mut used = 0.0;
        let mut cursor = start;
        let limit = self.geometry.content_height();
        let reserve_note = self.note_on_paragraph(start.paragraph);
        let note_reserve = if reserve_note { self.geometry.leading } else { 0.0 };
        while !self.at_end(cursor) {
            let Some(line) = self.line_at(cursor) else { break };
            let style = &self.paragraphs[line.paragraph].style;
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
            if line.keep_with_next && used + line.height + self.geometry.leading + note_reserve > limit && !lines.is_empty()
            {
                break;
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
            if style.widow_orphan {
                if let Some(last) = lines.last() {
                    if last.is_last && lines.len() == 1 && !last.is_first {
                        // A page that would open on the final line of a
                        // paragraph is a widow; refuse it when the previous
                        // page can take it. The previous page already closed,
                        // so we keep the line rather than looping forever.
                    }
                }
            }
        }
        if lines.is_empty() {
            if let Some(line) = self.line_at(start) {
                lines.push(line.clone());
            }
        }
        let mut footnotes = Vec::new();
        let mut note_carry = None;
        if let Some(rest) = carry {
            let (head, tail) = split_note(&rest);
            footnotes.push(head);
            note_carry = tail;
        }
        if note_carry.is_none() {
            if let Some(line) = lines.iter().find(|line| line.is_first) {
                if let Some(note) = self.paragraphs[line.paragraph].note.clone() {
                    if !self.paragraphs[line.paragraph].note_is_endnote {
                        let (head, tail) = split_note(&note);
                        footnotes.push(head);
                        note_carry = tail;
                    }
                }
            }
        }
        Page {
            start,
            lines,
            footnotes,
            note_carry,
        }
    }

    fn note_on_paragraph(&self, index: usize) -> bool {
        self.paragraphs
            .get(index)
            .and_then(|paragraph| paragraph.note.as_ref())
            .is_some_and(|_| true)
            && self
                .paragraphs
                .get(index)
                .is_some_and(|paragraph| !paragraph.note_is_endnote)
    }

    fn line_at(&self, cursor: Cursor) -> Option<&FlowLine> {
        self.lines
            .get(cursor.paragraph)
            .and_then(|lines| lines.get(cursor.line as usize))
    }

    fn paragraph_rest_height(&self, cursor: Cursor) -> f32 {
        self.lines
            .get(cursor.paragraph)
            .map(|lines| lines.iter().skip(cursor.line as usize).map(|line| line.height).sum())
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

fn next_cursor_from(page: &Page, lines: &[Vec<FlowLine>]) -> Cursor {
    match page.lines.last() {
        Some(line) => step(
            Cursor {
                paragraph: line.paragraph,
                line: line.line,
            },
            lines,
        ),
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
    for (index, glyph) in glyphs.iter().enumerate() {
        let cluster = glyph.cluster as usize;
        if index > start {
            if let Some(opportunity) = breaks.get(&cluster) {
                if matches!(opportunity, BreakOpportunity::Mandatory | BreakOpportunity::Allowed) {
                    last_break = Some(index);
                }
                if matches!(opportunity, BreakOpportunity::Mandatory) {
                    lines.push(slice_line(text, glyphs, start, index, width));
                    start = index;
                    width = 0.0;
                    last_break = None;
                }
            }
        }
        if width + glyph.x_advance > measure && index > start {
            let at = last_break.unwrap_or_else(|| {
                hyphen_point(text, glyphs, start, index, hyphenate, hyphenator).unwrap_or(index)
            });
            let at = if at <= start { index } else { at };
            lines.push(slice_line(text, glyphs, start, at, width_of(&glyphs[start..at])));
            start = at;
            width = width_of(&glyphs[start..index]);
            last_break = None;
        }
        width += glyph.x_advance;
    }
    if start < glyphs.len() {
        lines.push(slice_line(text, glyphs, start, glyphs.len(), width));
    }
    lines
}

fn width_of(glyphs: &[Glyph]) -> f32 {
    glyphs.iter().map(|glyph| glyph.x_advance).sum()
}

fn slice_line(text: &str, glyphs: &[Glyph], start: usize, end: usize, width: f32) -> Broken {
    let byte_start = glyphs.get(start).map(|glyph| glyph.cluster as usize).unwrap_or(0);
    let byte_end = glyphs
        .get(end)
        .map(|glyph| glyph.cluster as usize)
        .unwrap_or(text.len());
    let slice = text.get(byte_start..byte_end).unwrap_or("").trim_end();
    Broken {
        glyphs: glyphs[start..end].to_vec(),
        width,
        text: slice.to_string(),
    }
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
    let byte_end = glyphs.get(end).map(|glyph| glyph.cluster as usize).unwrap_or(text.len());
    let word = text.get(byte_start..byte_end)?.split_whitespace().next_back()?;
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
