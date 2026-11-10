//! The six-page view (G): six pages of the queue at once, three across and
//! two down, each fitted into its cell with its boxes, for queues whose boxes
//! are almost always right and only need a look (large pictures above all).
//!
//! Pages come in aligned groups of six: the group of queue position `p`
//! starts at `p / 6 * 6`, so a page opened from the view and left returns to
//! the same six. The window is all pages: a slim header line, then a margin
//! around the window's edge and around every page, so a box lying exactly on
//! a page edge (a whole-page figure) is drawn in the gap, never under a
//! neighbour or the window border.

use std::sync::Arc;

use liminalkit_raster::{Painter, Rect, WindowBuffer};
use liminalkit_shell::Scale;
use liminalkit_text::{Align, TextCache};

use crate::annotation::commands::{Annotation, ReviewStatus};
use crate::annotation::geometry::Box2;
use crate::annotation::labels::{ABANDON, FIGURE, IGNORE_COLOR, NUM_LABELS};
use crate::canvas::draw::{
    CANVAS_BG, PageRaster, RasterKey, blit_raster, dashed_outline, outline, pixel_rect, render_page,
};
use crate::canvas::viewport::{CanvasRect, ViewTransform};
use crate::image_io::decode::PageImage;
use crate::ui::{self, ACCENT, BAD, GOOD, HUD_BG, HUD_DIM, HUD_EDGE, HUD_TEXT, WARN};

pub const COLS: usize = 3;
pub const ROWS: usize = 2;
/// Pages shown at once.
pub const PER_VIEW: usize = COLS * ROWS;

/// Window background around the cells: darker than a cell, so the margins
/// and the gaps between pages read as such.
const WINDOW_BG: u32 = 0x000e_1012;

/// Logical pixels.
const HEADER: i32 = 30;
const OUTER_MARGIN: i32 = 16;
const GAP: i32 = 14;
/// Space between a cell's edge and its page (a box on the page edge is
/// drawn into it).
const PAGE_PAD: i32 = 8;

/// First queue position of the group holding `pos`.
pub fn group_start(pos: usize) -> usize {
    pos / PER_VIEW * PER_VIEW
}

/// Where everything goes for a window of `w × h` physical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub header: Rect,
    /// [`PER_VIEW`] cells, row by row.
    pub cells: Vec<Rect>,
    /// Height of the label line at the top of each cell.
    pub label_h: i32,
    pub pad: i32,
}

impl Layout {
    /// The cell under `(x, y)`, if any (the margins and gaps are no cell).
    pub fn cell_at(&self, x: i32, y: i32) -> Option<usize> {
        self.cells.iter().position(|r| r.contains(x, y))
    }

    /// The area cell `i`'s page is fitted into.
    pub fn page_area(&self, i: usize) -> Rect {
        let c = self.cells[i];
        Rect::new(
            c.x + self.pad,
            c.y + self.label_h,
            (c.w - 2 * self.pad).max(1),
            (c.h - self.label_h - self.pad).max(1),
        )
    }
}

/// The view's layout. `label_h` is the label line's height (text height
/// plus a little), measured by the caller.
pub fn layout(w: i32, h: i32, s: Scale, label_h: i32) -> Layout {
    let header_h = s.px(HEADER);
    let m = s.px(OUTER_MARGIN);
    let g = s.px(GAP);
    let top = header_h + m;
    let (cols, rows) = (COLS as i32, ROWS as i32);
    let cw = ((w - 2 * m - (cols - 1) * g) / cols).max(1);
    let ch = ((h - top - m - (rows - 1) * g) / rows).max(1);
    let cells = (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (r, c)))
        .map(|(r, c)| Rect::new(m + c * (cw + g), top + r * (ch + g), cw, ch))
        .collect();
    Layout {
        header: Rect::new(0, 0, w, header_h),
        cells,
        label_h,
        pad: s.px(PAGE_PAD),
    }
}

