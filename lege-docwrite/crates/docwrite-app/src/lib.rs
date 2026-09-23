//! Editor behavior the window hosts: paging, snap, painting, and the latency budget.

mod nav;

use std::time::Duration;

pub use nav::{Pager, Phase};

use docwrite_model::Book;
use docwrite_typeset::{Document, Face, Geometry, GlyphAtlas, Paragraph, ParagraphStyle};

const DESK: u32 = 0x00E6_E1D6;
const PAGE_COLOR: u32 = 0x00FF_FBF4;
const INK: u32 = 0x001C_1916;

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
        }
    }

    /// Insert text the input method finished composing.
    pub fn commit_ime(&mut self, text: &str) {
        self.preedit.clear();
        if !text.is_empty() {
            let _ = self.book.insert(text);
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
        let page_h = (height * 7 / 10).max(1);
        let page_w = (page_h * 2 / 3).max(1);
        let gap = 24;
        let scroll = self.pager.scroll();
        let origin = (height / 2) - ((scroll.fract() * page_h as f64) as i32);
        let first = scroll.floor() as i32;
        let document = self.laid_out(page_w, page_h);
        let mut painter = pixelkit_raster::Painter::new(buffer);
        for slot in -1..4 {
            let page_index = first + slot;
            if page_index < 0 {
                continue;
            }
            let top = origin + slot * (page_h + gap);
            let left = (width - page_w) / 2;
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

    fn laid_out(&self, page_w: i32, page_h: i32) -> Option<Document> {
        let face = self.face.as_ref()?.duplicate().ok()?;
        let geometry = Geometry {
            page_width: page_w as f32,
            page_height: page_h as f32,
            margin_top: 48.0,
            margin_bottom: 48.0,
            margin_inner: 40.0,
            margin_outer: 40.0,
            font_size: 18.0,
            leading: 24.0,
        };
        Document::new(face, geometry, paragraphs_of(&self.book, &self.preedit)).ok()
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
        painter.push_clip(pixelkit_raster::Rect::new(left, top, page_w, page_h));
        let lines = document.page_line_glyphs(page);
        let mut caret_x = left + geometry.margin_inner as i32;
        let mut caret_y = top + geometry.margin_top as i32;
        let face = document.face();
        for (index, line) in lines.iter().enumerate() {
            let baseline = top
                + geometry.margin_top as i32
                + geometry.font_size as i32
                + index as i32 * geometry.leading as i32;
            let mut pen = left + geometry.margin_inner as i32;
            for glyph in line {
                let size = if glyph.em > 0.0 {
                    glyph.em
                } else {
                    geometry.font_size
                };
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
                pen += glyph.x_advance.round() as i32;
            }
            caret_x = pen;
            caret_y = baseline - geometry.font_size as i32;
        }
        if page == document.page_count() {
            let height = geometry.font_size.max(1.0) as i32;
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

    pub fn book(&self) -> &Book {
        &self.book
    }

    pub fn pager(&self) -> &Pager {
        &self.pager
    }

    pub fn type_text(&mut self, text: &str) {
        let _ = self.book.insert(text);
    }

    pub fn backspace(&mut self) {
        let _ = self.book.delete_backward();
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

fn paragraphs_of(book: &Book, preedit: &str) -> Vec<Paragraph> {
    let mut paragraphs: Vec<Paragraph> = book
        .blocks()
        .enumerate()
        .map(|(index, block)| Paragraph {
            id: index as u64 + 1,
            text: block.text(),
            style: ParagraphStyle {
                hyphenate: true,
                widow_orphan: true,
                ..ParagraphStyle::default()
            },
            note: None,
            note_is_endnote: false,
        })
        .collect();
    if !preedit.is_empty() {
        if let Some(last) = paragraphs.last_mut() {
            last.text.push_str(preedit);
        } else {
            paragraphs.push(Paragraph {
                id: 1,
                text: preedit.to_string(),
                style: ParagraphStyle::default(),
                note: None,
                note_is_endnote: false,
            });
        }
    }
    if paragraphs.is_empty() {
        paragraphs.push(Paragraph {
            id: 1,
            text: String::new(),
            style: ParagraphStyle::default(),
            note: None,
            note_is_endnote: false,
        });
    }
    paragraphs
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
