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
    /// Identity of the font bytes, so caches keyed by face tell fonts apart.
    id: u64,
    cap_ratio: f32,
    /// The font has real small capitals (`smcp` changes glyphs).
    real_small_caps: bool,
}

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Face")
            .field("bytes", &self.data.len())
            .field("upem", &self.upem)
            .finish_non_exhaustive()
    }
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
        let id = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut hasher);
            hasher.finish()
        };
        let cap_ratio = {
            use skrifa::MetadataProvider;
            skrifa::FontRef::from_index(&bytes, 0)
                .ok()
                .and_then(|font| {
                    font.metrics(
                        skrifa::instance::Size::unscaled(),
                        skrifa::instance::LocationRef::default(),
                    )
                    .cap_height
                    .map(|cap| cap / upem as f32)
                })
                .filter(|ratio| *ratio > 0.3 && *ratio < 1.0)
                .unwrap_or(0.7)
        };
        let mut face = Self {
            data: bytes,
            upem,
            shaper,
            id,
            cap_ratio,
            real_small_caps: false,
        };
        // A font has small caps when `smcp` changes what "x" shapes to.
        let plain = face.shape("x", 10.0, &[])?;
        let caps = face.shape("x", 10.0, &features_for(true, false))?;
        face.real_small_caps =
            plain.first().map(|glyph| glyph.id) != caps.first().map(|glyph| glyph.id);
        Ok(face)
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

    /// Shape `text` with small capitals when asked: the font's own (`smcp`)
    /// if it has them, otherwise capitals set at 78% for lowercase letters.
    /// Clusters stay byte offsets into `text`.
    pub fn shape_small_caps(
        &self,
        text: &str,
        size_px: f32,
        small_caps: bool,
        oldstyle: bool,
    ) -> Result<Vec<Glyph>, TypesetError> {
        if !small_caps || self.real_small_caps {
            return self.shape(text, size_px, &features_for(small_caps, oldstyle));
        }
        let features = features_for(false, oldstyle);
        let mut glyphs = Vec::new();
        let mut chars = text.char_indices().peekable();
        while let Some((at, ch)) = chars.next() {
            let lower = ch.is_lowercase();
            let mut end = at + ch.len_utf8();
            while let Some((next_at, next)) = chars.peek().copied() {
                if next.is_lowercase() != lower {
                    break;
                }
                end = next_at + next.len_utf8();
                chars.next();
            }
            let start = at;
            let segment = &text[start..end];
            if lower {
                // Each lowercase letter becomes its capital, shaped small;
                // clusters map back to the letter it came from.
                let mut upper = String::new();
                let mut origin = Vec::new();
                for (offset, letter) in segment.char_indices() {
                    for capital in letter.to_uppercase() {
                        origin.push((upper.len(), start + offset));
                        upper.push(capital);
                    }
                }
                for glyph in self.shape(&upper, size_px * 0.78, &features)? {
                    let cluster = origin
                        .iter()
                        .rev()
                        .find(|(upper_at, _)| *upper_at <= glyph.cluster as usize)
                        .map_or(start, |(_, text_at)| *text_at);
                    glyphs.push(Glyph {
                        cluster: cluster as u32,
                        ..glyph
                    });
                }
            } else {
                for glyph in self.shape(segment, size_px, &features)? {
                    glyphs.push(Glyph {
                        cluster: glyph.cluster + start as u32,
                        ..glyph
                    });
                }
            }
        }
        Ok(glyphs)
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

    /// Cap height as a fraction of the em, from the font's metrics.
    pub fn cap_height_ratio(&self) -> f32 {
        self.cap_ratio
    }

    /// Identity of the font bytes; equal for a [`Self::duplicate`].
    pub fn id(&self) -> u64 {
        self.id
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
            face: 0,
            id: info.glyph_id as u16,
            cluster: info.cluster,
            x_advance: pos.x_advance as f32 * scale,
            x_offset: pos.x_offset as f32 * scale,
            y_offset: pos.y_offset as f32 * scale,
            em: size_px,
        })
        .collect()
}

/// Cluster of a hyphen glyph inserted at a hyphenated line break. It points
/// at no byte of the paragraph; its text is "-".
pub const HYPHEN_CLUSTER: u32 = u32::MAX;

/// Cluster of a footnote or endnote reference mark set after a paragraph.
/// It points at no byte of the paragraph; its text is the note number.
pub const NOTE_MARK_CLUSTER: u32 = u32::MAX - 1;

/// One line of a footnote, shaped at footnote size.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteLine {
    /// The line's text, number included on a note's first line.
    pub text: String,
    /// Glyphs in visual order; clusters are byte offsets into `text`.
    pub glyphs: Arc<Vec<Glyph>>,
}

/// A footnote line placed on a page.
#[derive(Clone, Debug)]
pub struct FootnoteLine {
    /// The line's text.
    pub text: String,
    /// Glyphs; clusters are byte offsets into `text`.
    pub glyphs: Vec<Glyph>,
    /// Baseline, down from the top of the page.
    pub baseline: f32,
    /// Footnote em size.
    pub em: f32,
}