/// One page of the group, as the view shows it.
#[derive(Clone, Debug)]
pub struct Cell {
    pub key: String,
    pub book: String,
    /// The image file's name, without folders.
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub status: ReviewStatus,
    /// The editable layer: the in-memory document when there is one (it may
    /// hold edits not yet written), else the database's.
    pub annotations: Vec<Annotation>,
    /// Opened in the editor from this view and left without accepting:
    /// Right in the view does not accept it.
    pub held: bool,
}

impl Cell {
    pub fn todo(&self) -> bool {
        matches!(self.status, ReviewStatus::Unseen | ReviewStatus::Opened)
    }

    pub fn abandon(&self) -> usize {
        self.annotations
            .iter()
            .filter(|a| a.class_id == ABANDON)
            .count()
    }

    /// The largest figure box as a share of the page.
    pub fn figure_share(&self) -> f64 {
        let page = self.width as f64 * self.height as f64;
        if page <= 0.0 {
            return 0.0;
        }
        self.annotations
            .iter()
            .filter(|a| a.class_id == FIGURE)
            .map(|a| a.bbox.area())
            .fold(0.0, f64::max)
            / page
    }

    /// What the label says about the page's status, and in which colour.
    fn status_label(&self) -> (&'static str, u32) {
        match self.status {
            _ if self.held && self.todo() => ("opened, not accepted", ACCENT),
            ReviewStatus::Unseen | ReviewStatus::Opened => ("to do", HUD_DIM),
            ReviewStatus::Reviewed => ("reviewed", GOOD),
            ReviewStatus::Excluded => ("excluded", BAD),
            ReviewStatus::NeedsAttention => ("needs attention", WARN),
        }
    }
}

/// A cell's image.
#[derive(Clone)]
pub enum CellImage {
    Ready(Arc<PageImage>),
    Loading,
    Failed(String),
}

/// The view's state while it shows.
pub struct GridView {
    /// Queue position of the first page shown.
    pub start: usize,
    pub cells: Vec<Cell>,
    /// From the last frame drawn (for the pointer).
    pub layout: Option<Layout>,
    /// The cell under the pointer.
    pub hover: Option<usize>,
    /// The cell a left press went down in (a click opens it on release).
    pub pressed: Option<usize>,
    /// The group whose Right was refused because pages to do there have
    /// `abandon` boxes: a second Right accepts the others.
    pub abandon_warned: Option<usize>,
    rasters: Vec<PageRaster>,
}

/// What the header line says.
pub struct Header<'a> {
    pub left: &'a str,
    pub right: &'a str,
    /// A transient message replaces `right` (true: an error).
    pub message: Option<(&'a str, bool)>,
}

impl GridView {
    pub fn new(start: usize) -> GridView {
        GridView {
            start,
            cells: Vec::new(),
            layout: None,
            hover: None,
            pressed: None,
            abandon_warned: None,
            rasters: (0..PER_VIEW).map(|_| PageRaster::default()).collect(),
        }
    }

