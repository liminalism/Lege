//! The research pane: a source PDF beside the manuscript. Select a passage
//! by dragging over its words, capture it as a Source Note, and quote or
//! cite it at the caret. A citation in the manuscript opens its source at
//! the page it came from, with the passage highlighted.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use docwrite_model::SourceNote;
use docwrite_typeset::Face;
use lege_pdf_read::{NativeTextWord, RasterPlane, RasterProduct, RenderSession};
use pixelkit_raster::{Bitmap, Rect, ScaleFilter};

use crate::Editor;
use crate::toolbar::PromptKind;

/// Pane width in logical pixels.
const PANE_W: f32 = 460.0;
/// Header height in logical pixels.
const HEADER_H: f32 = 36.0;

/// An open source document.
pub(crate) struct Research {
    path: PathBuf,
    session: RenderSession,
    /// 0-based page shown.
    page: u32,
    pages: u32,
    /// The page's raster at the size it is shown, with that size.
    raster: Option<(u32, u32, u32, Bitmap)>,
    /// Words of the shown page in raster pixels.
    words: Vec<NativeTextWord>,
    /// Selected words, as an inclusive index range.
    selected: Option<(usize, usize)>,
    drag_from: Option<usize>,
    /// Passage rectangles to show, as fractions of the page (from a citation).
    highlight: Vec<[f32; 4]>,
    /// The source last captured or cited, for Quote and Cite.
    last_source: Option<String>,
}

impl std::fmt::Debug for Research {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Research")
            .field("path", &self.path)
            .field("page", &self.page)
            .field("pages", &self.pages)
            .finish_non_exhaustive()
    }
}

/// Something the pane's header can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAction {
    Previous,
    Next,
    Capture,
    Quote,
    Cite,
    Close,
}

impl Editor {
    /// Open the PDF at `path` beside the manuscript.
    pub fn open_source(&mut self, path: impl Into<PathBuf>) -> Result<(), String> {
        let path = path.into();
        let bytes = std::fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let session =
            RenderSession::open(Arc::from(bytes), None).map_err(|err| format!("{err:?}"))?;
        let pages = session.page_count();
        self.research = Some(Research {
            path,
            session,
            page: 0,
            pages,
            raster: None,
            words: Vec::new(),
            selected: None,
            drag_from: None,
            highlight: Vec::new(),
            last_source: None,
        });
        Ok(())
    }

    /// Close the research pane.
    pub fn close_source(&mut self) {
        self.research = None;
    }

    /// The open source's path and 0-based page, if a source is open.
    pub fn source_view(&self) -> Option<(&Path, u32)> {
        self.research
            .as_ref()
            .map(|research| (research.path.as_path(), research.page))
    }

    /// Show 0-based `page` of the open source.
    pub fn source_page(&mut self, page: u32) {
        if let Some(research) = self.research.as_mut() {
            research.page = page.min(research.pages.saturating_sub(1));
            research.selected = None;
            research.highlight.clear();
        }
    }

    /// The selected passage's text in the open source.
    pub fn source_selection(&self) -> Option<String> {
        let research = self.research.as_ref()?;
        let (from, to) = research.selected?;
        let words: Vec<&str> = research
            .words
            .get(from..=to)?
            .iter()
            .map(|word| word.text.as_str())
            .collect();
        Some(words.join(" "))
    }

    /// Select the words from index `from` to `to` (inclusive) of the shown page.
    pub fn select_source_words(&mut self, from: usize, to: usize) {
        if let Some(research) = self.research.as_mut() {
            let last = research.words.len().saturating_sub(1);
            research.selected = Some((from.min(to).min(last), from.max(to).min(last)));
        }
    }