/// One shaped line, tied back to the paragraph it was broken from.
#[derive(Clone, Debug)]
pub struct PaintedLine {
    /// Index into the paragraph list passed to [`Document::new`].
    pub paragraph: usize,
    /// Glyphs in visual order. `cluster` is a byte offset into the
    /// paragraph's text, or [`HYPHEN_CLUSTER`].
    pub glyphs: Vec<Glyph>,
    /// First-line indent in geometry units. Other lines are zero.
    pub indent: f32,
    /// Baseline, measured down from the top of the page in geometry units.
    pub baseline: f32,
    /// Em size of the paragraph's body text.
    pub em: f32,
    /// This is the paragraph's last line.
    pub ends_paragraph: bool,
}

/// One shaped glyph in pixels.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Glyph {
    /// Which face of the document's family drew it: see [`FaceStyle`].
    pub face: u8,
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
    /// First-line indent, from the left indent; negative hangs.
    pub first_indent: f32,
    /// Stable name such as `Body` or `Chapter Title`. Empty uses the geometry's size.
    pub name: String,
    /// How lines sit in the measure.
    pub align: Alignment,
    /// Indent of every line from the left of the measure.
    pub left_indent: f32,
    /// Indent of every line from the right of the measure.
    pub right_indent: f32,
}

/// Line alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Alignment {
    /// Flush left, ragged right.
    #[default]
    Left,
    /// Flush both sides; a paragraph's last line is flush left.
    Justify,
    /// Centered.
    Center,
    /// Flush right.
    Right,
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
            align: Alignment::Left,
            left_indent: 0.0,
            right_indent: 0.0,
        }
    }
}

/// A paragraph the paginator can see. `id` is the model's block id as a number
/// so this crate does not need to own the model type.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Paragraph {
    pub id: u64,
    pub text: String,
    pub style: ParagraphStyle,
    /// Footnote or endnote body anchored to this paragraph, if any.
    pub note: Option<String>,
    pub note_is_endnote: bool,
    /// Reference mark set in superscript after the paragraph's last
    /// character, such as a note number. Empty for none.
    pub note_mark: String,
    /// Character styling. Empty means the whole paragraph in its style's face.
    pub runs: Vec<StyledRun>,
}

/// The faces of a family, as indices into [`Document::faces`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceStyle {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl FaceStyle {
    /// The face for these marks.
    pub fn of(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Self::Regular,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (true, true) => Self::BoldItalic,
        }
    }
}

/// Character styling over a span of a paragraph's text.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct StyledRun {
    /// First byte, inclusive.
    pub start: usize,
    /// First byte after the run.
    pub end: usize,
    pub bold: bool,
    pub italic: bool,
    pub small_caps: bool,
    /// Positive for superscript, negative for subscript, zero on the baseline.
    pub script: i8,
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
    /// On a paragraph's first line: its footnote, broken into lines.
    note: Option<Arc<Vec<NoteLine>>>,
}

#[derive(Clone, Debug)]
struct Page {
    start: Cursor,
    lines: Vec<FlowLine>,
    /// Footnote lines placed on this page, possibly part of a longer note.
    footnotes: Vec<NoteLine>,
    /// Lines of a note that did not fit, carried onto the next page.
    note_carry: Vec<NoteLine>,
    /// Notes referenced by now whose lines have not started yet.
    note_waiting: Vec<Arc<Vec<NoteLine>>>,
    /// A verso left empty so the next chapter can open on a recto.
    blank: bool,
    folio: Option<String>,
    running_head: Option<String>,
    /// Left inset of the text block. Verso pages mirror inner and outer.
    content_inset: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct NoteFlow {
    carry: Vec<NoteLine>,
    waiting: Vec<Arc<Vec<NoteLine>>>,
}

/// What the next page is, before any line is placed.
#[derive(Clone, Debug, PartialEq)]
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
        !self.carry.is_empty() || !self.waiting.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct LayoutHints {
    pub recto_at: Vec<bool>,
    /// A chapter opener that starts a new page (NextPage or NextRecto).
    pub break_at: Vec<bool>,
    pub heads: Vec<String>,
    pub folio: bool,
    pub hide_opener_folio: bool,
    pub running_head: bool,
    pub facing: bool,
    /// Page rules by chapter template; `master_at` indexes it per paragraph.
    /// Empty uses the geometry's margins and the flags above everywhere.
    pub masters: Vec<PageRules>,
    pub master_at: Vec<usize>,
}

/// Margins and furniture of one page master, as a chapter template uses it.
/// The trim size is the book's; masters differ in what sits inside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PageRules {
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_inner: f32,
    pub margin_outer: f32,
    pub folio: bool,
    pub running_head: bool,
    pub hide_opener_folio: bool,
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
    /// Regular, bold, italic, bold italic; missing styles use regular.
    faces: Vec<Face>,
    geometry: Geometry,
    paragraphs: Vec<Paragraph>,
    /// Cached lines per paragraph index.
    lines: Vec<Vec<FlowLine>>,
    pages: Vec<Page>,
    hyphenator: Option<Standard>,
    hints: LayoutHints,
    /// Paragraph index by paragraph id.
    by_id: HashMap<u64, usize>,
    /// Page numbers the table of contents shows, by chapter id.
    pub(crate) contents_pages: HashMap<u64, u32>,
}