    /// Draw the whole window. `images` has one entry per cell.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        buffer: &mut WindowBuffer,
        text: &mut TextCache,
        s: Scale,
        images: &[CellImage],
        colors: &[u32; NUM_LABELS],
        header: &Header<'_>,
        threads: usize,
    ) {
        let (bw, bh) = (buffer.width as i32, buffer.height as i32);
        let label_st = ui::style(s, 11.5);
        let label_h = text.line_height(label_st) + s.px(8);
        let l = layout(bw, bh, s, label_h);

        // Pages first: the rasters go straight into the buffer.
        {
            let mut p = Painter::new(buffer);
            p.fill_rect(Rect::new(0, 0, bw, bh), WINDOW_BG);
            for c in l.cells.iter().take(self.cells.len()) {
                p.fill_rect(*c, CANVAS_BG);
            }
        }
        let mut views: Vec<Option<ViewTransform>> = vec![None; self.cells.len()];
        for (i, cell) in self.cells.iter().enumerate() {
            let Some(CellImage::Ready(img)) = images.get(i) else {
                continue;
            };
            let area = l.page_area(i);
            let view = ViewTransform::fit(
                CanvasRect {
                    x: area.x as f64,
                    y: area.y as f64,
                    w: area.w as f64,
                    h: area.h as f64,
                },
                cell.width.max(1) as f64,
                cell.height.max(1) as f64,
                0.0,
            );
            views[i] = Some(view);
            let key = RasterKey {
                image_id: Arc::as_ptr(img) as usize,
                origin_x: view.origin.x,
                origin_y: view.origin.y,
                scale: view.scale,
                w: area.w as u32,
                h: area.h as u32,
                x: area.x,
                y: area.y,
            };
            let raster = &mut self.rasters[i];
            if raster.key != Some(key) {
                render_page(
                    raster,
                    img,
                    &view,
                    area.x,
                    area.y,
                    area.w as u32,
                    area.h as u32,
                    threads,
                );
                raster.key = Some(key);
            }
            blit_raster(
                &mut buffer.pixels,
                bw as usize,
                bh as usize,
                raster,
                area.x,
                area.y,
            );
        }

        let mut p = Painter::new(buffer);
        // Header.
        p.fill_rect(l.header, HUD_BG);
        p.fill_rect(Rect::new(0, l.header.bottom() - 1, l.header.w, 1), HUD_EDGE);
        let hs = ui::style(s, 12.5);
        let hy = (l.header.h - text.line_height(hs)) / 2;
        let left_w = text.measure(header.left, hs).min(bw / 2);
        text.draw_fitted(
            &mut p,
            header.left,
            Rect::new(s.px(12), 0, left_w, l.header.h),
            hy,
            hs,
            HUD_TEXT,
            Align::Left,
        );
        let (right, rc) = match header.message {
            Some((m, true)) => (m, BAD),
            Some((m, false)) => (m, HUD_TEXT),
            None => (header.right, HUD_DIM),
        };
        let rx = s.px(12) + left_w + s.px(24);
        text.draw_fitted(
            &mut p,
            right,
            Rect::new(rx, 0, (bw - rx - s.px(12)).max(0), l.header.h),
            hy,
            hs,
            rc,
            Align::Right,
        );

        for (i, cell) in self.cells.iter().enumerate() {
            let c = l.cells[i];
            let area = l.page_area(i);
            p.push_clip(c);
            // Label: key number, book, file, status, largest figure.
            let ty = c.y + (label_h - text.line_height(label_st)) / 2;
            let num = format!("{}", i + 1);
            let nw = text.measure(&num, label_st) + s.px(10);
            let nr = Rect::new(c.x + s.px(6), c.y + s.px(3), nw, label_h - s.px(6));
            p.fill_rect(
                nr,
                if self.hover == Some(i) {
                    ACCENT
                } else {
                    HUD_EDGE
                },
            );
            text.draw(&mut p, &num, nr.x + s.px(5), ty, label_st, HUD_TEXT);
            let (status, sc) = cell.status_label();
            let share = cell.figure_share();
            let fig = if share > 0.0 {
                format!("  ·  figure {:.0}%", share * 100.0)
            } else {
                String::new()
            };
            let right = format!("{status}{fig}");
            let rw = text.measure(&right, label_st);
            let rx = c.right() - s.px(8) - rw;
            text.draw(&mut p, status, rx, ty, label_st, sc);
            if !fig.is_empty() {
                let sw = text.measure(status, label_st);
                text.draw(&mut p, &fig, rx + sw, ty, label_st, HUD_DIM);
            }
            let name_x = nr.right() + s.px(8);
            text.draw_fitted(
                &mut p,
                &format!("{} · {}", cell.book, cell.file),
                Rect::new(name_x, c.y, (rx - s.px(10) - name_x).max(0), label_h),
                ty,
                label_st,
                HUD_TEXT,
                Align::Left,
            );

            match (images.get(i), views[i]) {
                (Some(CellImage::Ready(_)), Some(view)) => {
                    draw_boxes(&mut p, &cell.annotations, &view, colors, s);
                    if cell.status == ReviewStatus::Excluded {
                        // Dim the page: it will not be exported.
                        let page = pixel_rect(view.box_to_screen(Box2::new(
                            0.0,
                            0.0,
                            cell.width as f64,
                            cell.height as f64,
                        )));
                        p.blend_rect(page, 0x000000, 110);
                    }
                }
                (Some(CellImage::Failed(e)), _) => {
                    let st = ui::style(s, 12.0);
                    let y = area.y + (area.h - text.line_height(st)) / 2;
                    text.draw_fitted(
                        &mut p,
                        &format!("Image failed to load: {e}"),
                        area.inset(s.px(8)),
                        y,
                        st,
                        BAD,
                        Align::Centre,
                    );
                }
                _ => {
                    let st = ui::style(s, 12.0);
                    let y = area.y + (area.h - text.line_height(st)) / 2;
                    text.draw_fitted(
                        &mut p,
                        "Loading page…",
                        area.inset(s.px(8)),
                        y,
                        st,
                        HUD_DIM,
                        Align::Centre,
                    );
                }
            }
            p.pop_clip();
            // The cell edge: the status colour once the page is decided,
            // the accent under the pointer.
            let edge = match (self.hover == Some(i), cell.status) {
                (true, _) => Some(ACCENT),
                (_, ReviewStatus::Reviewed) => Some(GOOD),
                (_, ReviewStatus::Excluded) => Some(BAD),
                (_, ReviewStatus::NeedsAttention) => Some(WARN),
                _ => None,
            };
            match edge {
                Some(color) => outline(&mut p, c.inset(-s.px(2)), color, s.px(2).max(1)),
                None => outline(&mut p, c, HUD_EDGE, 1),
            }
        }
        self.layout = Some(l);
    }
}

