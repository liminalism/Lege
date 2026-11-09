//! Image ↔ screen mapping with cursor-anchored zoom, fit, 1:1 and a
//! constraint that keeps the page from disappearing.
//!
//! Screen coordinates are physical device pixels of the window buffer. The
//! transform is a uniform scale plus an origin: `screen = origin + image *
//! scale`. "Actual pixels" is therefore `scale = 1`: one physical screen pixel
//! per original image pixel, independent of the Windows DPI setting.

use crate::annotation::geometry::{Box2, Point};

pub const MIN_SCALE: f64 = 0.005;
pub const MAX_SCALE: f64 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewTransform {
    /// Screen position of image coordinate (0, 0).
    pub origin: Point,
    /// Physical screen pixels per original image pixel.
    pub scale: f64,
}

impl Default for ViewTransform {
    fn default() -> Self {
        ViewTransform {
            origin: Point::new(0.0, 0.0),
            scale: 1.0,
        }
    }
}

/// The screen rectangle the page is shown in, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl CanvasRect {
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }
}

/// How the view was last set; a window resize re-applies `Fit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomMode {
    Fit,
    Actual,
    Free,
}

impl ViewTransform {
    pub fn image_to_screen(&self, p: Point) -> Point {
        Point {
            x: self.origin.x + p.x * self.scale,
            y: self.origin.y + p.y * self.scale,
        }
    }

    pub fn screen_to_image(&self, p: Point) -> Point {
        Point {
            x: (p.x - self.origin.x) / self.scale,
            y: (p.y - self.origin.y) / self.scale,
        }
    }

    pub fn box_to_screen(&self, b: Box2) -> Box2 {
        let a = self.image_to_screen(Point::new(b.x0, b.y0));
        let c = self.image_to_screen(Point::new(b.x1, b.y1));
        Box2::new(a.x, a.y, c.x, c.y)
    }

    /// Zoom without moving the image point beneath the cursor.
    pub fn zoom_at(&mut self, cursor: Point, factor: f64) {
        let anchor = self.screen_to_image(cursor);
        self.scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        self.origin = Point {
            x: cursor.x - anchor.x * self.scale,
            y: cursor.y - anchor.y * self.scale,
        };
    }

    /// The whole page, centred, as large as fits (with a small margin).
    pub fn fit(canvas: CanvasRect, image_w: f64, image_h: f64, margin: f64) -> ViewTransform {
        let avail_w = (canvas.w - 2.0 * margin).max(1.0);
        let avail_h = (canvas.h - 2.0 * margin).max(1.0);
        let scale = (avail_w / image_w)
            .min(avail_h / image_h)
            .clamp(MIN_SCALE, MAX_SCALE);
        ViewTransform {
            origin: Point::new(
                canvas.x + (canvas.w - image_w * scale) * 0.5,
                canvas.y + (canvas.h - image_h * scale) * 0.5,
            ),
            scale,
        }
    }

    /// One physical pixel per image pixel, keeping the image point under
    /// `anchor` (usually the cursor, or the canvas centre) where it is.
    pub fn actual_at(&mut self, anchor: Point) {
        self.zoom_at(anchor, 1.0 / self.scale);
        self.scale = 1.0;
    }

    /// Keep at least `keep` physical pixels of the page visible on each axis
    /// (or the whole page, if it is smaller), so panning and zooming can
    /// never lose it entirely.
    pub fn constrain(&mut self, canvas: CanvasRect, image_w: f64, image_h: f64, keep: f64) {
        let w = image_w * self.scale;
        let h = image_h * self.scale;
        let kx = keep.min(w);
        let ky = keep.min(h);
        // Page's right edge must stay right of canvas.x + kx; its left edge
        // must stay left of canvas right - kx.
        let min_x = canvas.x + kx - w;
        let max_x = canvas.x + canvas.w - kx;
        let min_y = canvas.y + ky - h;
        let max_y = canvas.y + canvas.h - ky;
        self.origin.x = self.origin.x.clamp(min_x.min(max_x), max_x.max(min_x));
        self.origin.y = self.origin.y.clamp(min_y.min(max_y), max_y.max(min_y));
    }
}

/// Snap a view origin to a multiple of 2^-20 px (under a millionth of a pixel,
/// far below anything visible).
///
/// A pan moves the origin by whole pixels. From an arbitrary float origin `o`,
/// `o + 3` and `o + 5` are each rounded, so their difference is not exactly 2
/// and the raster cache cannot tell the pan was a pure shift. From a grid
/// origin below 2^32 px the sums are exact, the deltas are exact integers, and
/// a shifted raster is bit-identical to a full render of the new view.
pub fn pan_grid(v: f64) -> f64 {
    const GRID: f64 = 1_048_576.0; // 2^20
    (v * GRID).round() / GRID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_cursor_point() {
        let mut t = ViewTransform {
            origin: Point::new(37.0, -12.5),
            scale: 0.73,
        };
        let cursor = Point::new(400.0, 300.0);
        let before = t.screen_to_image(cursor);
        t.zoom_at(cursor, 1.37);
        let after = t.screen_to_image(cursor);
        assert!((before.x - after.x).abs() < 1e-9 && (before.y - after.y).abs() < 1e-9);
    }

    #[test]
    fn fit_centres() {
        let c = CanvasRect {
            x: 0.0,
            y: 40.0,
            w: 1000.0,
            h: 800.0,
        };
        let t = ViewTransform::fit(c, 850.0, 1280.0, 0.0);
        assert!((t.scale - 800.0 / 1280.0).abs() < 1e-12);
        let tl = t.image_to_screen(Point::new(0.0, 0.0));
        let br = t.image_to_screen(Point::new(850.0, 1280.0));
        assert!((tl.y - 40.0).abs() < 1e-9 && (br.y - 840.0).abs() < 1e-9);
        assert!(((tl.x + br.x) * 0.5 - 500.0).abs() < 1e-9);
    }
}