impl Document {
    pub fn new(
        face: Face,
        geometry: Geometry,
        paragraphs: Vec<Paragraph>,
    ) -> Result<Self, TypesetError> {
        Self::new_with(vec![face], geometry, paragraphs, LayoutHints::default())
    }

    pub(crate) fn new_with(
        faces: Vec<Face>,
        geometry: Geometry,
        paragraphs: Vec<Paragraph>,
        hints: LayoutHints,
    ) -> Result<Self, TypesetError> {
        if faces.is_empty() {
            return Err(TypesetError::Font(
                "a document needs at least one face".into(),
            ));
        }
        let hyphenator = Standard::from_embedded(Language::EnglishUS).ok();
        let mut doc = Self {
            faces,
            geometry,
            paragraphs,
            lines: Vec::new(),
            pages: Vec::new(),
            hyphenator,
            hints,
            by_id: HashMap::new(),
            contents_pages: HashMap::new(),
        };
        doc.index_ids();
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

    /// Reading-order index of the paragraph with `id` (a block's raw id).
    pub fn paragraph_of(&self, id: u64) -> Option<usize> {
        self.by_id.get(&id).copied()
    }

    /// The page rules paragraph `index` is set under.
    fn rules(&self, index: usize) -> PageRules {
        self.hints
            .master_at
            .get(index)
            .and_then(|master| self.hints.masters.get(*master))
            .copied()
            .unwrap_or(PageRules {
                margin_top: self.geometry.margin_top,
                margin_bottom: self.geometry.margin_bottom,
                margin_inner: self.geometry.margin_inner,
                margin_outer: self.geometry.margin_outer,
                folio: self.hints.folio,
                running_head: self.hints.running_head,
                hide_opener_folio: self.hints.hide_opener_folio,
            })
    }

    /// Line measure for paragraph `index`: the trim less its master's margins.
    fn measure(&self, index: usize) -> f32 {
        let rules = self.rules(index);
        (self.geometry.page_width - rules.margin_inner - rules.margin_outer).max(1.0)
    }

    /// Text-block height of a page that starts on paragraph `index`.
    fn block_height(&self, index: usize) -> f32 {
        let rules = self.rules(index);
        (self.geometry.page_height - rules.margin_top - rules.margin_bottom)
            .max(self.geometry.leading)
    }

    /// The paragraph whose master a page follows: its first line's.
    fn page_paragraph(page: &Page) -> usize {
        page.lines
            .first()
            .map_or(page.start.paragraph, |line| line.paragraph)
    }

    fn index_ids(&mut self) {
        self.by_id = self
            .paragraphs
            .iter()
            .enumerate()
            .map(|(index, paragraph)| (paragraph.id, index))
            .collect();
    }

    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    /// The regular face.
    pub fn face(&self) -> &Face {
        &self.faces[0]
    }

    /// The face a glyph's `face` index names; regular when the family has
    /// no such style.
    pub fn face_at(&self, index: u8) -> &Face {
        self.faces.get(usize::from(index)).unwrap_or(&self.faces[0])
    }

    /// Every face of the family, regular first.
    pub fn faces(&self) -> &[Face] {
        &self.faces
    }

    /// Shape `text` in paragraph style `features`, span by span with each
    /// run's face, size and features; clusters stay offsets into `text`.
    fn shape_runs(
        &self,
        text: &str,
        runs: &[StyledRun],
        size: f32,
        small_caps: bool,
        oldstyle: bool,
    ) -> Result<Vec<Glyph>, TypesetError> {
        let plain = runs
            .iter()
            .all(|run| !run.bold && !run.italic && !run.small_caps && run.script == 0);
        if plain {
            return self
                .face()
                .shape_small_caps(text, size, small_caps, oldstyle);
        }
        let mut glyphs = Vec::new();
        let mut covered = 0;
        let spans = runs
            .iter()
            .filter(|run| run.start < run.end && run.end <= text.len())
            .cloned()
            .collect::<Vec<_>>();
        let mut emit = |glyphs: &mut Vec<Glyph>, run: &StyledRun| -> Result<(), TypesetError> {
            let Some(slice) = text.get(run.start..run.end) else {
                return Ok(());
            };
            let style = FaceStyle::of(run.bold, run.italic) as u8;
            let face_index = if usize::from(style) < self.faces.len() {
                style
            } else {
                0
            };
            let face = self.face_at(face_index);
            let (em, rise) = match run.script {
                s if s > 0 => (size * 0.65, size * 0.35),
                s if s < 0 => (size * 0.65, -size * 0.15),
                _ => (size, 0.0),
            };
            for glyph in face.shape_small_caps(slice, em, small_caps || run.small_caps, oldstyle)? {
                glyphs.push(Glyph {
                    face: face_index,
                    cluster: glyph.cluster + run.start as u32,
                    y_offset: glyph.y_offset + rise,
                    em,
                    ..glyph
                });
            }
            Ok(())
        };
        for run in &spans {
            if run.start > covered {
                emit(
                    &mut glyphs,
                    &StyledRun {
                        start: covered,
                        end: run.start,
                        ..StyledRun::default()
                    },
                )?;
            }
            emit(&mut glyphs, run)?;
            covered = covered.max(run.end);
        }
        if covered < text.len() {
            emit(
                &mut glyphs,
                &StyledRun {
                    start: covered,
                    end: text.len(),
                    ..StyledRun::default()
                },
            )?;
        }
        Ok(glyphs)
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
            .map(|page| {
                page.footnotes
                    .iter()
                    .map(|line| line.text.clone())
                    .collect()
            })
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
                let mut top = self.rules(Self::page_paragraph(page)).margin_top;
                page.lines
                    .iter()
                    .map(|line| {
                        let style = self.paragraphs.get(line.paragraph).map(|p| &p.style);
                        let after = match style {
                            Some(style) if line.is_last => style.space_after,
                            _ => 0.0,
                        };
                        let em = style
                            .map(|style| style.font_size)
                            .filter(|size| *size > 0.0)
                            .unwrap_or(self.geometry.font_size);
                        let leading = style
                            .map(|style| style.leading)
                            .filter(|leading| *leading > 0.0)
                            .unwrap_or(self.geometry.leading);
                        // The text sits centred in its leading, at the bottom
                        // of the line box (a drop-cap line's box is taller).
                        let bottom = top + line.height - after;
                        let baseline = bottom - (leading - em).max(0.0) / 2.0 - em * 0.2;
                        top += line.height;
                        PaintedLine {
                            paragraph: line.paragraph,
                            glyphs: line.glyphs.as_ref().clone(),
                            indent: line.indent,
                            baseline,
                            em,
                            ends_paragraph: line.is_last,
                        }
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
            && self.hints.facing == hints.facing
            && self.hints.masters == hints.masters;
        if !same_geometry || !same_page_rules || self.pages.is_empty() {
            self.geometry = geometry;
            self.paragraphs = paragraphs;
            self.hints = hints;
            self.index_ids();
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
        if old_paragraphs.len() != self.paragraphs.len()
            || old_paragraphs
                .iter()
                .zip(&self.paragraphs)
                .any(|(old, new)| old.id != new.id)
        {
            self.index_ids();
        }
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
                        && self.hints.break_at.get(*old) == hints.break_at.get(index)
                        && self.hints.master_at.get(*old) == hints.master_at.get(index)
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
                    note_mark: String::new(),
                    runs: Vec::new(),
                },
            );
        }
        self.index_ids();
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
            if image
                && caption
                && let Some(last) = self.lines.get_mut(index).and_then(|lines| lines.last_mut())
            {
                last.keep_with_next = true;
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
        let indent = paragraph.style.first_indent;
        let left = paragraph.style.left_indent.max(0.0);
        let measure = (self.measure(index) - left - paragraph.style.right_indent.max(0.0)).max(1.0);
        let mut glyphs = self.shape_runs(
            &paragraph.text,
            &paragraph.runs,
            size,
            paragraph.style.small_caps,
            paragraph.style.oldstyle_figures,
        )?;
        // A tab shapes as a space whose width alignment sets later.
        if paragraph.text.contains('\t') {
            let space = self.face().shape(" ", size, &features_for(false, false))?;
            if let Some(space) = space.first() {
                for glyph in glyphs
                    .iter_mut()
                    .filter(|glyph| is_tab(&paragraph.text, glyph))
                {
                    glyph.id = space.id;
                    glyph.face = 0;
                    glyph.x_advance = space.x_advance;
                }
            }
        }
        // A drop cap: the first letter, sized so its cap height reaches from
        // the baseline of line `drop_lines` up to the cap height of line one,
        // set on that lower baseline, with the lines beside it indented.
        let drop_lines = usize::from(paragraph.style.drop_cap_lines);
        let drop = if drop_lines >= 2 {
            let first_len = paragraph.text.chars().next().map_or(0, char::len_utf8);
            let ratio = self.face().cap_height_ratio();
            let cap_em = ((drop_lines - 1) as f32 * leading + size * ratio) / ratio;
            let cap_text = paragraph.text.get(..first_len).unwrap_or("");
            let cap: Vec<Glyph> = self
                .face()
                .shape(cap_text, cap_em, &features)?
                .into_iter()
                .map(|glyph| Glyph {
                    em: cap_em,
                    y_offset: -((drop_lines - 1) as f32) * leading,
                    ..glyph
                })
                .collect();
            if cap.is_empty() || first_len == 0 {
                None
            } else {
                glyphs.retain(|glyph| glyph.cluster as usize >= first_len);
                let width = cap.iter().map(|glyph| glyph.x_advance).sum::<f32>() + size * 0.2;
                Some((cap, width))
            }
        } else {
            None
        };
        let hang = drop.as_ref().map_or(0.0, |(_, width)| *width);
        let line_indent = |line: usize| {
            if drop.is_some() {
                if line < drop_lines { hang } else { 0.0 }
            } else if line == 0 {
                indent.max(-left)
            } else {
                0.0
            }
        };
        let mut broken = break_lines(
            &paragraph.text,
            &glyphs,
            measure,
            &line_indent,
            paragraph.style.hyphenate,
            self.hyphenator.as_ref(),
        );
        if let Some((cap, width)) = drop.as_ref() {
            // The cap leads line one; its advance is the hang, so line one's
            // text starts where the indented lines below it start.
            if broken.is_empty() {
                broken.push(Broken {
                    glyphs: Vec::new(),
                    width: 0.0,
                    text: String::new(),
                    hyphenated: false,
                });
            }
            if let Some(first) = broken.first_mut() {
                let mut lead: Vec<Glyph> = cap.clone();
                let drawn: f32 = cap.iter().map(|glyph| glyph.x_advance).sum();
                if let Some(last) = lead.last_mut() {
                    last.x_advance += width - drawn;
                }
                lead.append(&mut first.glyphs);
                first.glyphs = lead;
                first.width += width;
                let cap_text: String = paragraph.text.chars().take(1).collect();
                first.text = format!("{cap_text}{}", first.text);
            }
        }
        if broken.is_empty() {
            broken.push(Broken {
                glyphs: Vec::new(),
                width: 0.0,
                text: String::new(),
                hyphenated: false,
            });
        }
        if broken.iter().any(|line| line.hyphenated) {
            // The break inserted a hyphen the source text does not contain:
            // draw it. Its cluster is `HYPHEN_CLUSTER`, which maps to "-".
            let hyphen = self.face().shape("-", size, &features)?;
            for line in broken.iter_mut().filter(|line| line.hyphenated) {
                for glyph in &hyphen {
                    line.width += glyph.x_advance;
                    line.glyphs.push(Glyph {
                        cluster: HYPHEN_CLUSTER,
                        ..glyph.clone()
                    });
                }
            }
        }
        if !paragraph.note_mark.is_empty()
            && let Some(last_line) = broken.last_mut()
        {
            // The reference mark: superscript, after the last character.
            let mark_em = size * 0.6;
            for glyph in self
                .face()
                .shape(&paragraph.note_mark, mark_em, &features)?
            {
                last_line.width += glyph.x_advance;
                last_line.glyphs.push(Glyph {
                    cluster: NOTE_MARK_CLUSTER,
                    y_offset: glyph.y_offset + size * 0.35,
                    ..glyph
                });
            }
        }
        let note = match paragraph.note.as_ref() {
            Some(text) if !paragraph.note_is_endnote => {
                Some(Arc::new(self.note_lines(text, index)?))
            }
            _ => None,
        };
        let last = broken.len().saturating_sub(1) as u32;
        let keep_together = paragraph.style.keep_lines && broken.len() <= 3;
        let has_cap = drop.is_some();
        // Alignment: each line's offset in the measure, and for justified
        // lines the extra width each space takes.
        let mut offsets = Vec::with_capacity(broken.len());
        for (line_index, line) in broken.iter_mut().enumerate() {
            let own_indent = if has_cap && line_index == 0 {
                0.0
            } else {
                line_indent(line_index)
            };
            let available = measure - own_indent;
            let natural = natural_width(&paragraph.text, &line.glyphs);
            let slack = (available - natural).max(0.0);
            if line
                .glyphs
                .iter()
                .any(|glyph| is_tab(&paragraph.text, glyph))
            {
                // A tab takes the line's slack: what follows sits flush right.
                if let Some(tab) = line
                    .glyphs
                    .iter_mut()
                    .rev()
                    .find(|glyph| is_tab(&paragraph.text, glyph))
                {
                    tab.x_advance += slack;
                }
                offsets.push(0.0);
                continue;
            }
            let offset = match paragraph.style.align {
                Alignment::Center => slack / 2.0,
                Alignment::Right => slack,
                Alignment::Left => 0.0,
                Alignment::Justify => {
                    if line_index as u32 != last {
                        justify(&paragraph.text, &mut line.glyphs, slack, size);
                    }
                    0.0
                }
            };
            offsets.push(offset);
        }
        // A paragraph shorter than its drop cap still makes room for it.
        let cap_room = if has_cap && broken.len() < drop_lines {
            (drop_lines - broken.len()) as f32 * leading
        } else {
            0.0
        };
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
                let drop_cap = has_cap && line_index == 0;
                if line_index as u32 == last {
                    height += cap_room;
                }
                FlowLine {
                    paragraph: index,
                    line: line_index as u32,
                    height,
                    is_first: line_index == 0,
                    is_last: line_index as u32 == last,
                    // The lines beside a drop cap stay on one page with it.
                    keep_with_next: (paragraph.style.keep_with_next && line_index as u32 == last)
                        || (has_cap && line_index + 1 < drop_lines && (line_index as u32) < last),
                    keep_together,
                    widow_orphan: paragraph.style.widow_orphan,
                    drop_cap,
                    glyphs: Arc::new(line.glyphs),
                    width: line.width,
                    text: line.text,
                    indent: left
                        + offsets.get(line_index).copied().unwrap_or(0.0)
                        + if has_cap {
                            if line_index > 0 && line_index < drop_lines {
                                hang
                            } else {
                                0.0
                            }
                        } else {
                            line_indent(line_index)
                        },
                    note: if line_index == 0 { note.clone() } else { None },
                }
            })
            .collect())
    }

    /// A footnote broken into lines at footnote size on its paragraph's measure.
    fn note_lines(&self, text: &str, index: usize) -> Result<Vec<NoteLine>, TypesetError> {
        let em = self.note_em();
        let glyphs = self.face().shape(text, em, &features_for(false, false))?;
        let broken = break_lines(text, &glyphs, self.measure(index), &|_| 0.0, false, None);
        let mut lines = Vec::with_capacity(broken.len().max(1));
        for line in broken {
            // Clusters are rebased so each line's glyphs index its own text.
            let base = line
                .glyphs
                .iter()
                .map(|glyph| glyph.cluster)
                .min()
                .unwrap_or(0);
            let end = line
                .glyphs
                .iter()
                .map(|glyph| glyph.cluster as usize)
                .max()
                .map_or(base as usize, |last| {
                    text.get(last..)
                        .and_then(|rest| rest.chars().next())
                        .map_or(last, |ch| last + ch.len_utf8())
                });
            let own = text.get(base as usize..end).unwrap_or("").to_string();
            let glyphs = line
                .glyphs
                .into_iter()
                .map(|glyph| Glyph {
                    cluster: glyph.cluster - base,
                    ..glyph
                })
                .collect();
            lines.push(NoteLine {
                text: own,
                glyphs: Arc::new(glyphs),
            });
        }
        if lines.is_empty() {
            lines.push(NoteLine {
                text: String::new(),
                glyphs: Arc::new(Vec::new()),
            });
        }
        Ok(lines)
    }

    /// Footnote lines on 1-based `page`, with baselines: set from the
    /// bottom of the text block up, below a short rule.
    pub fn page_footnote_lines(&self, page: u32) -> Vec<FootnoteLine> {
        let Some(page) = self.pages.get(page.saturating_sub(1) as usize) else {
            return Vec::new();
        };
        let rules = self.rules(Self::page_paragraph(page));
        let bottom = self.geometry.page_height - rules.margin_bottom;
        let em = self.note_em();
        let leading = self.note_leading();
        let count = page.footnotes.len();
        page.footnotes
            .iter()
            .enumerate()
            .map(|(index, line)| FootnoteLine {
                text: line.text.clone(),
                glyphs: line.glyphs.as_ref().clone(),
                baseline: bottom - leading * (count - 1 - index) as f32 - em * 0.25,
                em,
            })
            .collect()
    }

    /// Where the rule above 1-based `page`'s footnotes sits, down from the
    /// page top, and how long it is. `None` when the page has no footnotes.
    pub fn page_footnote_rule(&self, page: u32) -> Option<(f32, f32)> {
        let index = page.saturating_sub(1) as usize;
        let data = self.pages.get(index)?;
        if data.footnotes.is_empty() {
            return None;
        }
        let rules = self.rules(Self::page_paragraph(data));
        let top = self.geometry.page_height
            - rules.margin_bottom
            - self.notes_height(data.footnotes.len())
            + self.note_leading() * 0.3;
        Some((top, self.measure(Self::page_paragraph(data)) / 3.0))
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
                    note_carry: Vec::new(),
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
                    .is_some_and(|page| page.note_carry.is_empty() && page.note_waiting.is_empty());
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

    /// Footnote em and leading, from the body's.
    fn note_em(&self) -> f32 {
        self.geometry.font_size * 0.8
    }

    fn note_leading(&self) -> f32 {
        self.geometry.leading * 0.8
    }

    /// Height of `count` footnote lines with the rule above them.
    fn notes_height(&self, count: usize) -> f32 {
        if count == 0 {
            0.0
        } else {
            self.note_leading() * (count as f32 + 0.6)
        }
    }

    /// Move pending note lines onto the page, carried lines first, while
    /// the footnote area stays within `max_height`.
    fn pour_notes(&self, flow: &mut NoteFlow, placed: &mut Vec<NoteLine>, max_height: f32) {
        loop {
            if flow.carry.is_empty() {
                if flow.waiting.is_empty() {
                    return;
                }
                let next = flow.waiting.remove(0);
                flow.carry = next.as_ref().clone();
            }
            if self.notes_height(placed.len() + 1) > max_height {
                return;
            }
            placed.push(flow.carry.remove(0));
        }
    }

    /// Fill one page from `start`. Body lines go on while they, and the
    /// footnotes their paragraphs reference, fit the text block. A footnote
    /// needs room for at least its first line on its reference page; the
    /// rest carries over. Notes carried from earlier pages come first, up to
    /// half the block, so the body still moves on.
    fn fill_page(&self, start: Cursor, flow: NoteFlow, built: usize) -> (Page, NoteFlow) {
        let mut lines = Vec::new();
        let mut used = 0.0;
        let mut cursor = start;
        let limit = self.block_height(start.paragraph);
        let verso = (built + 1).is_multiple_of(2);
        let mut flow = flow;
        let mut notes: Vec<NoteLine> = Vec::new();
        self.pour_notes(&mut flow, &mut notes, limit / 2.0);
        if notes.is_empty() && flow.pending() {
            // Carried notes always move on by at least a line, so a note
            // cannot be deferred forever by a page too small for half of it.
            if flow.carry.is_empty() && !flow.waiting.is_empty() {
                let next = flow.waiting.remove(0);
                flow.carry = next.as_ref().clone();
            }
            if !flow.carry.is_empty() {
                notes.push(flow.carry.remove(0));
            }
        }
        while !self.at_end(cursor) {
            let Some(line) = self.line_at(cursor) else {
                break;
            };
            let style = &self.paragraphs[line.paragraph].style;
            if self.refuses_recto(line, !lines.is_empty(), verso) {
                break;
            }
            let note_h = self.notes_height(notes.len());
            let brings_note = line.is_first && line.note.is_some();
            // A new note needs its first line on this page, unless earlier
            // notes are still waiting, in which case it queues behind them.
            let note_need = if brings_note && !flow.pending() {
                self.notes_height(notes.len() + 1) - note_h
            } else {
                0.0
            };
            if used + line.height + note_h + note_need > limit && !lines.is_empty() {
                break;
            }
            if style.widow_orphan && line.is_first && !line.is_last {
                let rest = self.paragraph_rest_height(cursor);
                if used > 0.0
                    && used + line.height + note_h + note_need <= limit
                    && used + rest + note_h > limit
                {
                    // Orphan: do not leave the first line of a paragraph alone
                    // at the bottom of the page.
                    break;
                }
            }
            if style.widow_orphan && !lines.is_empty() && !line.is_last {
                let next = step(cursor, &self.lines);
                if let Some(next_line) = self.line_at(next)
                    && next_line.is_last
                    && next_line.paragraph == line.paragraph
                    && used + line.height + next_line.height + note_h > limit
                {
                    // Widow: keep the last two lines together on the next page.
                    break;
                }
            }
            if line.keep_with_next && !lines.is_empty() {
                let next = step(cursor, &self.lines);
                let next_height = self
                    .line_at(next)
                    .map(|next_line| next_line.height)
                    .unwrap_or(self.geometry.leading);
                if used + line.height + next_height + note_h > limit {
                    break;
                }
            }
            if line.keep_together && line.is_first {
                let rest = self.paragraph_rest_height(cursor);
                if used > 0.0 && used + rest + note_h > limit {
                    break;
                }
            }
            used += line.height;
            let next = step(cursor, &self.lines);
            lines.push(line.clone());
            cursor = next;
            if let Some(note) = line.note.as_ref().filter(|_| line.is_first) {
                flow.waiting.push(note.clone());
                self.pour_notes(&mut flow, &mut notes, limit - used);
            }
        }
        if lines.is_empty()
            && let Some(line) = self.line_at(start)
        {
            // Do not undo a recto refusal: an empty verso must stay empty
            // of the chapter that is waiting for an odd page.
            if !self.refuses_recto(line, false, verso) {
                lines.push(line.clone());
                if let Some(note) = line.note.as_ref().filter(|_| line.is_first) {
                    flow.waiting.push(note.clone());
                    self.pour_notes(&mut flow, &mut notes, limit - line.height);
                }
            }
        }
        (
            Page {
                start,
                lines,
                footnotes: notes,
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

    fn each_paragraph_is_one_page(&self) -> bool {
        if self.hints.recto_at.iter().any(|flag| *flag)
            || self.hints.break_at.iter().any(|flag| *flag)
            || self.hints.masters.len() > 1
        {
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
                let mut next = flow;
                let mut footnotes = Vec::new();
                let last = self.paragraphs.len().saturating_sub(1);
                let limit = self.block_height(cursor.paragraph.min(last));
                self.pour_notes(&mut next, &mut footnotes, limit);
                if footnotes.is_empty() {
                    // A note line taller than the block still has to go somewhere.
                    if next.carry.is_empty() && !next.waiting.is_empty() {
                        let first = next.waiting.remove(0);
                        next.carry = first.as_ref().clone();
                    }
                    if !next.carry.is_empty() {
                        footnotes.push(next.carry.remove(0));
                    }
                }
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

    /// A chapter that starts a new page does not join a page that already
    /// has lines, and a recto chapter does not open on an empty verso either.
    /// [`page_decision`] is what emits the blank.
    fn refuses_recto(&self, line: &FlowLine, page_has_lines: bool, verso: bool) -> bool {
        if !line.is_first {
            return false;
        }
        let flag = |flags: &[bool]| flags.get(line.paragraph).copied().unwrap_or(false);
        let breaks = flag(&self.hints.break_at);
        let recto = flag(&self.hints.recto_at);
        ((breaks || recto) && page_has_lines) || (recto && verso)
    }

    fn note_page(&self, start: Cursor, footnotes: Vec<NoteLine>, flow: &NoteFlow) -> Page {
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
        let facing = self.hints.facing;
        let mut carried = String::new();
        let rules: Vec<PageRules> = self
            .pages
            .iter()
            .map(|page| self.rules(Self::page_paragraph(page)))
            .collect();
        for (index, page) in self.pages.iter_mut().enumerate() {
            let rule = rules[index];
            let page_no = index as u32 + 1;
            let verso = facing && page_no.is_multiple_of(2);
            page.content_inset = if verso {
                rule.margin_outer
            } else {
                rule.margin_inner
            };
            if !rule.folio && !rule.running_head {
                continue;
            }
            if let Some(line) = page.lines.first()
                && let Some(head) = self.hints.heads.get(line.paragraph)
                && !head.is_empty()
            {
                carried = head.clone();
            }
            let opener = page.lines.first().is_some_and(|line| {
                let flag = |flags: &[bool]| flags.get(line.paragraph).copied().unwrap_or(false);
                line.is_first && (flag(&self.hints.recto_at) || flag(&self.hints.break_at))
            }) || (index == 0 && rule.hide_opener_folio);
            if rule.folio && !(opener && rule.hide_opener_folio) {
                page.folio = Some(page_no.to_string());
            }
            if rule.running_head && !carried.is_empty() && !opener {
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
    /// Broken inside a word: the line ends with an inserted hyphen.
    hyphenated: bool,
}

/// Break `glyphs` into lines of `measure`, less `indent(n)` on line `n`.
fn break_lines(
    text: &str,
    glyphs: &[Glyph],
    measure: f32,
    indent: &dyn Fn(usize) -> f32,
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
    let mut limit = (measure - indent(0)).max(1.0);
    for (index, glyph) in glyphs.iter().enumerate() {
        let cluster = glyph.cluster as usize;
        if index > start
            && let Some(opportunity) = breaks.get(&cluster)
        {
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
                limit = (measure - indent(lines.len())).max(1.0);
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
            limit = (measure - indent(lines.len())).max(1.0);
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

/// Width of a line's glyphs without the spaces it ends on.
fn natural_width(text: &str, glyphs: &[Glyph]) -> f32 {
    let trailing = glyphs
        .iter()
        .rev()
        .take_while(|glyph| is_space(text, glyph))
        .map(|glyph| glyph.x_advance)
        .sum::<f32>();
    width_of(glyphs) - trailing
}

fn is_tab(text: &str, glyph: &Glyph) -> bool {
    glyph.cluster < NOTE_MARK_CLUSTER
        && text
            .get(glyph.cluster as usize..)
            .is_some_and(|rest| rest.starts_with('\t'))
}

fn is_space(text: &str, glyph: &Glyph) -> bool {
    glyph.cluster < NOTE_MARK_CLUSTER
        && text
            .get(glyph.cluster as usize..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|ch| ch == ' ' || ch == '\u{a0}')
}

/// Spread `slack` over the inner spaces of a line (not the ones it ends on).
/// A line with so few spaces that each would open wider than two ems is
/// left ragged rather than torn apart.
fn justify(text: &str, glyphs: &mut [Glyph], slack: f32, em: f32) {
    let end = glyphs.len()
        - glyphs
            .iter()
            .rev()
            .take_while(|glyph| is_space(text, glyph))
            .count();
    // The spaces a justified line ends on take no width: the right edge is
    // the last letter's.
    for glyph in &mut glyphs[end..] {
        glyph.x_advance = 0.0;
    }
    let spaces: Vec<usize> = (0..end)
        .filter(|index| is_space(text, &glyphs[*index]))
        .collect();
    if spaces.is_empty() || slack <= 0.0 {
        return;
    }
    let each = slack / spaces.len() as f32;
    if each > em * 2.0 {
        return;
    }
    for index in spaces {
        glyphs[index].x_advance += each;
    }
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
        hyphenated: hyphen,
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
            note_mark: String::new(),
            runs: Vec::new(),
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
            note_mark: String::new(),
            runs: Vec::new(),
        })
        .collect();
    Document::new(face, Geometry::one_line_pages(), paragraphs)
}

#[cfg(test)]
mod page_decision_tests {
    use super::{NoteFlow, PageStep, page_decision};

    #[test]
    fn a_verso_stays_blank_while_a_note_carry_is_pending() {
        let line = |text: &str| super::NoteLine {
            text: text.to_string(),
            glyphs: std::sync::Arc::new(Vec::new()),
        };
        let pending = NoteFlow {
            carry: vec![line("N")],
            waiting: vec![std::sync::Arc::new(vec![line("Second note body.")])],
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
