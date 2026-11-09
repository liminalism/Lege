//! Property tests (deterministic PRNG, many cases) for the coordinate
//! systems: image ↔ screen at unusual zooms and DPI factors, cursor-anchored
//! zoom, and the original ↔ training-resolution stretch.

use liminal_annotator::annotation::geometry::{
    Box2, DragLimits, Handle, Point, apply_drag, coco_xywh, hit_box, stretch_box,
};
use liminal_annotator::canvas::viewport::{CanvasRect, MAX_SCALE, MIN_SCALE, ViewTransform};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.next()
    }
    fn log_range(&mut self, a: f64, b: f64) -> f64 {
        (a.ln() + (b.ln() - a.ln()) * self.next()).exp()
    }
}

#[test]
fn image_screen_round_trip_at_any_zoom_and_dpi() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..200_000 {
        // DPI factors Windows offers, times any zoom.
        let dpi = [1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0][(r.next() * 8.0) as usize % 8];
        let t = ViewTransform {
            origin: Point::new(r.range(-50_000.0, 50_000.0), r.range(-50_000.0, 50_000.0)),
            scale: (r.log_range(MIN_SCALE, MAX_SCALE) * dpi).min(MAX_SCALE),
        };
        let p = Point::new(r.range(0.0, 12_000.0), r.range(0.0, 12_000.0));
        let q = t.screen_to_image(t.image_to_screen(p));
        let tol = 1e-9
            * (1.0 + p.x.abs().max(p.y.abs()) + t.origin.x.abs().max(t.origin.y.abs()) / t.scale);
        assert!(
            (p.x - q.x).abs() <= tol && (p.y - q.y).abs() <= tol,
            "{t:?} {p:?} -> {q:?}"
        );
    }
}

#[test]
fn zoom_sequences_never_move_the_anchor() {
    let mut r = Rng(42);
    for _ in 0..2000 {
        let mut t = ViewTransform {
            origin: Point::new(r.range(-500.0, 500.0), r.range(-500.0, 500.0)),
            scale: r.log_range(0.05, 8.0),
        };
        let cursor = Point::new(r.range(0.0, 3840.0), r.range(0.0, 2160.0));
        let anchor = t.screen_to_image(cursor);
        for _ in 0..40 {
            let f = r.log_range(0.5, 2.0);
            t.zoom_at(cursor, f);
            let now = t.screen_to_image(cursor);
            let tol = 1e-7 * (1.0 + anchor.x.abs() + anchor.y.abs());
            assert!(
                (now.x - anchor.x).abs() < tol && (now.y - anchor.y).abs() < tol,
                "{t:?}"
            );
            assert!(t.scale >= MIN_SCALE && t.scale <= MAX_SCALE);
            // Once clamped, the anchor still holds (zoom_at recomputes origin).
        }
    }
}

#[test]
fn fit_and_constrain_keep_the_page_visible() {
    let mut r = Rng(7);
    for _ in 0..5000 {
        let canvas = CanvasRect {
            x: 0.0,
            y: r.range(0.0, 80.0),
            w: r.range(200.0, 3840.0),
            h: r.range(200.0, 2160.0),
        };
        let (w, h) = (r.range(100.0, 12_000.0), r.range(100.0, 12_000.0));
        let t = ViewTransform::fit(canvas, w, h, 8.0);
        let tl = t.image_to_screen(Point::new(0.0, 0.0));
        let br = t.image_to_screen(Point::new(w, h));
        assert!(tl.x >= canvas.x - 1e-6 && tl.y >= canvas.y - 1e-6, "{t:?}");
        assert!(
            br.x <= canvas.x + canvas.w + 1e-6 && br.y <= canvas.y + canvas.h + 1e-6,
            "{t:?}"
        );
        let mut t2 = t;
        t2.origin = Point::new(r.range(-1e6, 1e6), r.range(-1e6, 1e6));
        t2.scale = r.log_range(MIN_SCALE, MAX_SCALE);
        t2.constrain(canvas, w, h, 64.0);
        let tl = t2.image_to_screen(Point::new(0.0, 0.0));
        let br = t2.image_to_screen(Point::new(w, h));
        let vis_w = br.x.min(canvas.x + canvas.w) - tl.x.max(canvas.x);
        let vis_h = br.y.min(canvas.y + canvas.h) - tl.y.max(canvas.y);
        let need_w = 64f64.min(w * t2.scale).min(canvas.w);
        let need_h = 64f64.min(h * t2.scale).min(canvas.h);
        assert!(
            vis_w >= need_w - 1e-6 && vis_h >= need_h - 1e-6,
            "page lost: {t2:?} {vis_w} {vis_h}"
        );
    }
}

