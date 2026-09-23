//! Editor behavior the window hosts: paging, snap, painting, and the latency budget.

mod map;
mod nav;

use std::path::PathBuf;
use std::time::Duration;

pub use map::{BookMap, MapKind, MapRow, SIDEBAR_W};
pub use nav::{Pager, Phase};

use docwrite_model::{Book, Direction, Motion};
use docwrite_typeset::{Document, Face, GlyphAtlas, from_book};
use map::BookMap as Map;

const DESK: u32 = 0x00E6_E1D6;
const PAGE_COLOR: u32 = 0x00FF_FBF4;
const INK: u32 = 0x001C_1916;
/// Selection highlight painted behind selected glyphs.
pub const SELECTION: u32 = 0x00B7_D0F5;

/// Caret rectangle in window pixels, for the IME candidate window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The manuscript and the page viewport the window draws.
pub struct Editor {
    book: Book,
    pager: Pager,
    face: Option<Face>,
    atlas: GlyphAtlas,
    preedit: String,
    caret: Option<CaretRect>,
    map: Map,
    cursor: (f32, f32),
    pressed_row: Option<usize>,
    bundle: Option<PathBuf>,
    dirty: bool,
}

impl Editor {
    pub fn new() -> Self {
        let mut book = Book::new("Untitled");
        let _ = book.insert("The book opens on a page.");
        Self {
            book,
            pager: Pager::new(8, false),
            face: load_face(),
            atlas: GlyphAtlas::new(),
            preedit: String::new(),
            caret: None,
            map: Map::new(),
            cursor: (0.0, 0.0),
            pressed_row: None,
            bundle: None,
            dirty: false,
        }
    }

    /// Remember where the pointer is. The window calls this on cursor motion.
    pub fn hover(&mut self, x: f32, y: f32) {
        self.cursor = (x, y);
    }

    /// Press or release the pointer. A drag from one chapter row to another
    /// reorders the book. A click collapses the part or chapter under the pointer.
    pub fn pointer(&mut self, pressed: bool) {
        let rows = self.map.rows(&self.book);
        let hit = self.map.row_at(self.cursor.0, self.cursor.1, rows.len());
        if pressed {
            self.pressed_row = hit;
            return;
        }
        let Some(from) = self.pressed_row.take() else {
            return;
        };
        let Some(to) = hit else {
            return;
        };
        if from == to {
            self.map.activate(&self.book, from);
        } else if self.map.drag_chapter(&mut self.book, from, to).is_ok() {
            self.dirty = true;
        }
    }

    /// Sidebar rows in the order the window paints them.
    pub fn map_rows(&self) -> Vec<MapRow> {
        self.map.rows(&self.book)
    }

    /// Center of a sidebar row, matching [`BookMap::row_center`].
    pub fn map_row_center(index: usize) -> (f32, f32) {
        BookMap::row_center(index)
    }

    /// Bind autosave and snapshots to `path`. Typing does not write the bundle.
    pub fn open_bundle(&mut self, path: impl Into<PathBuf>) {
        self.bundle = Some(path.into());
    }

    /// Write the bundle if an edit has landed since the last save.
    pub fn autosave(&mut self) -> Result<(), String> {
        let Some(path) = self.bundle.clone() else {
            return Err("no bundle".into());
        };
        if !self.dirty {
            return Ok(());
        }
        self.book
            .save_bundle(&path)
            .map_err(|err| err.to_string())?;
        self.dirty = false;
        Ok(())
    }

    /// Store a named snapshot of the manuscript as it is now.
    pub fn save_named_snapshot(&mut self, name: &str) -> Result<(), String> {
        let path = self.bundle.clone().ok_or_else(|| "no bundle".to_string())?;
        self.book
            .save_snapshot(&path, name)
            .map_err(|err| err.to_string())
    }

    /// Replace the open manuscript with a named snapshot.
    pub fn restore_named_snapshot(&mut self, name: &str) -> Result<(), String> {
        let path = self.bundle.clone().ok_or_else(|| "no bundle".to_string())?;
        self.book = Book::load_snapshot(&path, name).map_err(|err| err.to_string())?;
        self.dirty = false;
        Ok(())
    }

    pub fn move_left(&mut self) {
        let _ = self.book.move_caret(Motion::Char(Direction::Backward));
    }

    pub fn move_right(&mut self) {
        let _ = self.book.move_caret(Motion::Char(Direction::Forward));
    }