/// A page's boxes as the editor draws them, larger first so nested ones sit
/// on top. Figures, the point of the view, get a light fill and a thick
/// outline set half outside the box, so one on the page edge shows in the
/// margin around the page.
fn draw_boxes(
    p: &mut Painter<'_>,
    anns: &[Annotation],
    view: &ViewTransform,
    colors: &[u32; NUM_LABELS],
    s: Scale,
) {
    let mut order: Vec<&Annotation> = anns.iter().collect();
    order.sort_by(|a, b| b.bbox.area().total_cmp(&a.bbox.area()));
    let thick = s.px(3).max(2);
    for a in order {
        let r = pixel_rect(view.box_to_screen(a.bbox));
        let color = if a.is_ignore {
            IGNORE_COLOR
        } else {
            colors[(a.class_id as usize).min(NUM_LABELS - 1)]
        };
        if a.is_ignore {
            dashed_outline(p, r, color, 5, 3);
        } else if a.class_id == FIGURE {
            p.blend_rect(r, color, 40);
            outline(p, r.inset(-(thick / 2 + 1)), color, thick);
        } else if a.class_id == ABANDON {
            outline(p, r, color, 2);
        } else {
            outline(p, r, color, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_are_aligned_by_six() {
        assert_eq!(group_start(0), 0);
        assert_eq!(group_start(5), 0);
        assert_eq!(group_start(6), 6);
        assert_eq!(group_start(13), 12);
    }

    #[test]
    fn cells_keep_a_margin_from_the_window_and_each_other() {
        let l = layout(1600, 1000, Scale::ONE, 22);
        assert_eq!(l.cells.len(), PER_VIEW);
        for c in &l.cells {
            assert!(c.x >= OUTER_MARGIN && c.y >= HEADER + OUTER_MARGIN, "{c:?}");
            assert!(c.right() <= 1600 - OUTER_MARGIN && c.bottom() <= 1000 - OUTER_MARGIN);
        }
        // Three across, two down, a gap between neighbours.
        assert_eq!(l.cells[1].x - l.cells[0].right(), GAP);
        assert_eq!(l.cells[3].y - l.cells[0].bottom(), GAP);
        assert_eq!(l.cells[3].x, l.cells[0].x);
        // A page area stays inside its cell with room on every side.
        for i in 0..PER_VIEW {
            let (a, c) = (l.page_area(i), l.cells[i]);
            assert!(a.x - c.x >= PAGE_PAD && c.right() - a.right() >= PAGE_PAD);
            assert!(c.bottom() - a.bottom() >= PAGE_PAD && a.y > c.y);
        }
    }
}