#[test]
fn stretch_round_trips_through_training_resolution() {
    let mut r = Rng(99);
    for _ in 0..50_000 {
        let (sw, sh) = (
            r.range(100.0, 9000.0) as u32,
            r.range(100.0, 12_000.0) as u32,
        );
        let t = [512u32, 640, 768, 1024][(r.next() * 4.0) as usize % 4];
        let x0 = r.range(0.0, sw as f64 - 2.0);
        let y0 = r.range(0.0, sh as f64 - 2.0);
        let b = Box2::new(
            x0,
            y0,
            r.range(x0 + 1.0, sw as f64),
            r.range(y0 + 1.0, sh as f64),
        );
        let s = stretch_box(b, sw, sh, t, t);
        assert!(s.x0 >= 0.0 && s.y0 >= 0.0 && s.x1 <= t as f64 + 1e-9 && s.y1 <= t as f64 + 1e-9);
        let back = stretch_box(s, t, t, sw, sh);
        assert!(
            back.approx_eq(&b, 1e-6 * (sw.max(sh) as f64)),
            "{b:?} {back:?}"
        );
        let xywh = coco_xywh(s);
        assert!(xywh[2] > 0.0 && xywh[3] > 0.0);
    }
}

#[test]
fn drags_never_flip_or_escape_the_page() {
    let mut r = Rng(1234);
    let handles = [
        Handle::TopLeft,
        Handle::TopRight,
        Handle::BottomLeft,
        Handle::BottomRight,
        Handle::Move,
    ];
    for _ in 0..100_000 {
        let (w, h) = (r.range(50.0, 4000.0), r.range(50.0, 4000.0));
        let x0 = r.range(0.0, w - 5.0);
        let y0 = r.range(0.0, h - 5.0);
        let b = Box2::new(x0, y0, r.range(x0 + 3.0, w), r.range(y0 + 3.0, h));
        let lim = DragLimits {
            min_size: 2.0,
            bounds_w: w,
            bounds_h: h,
        };
        let hdl = handles[(r.next() * 5.0) as usize % 5];
        let d = Point::new(r.range(-2.0 * w, 2.0 * w), r.range(-2.0 * h, 2.0 * h));
        let out = apply_drag(b, hdl, d, lim);
        assert!(
            out.x1 > out.x0 && out.y1 > out.y0,
            "flipped {hdl:?} {b:?} {d:?} -> {out:?}"
        );
        assert!(
            out.x0 >= 0.0 && out.y0 >= 0.0 && out.x1 <= w + 1e-9 && out.y1 <= h + 1e-9,
            "{out:?}"
        );
        if hdl == Handle::Move {
            assert!(
                (out.width() - b.width()).abs() < 1e-9 && (out.height() - b.height()).abs() < 1e-9
            );
        } else {
            assert!(out.width() >= 2.0 - 1e-9 || out.width() >= b.width().min(2.0) - 1e-9);
        }
    }
}

#[test]
fn every_point_of_the_perimeter_band_is_a_corner_handle() {
    let mut r = Rng(5);
    for _ in 0..20_000 {
        let w = r.range(1.0, 600.0);
        let h = r.range(1.0, 600.0);
        let b = Box2::new(100.0, 100.0, 100.0 + w, 100.0 + h);
        let slop = 8.0;
        // A point just outside an edge, within the band.
        let t = r.next();
        let off = r.range(0.01, slop);
        let p = match (r.next() * 4.0) as usize % 4 {
            0 => Point::new(b.x0 + t * w, b.y0 - off),
            1 => Point::new(b.x0 + t * w, b.y1 + off),
            2 => Point::new(b.x0 - off, b.y0 + t * h),
            _ => Point::new(b.x1 + off, b.y0 + t * h),
        };
        let hit = hit_box(b, p, slop);
        assert!(
            matches!(hit, Some(h) if h.is_corner()),
            "{b:?} {p:?} {hit:?}"
        );
        // The interior centre is always movable, however narrow the box.
        let c = Point::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
        assert_eq!(hit_box(b, c, slop), Some(Handle::Move), "{b:?}");
    }
}