    pub fn extend_left(&mut self) {
        let _ = self
            .book
            .extend_selection(Motion::Char(Direction::Backward));
    }

    pub fn extend_right(&mut self) {
        let _ = self.book.extend_selection(Motion::Char(Direction::Forward));
    }

    /// Insert text the input method finished composing.
    pub fn commit_ime(&mut self, text: &str) {
        self.preedit.clear();
        if !text.is_empty() {
            let _ = self.book.insert(text);
            self.dirty = true;
        }
    }

    /// Show an in-progress composition without changing the manuscript.
    pub fn set_preedit(&mut self, text: &str) {
        self.preedit = text.to_string();
    }

    /// The composition the window is showing, if any.
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// Caret rectangle from the last [`Self::paint`], in window pixels.
    pub fn caret_area(&self) -> Option<CaretRect> {
        self.caret
    }

    /// Draw the desk, the page stack, and the manuscript on those pages.
    pub fn paint(&mut self, buffer: &mut pixelkit_raster::WindowBuffer) {
        self.caret = None;
        for pixel in buffer.pixels.iter_mut() {
            *pixel = DESK;
        }
        let width = buffer.width as i32;
        let height = buffer.height as i32;
        if width <= 0 || height <= 0 {
            return;
        }
        let content_left = SIDEBAR_W.min(width - 1);
        let content_w = (width - content_left).max(1);
        let page_h = (height * 7 / 10).max(1);
        let page_w = (page_h * 2 / 3).min(content_w - 16).max(1);
        let gap = 24;
        let scroll = self.pager.scroll();
        let origin = (height / 2) - ((scroll.fract() * page_h as f64) as i32);
        let first = scroll.floor() as i32;
        let document = self.laid_out();
        let mut painter = pixelkit_raster::Painter::new(buffer);
        for slot in -1..4 {
            let page_index = first + slot;
            if page_index < 0 {
                continue;
            }
            let top = origin + slot * (page_h + gap);
            let left = content_left + (content_w - page_w) / 2;
            painter.fill_rect(
                pixelkit_raster::Rect::new(left, top, page_w, page_h),
                PAGE_COLOR,
            );
            if let Some(document) = document.as_ref() {
                self.paint_page(
                    &mut painter,
                    document,
                    page_index as u32 + 1,
                    left,
                    top,
                    page_w,
                    page_h,
                );
            }
        }
        self.paint_sidebar(&mut painter, height);
    }

    /// How many painted pixels equal `color`.
    pub fn pixels_of(&mut self, width: u32, height: u32, color: u32) -> usize {
        let mut buffer = pixelkit_raster::WindowBuffer::new(width, height);
        self.paint(&mut buffer);
        buffer
            .pixels
            .iter()
            .filter(|pixel| **pixel == color)
            .count()
    }

    /// How many pixels of the painted window are neither the desk nor the page.
    pub fn painted_ink(&mut self, width: u32, height: u32) -> usize {
        let mut buffer = pixelkit_raster::WindowBuffer::new(width, height);
        self.paint(&mut buffer);
        buffer
            .pixels
            .iter()
            .filter(|pixel| **pixel != DESK && **pixel != PAGE_COLOR)
            .count()
    }

    fn laid_out(&self) -> Option<Document> {
        let face = self.face.as_ref()?.duplicate().ok()?;
        let mut book = self.book.clone();
        if !self.preedit.is_empty() {
            let _ = book.insert(&self.preedit);
        }
        from_book(&book, face).ok()
    }