    /// Words on the shown page of the source, in reading order.
    pub fn source_words(&mut self) -> Vec<String> {
        self.ensure_source_page(self.pane_size().0, self.pane_size().1);
        self.research
            .as_ref()
            .map(|research| {
                research
                    .words
                    .iter()
                    .map(|word| word.text.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Rectangles highlighted on the shown page, as page fractions.
    pub fn source_highlight(&self) -> &[[f32; 4]] {
        self.research
            .as_ref()
            .map_or(&[], |research| research.highlight.as_slice())
    }

    /// Keep the selected passage as a Source Note and ask for its citation.
    /// Returns the new source's id.
    pub fn capture_source(&mut self) -> Option<String> {
        let passage = self.source_selection()?;
        let research = self.research.as_ref()?;
        let (from, to) = research.selected?;
        let (width, height, _) = research
            .raster
            .as_ref()
            .map(|(_, width, height, _)| (*width, *height, ()))?;
        let rects = research.words[from..=to]
            .iter()
            .map(|word| {
                [
                    word.bbox[0] / width as f32,
                    word.bbox[1] / height as f32,
                    word.bbox[2] / width as f32,
                    word.bbox[3] / height as f32,
                ]
            })
            .collect();
        let number = self.book.sources().len() + 1;
        let mut id = format!("src{number}");
        while self.book.source(&id).is_some() {
            id.push('a');
        }
        let stem = research
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let citation = format!("{stem}, p. {}", research.page + 1);
        self.book.add_source(SourceNote {
            id: id.clone(),
            document: research.path.to_string_lossy().into_owned(),
            page: research.page,
            rects,
            passage,
            citation: citation.clone(),
            annotation: String::new(),
        });
        if let Some(research) = self.research.as_mut() {
            // The passage is kept now; Quote and Cite act on it from here.
            research.last_source = Some(id.clone());
            research.selected = None;
            research.highlight = self
                .book
                .source(&id)
                .map(|source| source.rects.clone())
                .unwrap_or_default();
        }
        self.dirty = true;
        self.open_prompt(PromptKind::Citation);
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.text = citation;
        }
        Some(id)
    }

    /// Set the citation text of the last captured source (from the prompt).
    pub(crate) fn set_last_citation(&mut self, text: &str) {
        let Some(id) = self
            .research
            .as_ref()
            .and_then(|research| research.last_source.clone())
        else {
            return;
        };
        if let Some(mut source) = self.book.source(&id).cloned() {
            if !text.trim().is_empty() {
                source.citation = text.trim().to_string();
            }
            self.book.update_source(source);
            self.dirty = true;
        }
    }

    /// Quote the last captured source at the caret, then cite it.
    pub fn quote_source(&mut self) {
        let id = self.current_source_id();
        if let Some(id) = id
            && self.book.quote(&id).is_ok()
        {
            self.edited();
        }
    }

    /// Cite the last captured source at the caret.
    pub fn cite_source(&mut self) {
        let id = self.current_source_id();
        if let Some(id) = id
            && self.book.cite(&id).is_ok()
        {
            self.edited();
        }
    }

    /// The source Quote and Cite act on: the selection, captured first if
    /// it has not been, or else the last one captured.
    fn current_source_id(&mut self) -> Option<String> {
        let selected = self
            .research
            .as_ref()
            .is_some_and(|research| research.selected.is_some());
        if selected && self.prompt.is_none() {
            let id = self.capture_source();
            self.prompt = None;
            if let Some(research) = self.research.as_mut() {
                research.selected = None;
            }
            return id;
        }
        self.research
            .as_ref()
            .and_then(|research| research.last_source.clone())
    }

    /// If the caret is on a citation, open its source at its page with the
    /// passage highlighted. Returns whether it did.
    pub fn follow_citation(&mut self) -> bool {
        let Some(source) = self.book.citation_at_caret().cloned() else {
            return false;
        };
        let already = self
            .research
            .as_ref()
            .is_some_and(|research| research.path == Path::new(&source.document));
        if !already && self.open_source(&source.document).is_err() {
            return false;
        }
        self.source_page(source.page);
        if let Some(research) = self.research.as_mut() {
            research.highlight = source.rects.clone();
            research.last_source = Some(source.id.clone());
        }
        true
    }

    /// Research pane width in window pixels; zero when no source is open.
    pub(crate) fn pane_width(&self) -> i32 {
        if self.research.is_none() || self.fullscreen {
            0
        } else {
            (PANE_W * self.ui_scale).round() as i32
        }
    }

    fn pane_size(&self) -> (u32, u32) {
        let s = self.ui_scale;
        let width = (self.pane_width() as f32 - 24.0 * s).max(1.0) as u32;
        let height =
            (self.frame_height as f32 - self.toolbar_height() as f32 - HEADER_H * s - 24.0 * s)
                .max(1.0) as u32;
        (width, height)
    }

    /// Render the shown page to fit `max_w` x `max_h` and read its words,
    /// unless that is already done.
    fn ensure_source_page(&mut self, max_w: u32, max_h: u32) {
        let Some(research) = self.research.as_mut() else {
            return;
        };
        let Ok(geometry) = research.session.page_geometry(research.page) else {
            return;
        };
        let (page_w, page_h) = (geometry.width().max(1.0), geometry.height().max(1.0));
        let scale = (f64::from(max_w) / page_w).min(f64::from(max_h) / page_h);
        let width = (page_w * scale).max(1.0) as u32;
        let height = (page_h * scale).max(1.0) as u32;
        if research
            .raster
            .as_ref()
            .is_some_and(|(page, w, h, _)| *page == research.page && *w == width && *h == height)
        {
            return;
        }
        let Ok(compiled) = research.session.compile(research.page) else {
            return;
        };
        let Ok(plane) = research
            .session
            .render(&compiled, &RasterProduct::rgb8(width, height))
        else {
            return;
        };
        let mut bitmap = Bitmap::new(width, height);
        if let RasterPlane::Rgb8(surface) = plane {
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let at = y * surface.stride + x * 3;
                    if let Some(rgb) = surface.pixels.get(at..at + 3) {
                        bitmap.pixels[y * width as usize + x] = 0xFF00_0000
                            | (u32::from(rgb[0]) << 16)
                            | (u32::from(rgb[1]) << 8)
                            | u32::from(rgb[2]);
                    }
                }
            }
        }
        research.words =
            lege_pdf_read::positioned_words(&research.session, research.page, width, height)
                .unwrap_or_default();
        research.raster = Some((research.page, width, height, bitmap));
    }

    /// Where the page image sits in the window: left, top.
    fn source_origin(&self, window_w: i32) -> (i32, i32) {
        let s = self.ui_scale;
        let left = window_w - self.pane_width() + (12.0 * s) as i32;
        let top = self.toolbar_height() + (HEADER_H * s) as i32 + (12.0 * s) as i32;
        (left, top)
    }

    fn header_buttons(&self, window_w: i32) -> Vec<(SourceAction, &'static str, Rect)> {
        let s = self.ui_scale;
        let mut x = window_w - self.pane_width() + (10.0 * s) as i32;
        let y = self.toolbar_height() + (5.0 * s) as i32;
        let h = (26.0 * s) as i32;
        let mut out = Vec::new();
        for (action, label, w, gap) in [
            (SourceAction::Previous, "\u{2039}", 28.0, 2.0),
            (SourceAction::Next, "\u{203a}", 28.0, 86.0),
            (SourceAction::Capture, "Capture", 70.0, 4.0),
            (SourceAction::Quote, "Quote", 56.0, 4.0),
            (SourceAction::Cite, "Cite", 44.0, 4.0),
            (SourceAction::Close, "\u{d7}", 28.0, 0.0),
        ] {
            let w = (w * s) as i32;
            out.push((action, label, Rect::new(x, y, w, h)));
            x += w + (gap * s) as i32;
        }
        out
    }

    /// Run a pane header action.
    pub fn run_source_action(&mut self, action: SourceAction) {
        let page = self.research.as_ref().map_or(0, |research| research.page);
        match action {
            SourceAction::Previous => self.source_page(page.saturating_sub(1)),
            SourceAction::Next => self.source_page(page + 1),
            SourceAction::Capture => {
                self.capture_source();
            }
            SourceAction::Quote => self.quote_source(),
            SourceAction::Cite => self.cite_source(),
            SourceAction::Close => self.close_source(),
        }
    }

    fn word_at(&self, x: f32, y: f32) -> Option<usize> {
        let research = self.research.as_ref()?;
        let (left, top) = self.source_origin(self.frame_width);
        let (px, py) = (x - left as f32, y - top as f32);
        research
            .words
            .iter()
            .position(|word| {
                px >= word.bbox[0] && px <= word.bbox[2] && py >= word.bbox[1] && py <= word.bbox[3]
            })
            .or_else(|| {
                // Nearest word on the same line, for a drag past a line's end.
                research
                    .words
                    .iter()
                    .enumerate()
                    .filter(|(_, word)| py >= word.bbox[1] && py <= word.bbox[3])
                    .min_by(|(_, a), (_, b)| {
                        let da = (px - (a.bbox[0] + a.bbox[2]) / 2.0).abs();
                        let db = (px - (b.bbox[0] + b.bbox[2]) / 2.0).abs();
                        da.total_cmp(&db)
                    })
                    .map(|(index, _)| index)
            })
    }

    /// Pointer press or release inside the pane. Returns whether it was.
    pub(crate) fn research_pointer(&mut self, pressed: bool) -> bool {
        let (x, y) = self.cursor;
        let pane_left = self.frame_width - self.pane_width();
        if self.research.is_none() || self.pane_width() == 0 || x < pane_left as f32 {
            if !pressed && let Some(research) = self.research.as_mut() {
                research.drag_from = None;
            }
            return false;
        }
        if pressed {
            let hit = self
                .header_buttons(self.frame_width)
                .into_iter()
                .find(|(_, _, rect)| {
                    x >= rect.x as f32
                        && x < (rect.x + rect.w) as f32
                        && y >= rect.y as f32
                        && y < (rect.y + rect.h) as f32
                })
                .map(|(action, _, _)| action);
            if let Some(action) = hit {
                self.run_source_action(action);
                return true;
            }
            let word = self.word_at(x, y);
            if let Some(research) = self.research.as_mut() {
                research.drag_from = word;
                research.selected = word.map(|word| (word, word));
                research.highlight.clear();
            }
        } else if let Some(research) = self.research.as_mut() {
            research.drag_from = None;
        }
        true
    }

    /// Pointer motion: extend a drag selection in the pane.
    pub(crate) fn research_drag(&mut self, x: f32, y: f32) {
        let Some(from) = self
            .research
            .as_ref()
            .and_then(|research| research.drag_from)
        else {
            return;
        };
        if let Some(to) = self.word_at(x, y) {
            self.select_source_words(from, to);
        }
    }

    /// Draw the pane: header, page image, selection and citation highlights.
    pub(crate) fn paint_research(
        &mut self,
        painter: &mut pixelkit_raster::Painter<'_>,
        face: &Face,
        width: i32,
        height: i32,
    ) {
        let pane = self.pane_width();
        if pane == 0 {
            return;
        }
        let s = self.ui_scale;
        let left = width - pane;
        let top = self.toolbar_height();
        painter.fill_rect(Rect::new(left, top, pane, height - top), 0x00D9_D2C6);
        painter.fill_rect(
            Rect::new(left, top, (1.0 * s).max(1.0) as i32, height - top),
            0x00C4_BAAB,
        );
        let (max_w, max_h) = self.pane_size();
        self.ensure_source_page(max_w, max_h);
        let size = 13.0 * s;
        let baseline = top + (23.0 * s) as i32;
        for (action, label, rect) in self.header_buttons(width) {
            let active = match action {
                SourceAction::Capture => self
                    .research
                    .as_ref()
                    .is_some_and(|research| research.selected.is_some()),
                _ => false,
            };
            painter.fill_rect(rect, if active { 0x00C9_D8EE } else { 0x00F2_EDE4 });
            let label_w: f32 = face
                .shape(label, size, &[])
                .map(|glyphs| glyphs.iter().map(|glyph| glyph.x_advance).sum())
                .unwrap_or(0.0);
            self.paint_label(
                painter,
                face,
                label,
                rect.x + ((rect.w as f32 - label_w) / 2.0) as i32,
                baseline,
                size,
            );
        }
        let counter = self
            .research
            .as_ref()
            .map(|research| format!("{} / {}", research.page + 1, research.pages))
            .unwrap_or_default();
        let next_right = self
            .header_buttons(width)
            .get(1)
            .map_or(left, |(_, _, rect)| rect.x + rect.w);
        self.paint_label(
            painter,
            face,
            &counter,
            next_right + (10.0 * s) as i32,
            baseline,
            size,
        );

        let (x0, y0) = self.source_origin(width);
        let Some(research) = self.research.as_ref() else {
            return;
        };
        let Some((_, w, h, bitmap)) = research.raster.as_ref() else {
            return;
        };
        painter.blit(
            bitmap,
            Rect::new(x0, y0, *w as i32, *h as i32),
            ScaleFilter::Nearest,
        );
        let mark = |painter: &mut pixelkit_raster::Painter<'_>, rect: [f32; 4], color: u32| {
            let r = Rect::new(
                x0 + rect[0] as i32,
                y0 + rect[1] as i32,
                (rect[2] - rect[0]).max(1.0) as i32,
                (rect[3] - rect[1]).max(1.0) as i32,
            );
            let mut tint = Bitmap::new(1, 1);
            tint.pixels[0] = color;
            painter.blit(&tint, r, ScaleFilter::Nearest);
        };
        if let Some((from, to)) = research.selected {
            for word in research.words.get(from..=to).unwrap_or(&[]) {
                mark(painter, word.bbox, 0x5A3C_78E6);
            }
        }
        for rect in &research.highlight {
            let pixels = [
                rect[0] * *w as f32,
                rect[1] * *h as f32,
                rect[2] * *w as f32,
                rect[3] * *h as f32,
            ];
            mark(painter, pixels, 0x66FF_D200);
        }
    }
}