    fn paint_page(
        &mut self,
        painter: &mut pixelkit_raster::Painter<'_>,
        document: &Document,
        page: u32,
        left: i32,
        top: i32,
        page_w: i32,
        page_h: i32,
    ) {
        if page == 0 || page > document.page_count() {
            return;
        }
        let geometry = document.geometry();
        let scale = (page_w as f32 / geometry.page_width.max(1.0))
            .min(page_h as f32 / geometry.page_height.max(1.0))
            .max(0.05);
        painter.push_clip(pixelkit_raster::Rect::new(left, top, page_w, page_h));
        if let Some(head) = document.page_running_head(page) {
            self.paint_label(
                painter,
                document.face(),
                &head,
                left + (geometry.margin_inner * scale) as i32,
                top + (12.0 * scale) as i32,
                10.0 * scale,
            );
        }
        if let Some(folio) = document.page_folio(page) {
            self.paint_label(
                painter,
                document.face(),
                &folio,
                left + page_w / 2,
                top + page_h - (18.0 * scale) as i32,
                10.0 * scale,
            );
        }
        let lines = document.page_painted_lines(page);
        let selected = selected_spans(&self.book);
        let focus = focus_mark(&self.book);
        let mut caret_x = left + geometry.margin_inner as i32;
        let mut caret_y = top + geometry.margin_top as i32;
        let mut saw_focus = false;
        let face = document.face();
        for (index, line) in lines.iter().enumerate() {
            let baseline = top
                + ((geometry.margin_top + geometry.font_size) * scale) as i32
                + (index as f32 * geometry.leading * scale) as i32;
            let mut pen = left + ((geometry.margin_inner + line.indent) * scale) as i32;
            if let Some((paragraph, focus_byte)) = focus {
                if paragraph == line.paragraph {
                    saw_focus = true;
                    let mut mark = pen;
                    for glyph in &line.glyphs {
                        if glyph.cluster as usize >= focus_byte {
                            break;
                        }
                        mark += (glyph.x_advance * scale).round() as i32;
                    }
                    caret_x = mark;
                    caret_y = baseline - (geometry.font_size * scale) as i32;
                }
            }
            for glyph in &line.glyphs {
                let advance = (glyph.x_advance * scale).round() as i32;
                if span_covers(&selected, line.paragraph, glyph.cluster as usize) {
                    painter.fill_rect(
                        pixelkit_raster::Rect::new(
                            pen,
                            baseline - (geometry.font_size * scale) as i32,
                            advance.max(1),
                            (geometry.leading * scale).max(1.0) as i32,
                        ),
                        SELECTION,
                    );
                }
                let size = (if glyph.em > 0.0 {
                    glyph.em
                } else {
                    geometry.font_size
                }) * scale;
                let origin_y = baseline - 48;
                let _ = self
                    .atlas
                    .with_coverage(face, glyph.id, size, 0.0, |coverage, stride| {
                        if stride == 0 {
                            return;
                        }
                        let rows = coverage.len() as u32 / stride;
                        for row in 0..rows {
                            let start = (row * stride) as usize;
                            let end = start + stride as usize;
                            if end <= coverage.len() {
                                painter.blend_coverage_row(
                                    pen,
                                    origin_y + row as i32,
                                    INK,
                                    &coverage[start..end],
                                );
                            }
                        }
                    });
                pen += advance;
            }
        }
        if saw_focus {
            let height = (geometry.font_size * scale).max(1.0) as i32;
            painter.fill_rect(pixelkit_raster::Rect::new(caret_x, caret_y, 2, height), INK);
            self.caret = Some(CaretRect {
                x: caret_x as f64,
                y: caret_y as f64,
                width: 2.0,
                height: height as f64,
            });
        }
        painter.pop_clip();
    }

    fn paint_label(
        &mut self,
        painter: &mut pixelkit_raster::Painter<'_>,
        face: &Face,
        text: &str,
        x: i32,
        y: i32,
        size: f32,
    ) {
        let Ok(glyphs) = face.shape(text, size.max(1.0), &[]) else {
            return;
        };
        let mut pen = x;
        for glyph in glyphs {
            let origin_y = y - 48;
            let _ =
                self.atlas
                    .with_coverage(face, glyph.id, size.max(1.0), 0.0, |coverage, stride| {
                        if stride == 0 {
                            return;
                        }
                        let rows = coverage.len() as u32 / stride;
                        for row in 0..rows {
                            let start = (row * stride) as usize;
                            let end = start + stride as usize;
                            if end <= coverage.len() {
                                painter.blend_coverage_row(
                                    pen,
                                    origin_y + row as i32,
                                    INK,
                                    &coverage[start..end],
                                );
                            }
                        }
                    });
            pen += glyph.x_advance.round() as i32;
        }
    }

    fn paint_sidebar(&self, painter: &mut pixelkit_raster::Painter<'_>, height: i32) {
        painter.fill_rect(
            pixelkit_raster::Rect::new(0, 0, SIDEBAR_W, height),
            0x00DD_D4C8,
        );
        for (index, row) in self.map.rows(&self.book).iter().enumerate() {
            let (_, y) = BookMap::row_center(index);
            let color = match row.kind {
                MapKind::Chapter => 0x00EF_E8DC,
                MapKind::Section => 0x00F6_F1E8,
                MapKind::FrontMatter | MapKind::Part | MapKind::BackMatter => 0x00D0_C6B8,
            };
            painter.fill_rect(
                pixelkit_raster::Rect::new(8, y as i32 - 12, SIDEBAR_W - 16, 24),
                color,
            );
        }
    }

    pub fn book(&self) -> &Book {
        &self.book
    }

    pub fn book_mut(&mut self) -> &mut Book {
        &mut self.book
    }

    pub fn pager(&self) -> &Pager {
        &self.pager
    }

    pub fn type_text(&mut self, text: &str) {
        let _ = self.book.insert(text);
        self.dirty = true;
    }

    pub fn backspace(&mut self) {
        let _ = self.book.delete_backward();
        self.dirty = true;
    }

    pub fn page_down(&mut self) {
        self.pager.page_down();
    }

    pub fn page_up(&mut self) {
        self.pager.page_up();
    }

    pub fn scroll_gesture(&mut self, delta: f64, phase: Phase) {
        self.pager.scroll_gesture(delta, phase);
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

/// Keypress-to-present budget. A few milliseconds, inside one 60 Hz frame.
pub const KEYPRESS_BUDGET: Duration = Duration::from_millis(8);

fn load_face() -> Option<Face> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("LEGE_DOCWRITE_FONT") {
        paths.push(std::path::PathBuf::from(path));
    }
    paths.push(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../pixelkit/crates/pixelkit-text/test-fonts/NotoSans-Regular.ttf"),
    );
    paths.push(std::path::PathBuf::from(
        "/System/Library/Fonts/Supplemental/Arial.ttf",
    ));
    paths.push(std::path::PathBuf::from("/Library/Fonts/Arial.ttf"));
    paths.push(std::path::PathBuf::from(
        "/System/Library/Fonts/Supplemental/Times New Roman.ttf",
    ));
    for path in paths {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Ok(face) = Face::parse(bytes) {
            return Some(face);
        }
    }
    None
}

fn byte_at(text: &str, chars: usize) -> usize {
    text.chars().take(chars).map(|ch| ch.len_utf8()).sum()
}

fn focus_mark(book: &Book) -> Option<(usize, usize)> {
    let focus = book.selection().focus;
    let index = book.block_ids().iter().position(|id| *id == focus.block)?;
    let text = book.block(focus.block).ok()?.text();
    Some((index, byte_at(&text, focus.offset)))
}

fn selected_spans(book: &Book) -> Vec<(usize, usize, usize)> {
    let ids = book.block_ids();
    let selection = book.selection();
    let anchor_i = ids.iter().position(|id| *id == selection.anchor.block);
    let focus_i = ids.iter().position(|id| *id == selection.focus.block);
    let (Some(anchor_i), Some(focus_i)) = (anchor_i, focus_i) else {
        return Vec::new();
    };
    let (from_i, from_off, to_i, to_off) =
        if (anchor_i, selection.anchor.offset) <= (focus_i, selection.focus.offset) {
            (
                anchor_i,
                selection.anchor.offset,
                focus_i,
                selection.focus.offset,
            )
        } else {
            (
                focus_i,
                selection.focus.offset,
                anchor_i,
                selection.anchor.offset,
            )
        };
    if from_i == to_i && from_off == to_off {
        return Vec::new();
    }
    let mut spans = Vec::new();
    for index in from_i..=to_i {
        let Some(id) = ids.get(index).copied() else {
            continue;
        };
        let Ok(len) = book.block_len(id) else {
            continue;
        };
        let start = if index == from_i {
            from_off.min(len)
        } else {
            0
        };
        let end = if index == to_i { to_off.min(len) } else { len };
        if start >= end {
            continue;
        }
        let Ok(block) = book.block(id) else {
            continue;
        };
        spans.push((
            index,
            byte_at(&block.text(), start),
            byte_at(&block.text(), end),
        ));
    }
    spans
}

fn span_covers(spans: &[(usize, usize, usize)], paragraph: usize, byte: usize) -> bool {
    spans
        .iter()
        .any(|(index, start, end)| *index == paragraph && byte >= *start && byte < *end)
}

/// True when opening a window is not expected to succeed.
pub fn display_unavailable() -> bool {
    if std::env::var_os("LEGE_DOCWRITE_HEADLESS").is_some() {
        return true;
    }
    cfg!(unix)
        && !cfg!(target_os = "macos")
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
}
