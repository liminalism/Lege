//! Rectangle geometry: points, boxes, edge-to-corner hit-testing, drag
//! application and the export-time coordinate transforms.
//!
//! Boxes are half-open, `[x0, x1) × [y0, y1)`, in floating-point *original
//! page-image* pixels. That system is authoritative: screen coordinates are
//! derived from it by the viewport, and training coordinates by the exporter.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Point {
        Point { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Box2 {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Box2 {
    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Box2 {
        Box2 { x0, y0, x1, y1 }
    }

    /// The box spanned by two arbitrary corners.
    pub fn from_corners(a: Point, b: Point) -> Box2 {
        Box2 {
            x0: a.x.min(b.x),
            y0: a.y.min(b.y),
            x1: a.x.max(b.x),
            y1: a.y.max(b.y),
        }
    }

    pub fn from_xywh(x: f64, y: f64, w: f64, h: f64) -> Box2 {
        Box2::new(x, y, x + w, y + h)
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn area(&self) -> f64 {
        self.width().max(0.0) * self.height().max(0.0)
    }

    pub fn is_valid(&self) -> bool {
        self.x1 > self.x0 && self.y1 > self.y0 && self.x0.is_finite() && self.y1.is_finite()
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }

    pub fn intersection(&self, other: &Box2) -> Box2 {
        Box2 {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    pub fn iou(&self, other: &Box2) -> f64 {
        let inter = self.intersection(other).area();
        let union = self.area() + other.area() - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }

    /// Clamp into `[0, w] × [0, h]`. May return an invalid (empty) box if the
    /// box lies entirely outside; callers validate afterwards.
    pub fn clamp_to(&self, w: f64, h: f64) -> Box2 {
        Box2 {
            x0: self.x0.clamp(0.0, w),
            y0: self.y0.clamp(0.0, h),
            x1: self.x1.clamp(0.0, w),
            y1: self.y1.clamp(0.0, h),
        }
    }

    pub fn translate(&self, dx: f64, dy: f64) -> Box2 {
        Box2::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }

    /// Exact-enough geometric equality for "was this edited?".
    pub fn approx_eq(&self, other: &Box2, eps: f64) -> bool {
        (self.x0 - other.x0).abs() <= eps
            && (self.y0 - other.y0).abs() <= eps
            && (self.x1 - other.x1).abs() <= eps
            && (self.y1 - other.y1).abs() <= eps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handle {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Move,
}

impl Handle {
    pub fn is_corner(self) -> bool {
        self != Handle::Move
    }
}

/// Which handle a pointer at `p` grabs on box `b`, with `slop` the
/// half-width of the invisible hit band around the outline (all in the same
/// coordinate system — the application passes *screen* coordinates so the
/// band is the same physical size at every zoom).
///
/// The whole perimeter is a target: the left half of the top edge moves the
/// top-left corner, the right half the top-right corner, and so on. Where two
/// edge bands overlap the closest edge wins. On a box too small for the bands
/// to leave an interior, the band shrinks on the inside to a third of the
/// box, so a narrow paragraph can still be moved.
pub fn hit_box(b: Box2, p: Point, slop: f64) -> Option<Handle> {
    hit_box_scored(b, p, slop).map(|(_, h)| h)
}

/// [`hit_box`] plus the distance to the grabbed edge (0 for `Move`), so
/// overlapping boxes can be ranked.
pub fn hit_box_scored(b: Box2, p: Point, slop: f64) -> Option<(f64, Handle)> {
    let inside = b.contains(p);
    // Inward slop: never more than a third of the box, so the middle third
    // of any box remains a Move target.
    let in_slop_x = slop.min(b.width() / 3.0);
    let in_slop_y = slop.min(b.height() / 3.0);
    let in_x = p.x >= b.x0 - slop && p.x <= b.x1 + slop;
    let in_y = p.y >= b.y0 - slop && p.y <= b.y1 + slop;
    let mid_x = (b.x0 + b.x1) * 0.5;
    let mid_y = (b.y0 + b.y1) * 0.5;

    let mut best: Option<(f64, Handle)> = None;
    let mut consider = |dist: f64, handle: Handle| {
        if best.is_none_or(|(d, _)| dist < d) {
            best = Some((dist, handle));
        }
    };

    if in_x {
        let top = (p.y - b.y0).abs();
        let bottom = (p.y - b.y1).abs();
        let top_ok = if p.y >= b.y0 {
            top <= in_slop_y
        } else {
            top <= slop
        };
        let bottom_ok = if p.y <= b.y1 {
            bottom <= in_slop_y
        } else {
            bottom <= slop
        };
        let left_half = p.x < mid_x;
        if top_ok {
            consider(
                top,
                if left_half {
                    Handle::TopLeft
                } else {
                    Handle::TopRight
                },
            );
        }
        if bottom_ok {
            consider(
                bottom,
                if left_half {
                    Handle::BottomLeft
                } else {
                    Handle::BottomRight
                },
            );
        }
    }
    if in_y {
        let left = (p.x - b.x0).abs();
        let right = (p.x - b.x1).abs();
        let left_ok = if p.x >= b.x0 {
            left <= in_slop_x
        } else {
            left <= slop
        };
        let right_ok = if p.x <= b.x1 {
            right <= in_slop_x
        } else {
            right <= slop
        };
        let top_half = p.y < mid_y;
        if left_ok {
            consider(
                left,
                if top_half {
                    Handle::TopLeft
                } else {
                    Handle::BottomLeft
                },
            );
        }
        if right_ok {
            consider(
                right,
                if top_half {
                    Handle::TopRight
                } else {
                    Handle::BottomRight
                },
            );
        }
    }
    if best.is_some() {
        return best;
    }
    if inside {
        Some((0.0, Handle::Move))
    } else {
        None
    }
}

/// Constraints a drag is applied under, in image pixels.
#[derive(Clone, Copy, Debug)]
pub struct DragLimits {
    /// Smallest width/height a resize may produce. A drag that crosses the
    /// opposite edge stops here instead of flipping the box.
    pub min_size: f64,
    /// Image bounds; boxes never leave `[0, w] × [0, h]`.
    pub bounds_w: f64,
    pub bounds_h: f64,
}

/// The preview geometry for a drag of `handle`, computed from the *original*
/// box and the total pointer displacement since the press. Recomputing from
/// the original every move (never from the previous preview) means no drift
/// accumulates however many events a gesture produces.
pub fn apply_drag(original: Box2, handle: Handle, delta: Point, limits: DragLimits) -> Box2 {
    let DragLimits {
        min_size,
        bounds_w,
        bounds_h,
    } = limits;
    let min_w = min_size.min(bounds_w);
    let min_h = min_size.min(bounds_h);
    match handle {
        Handle::Move => {
            let w = original.width().min(bounds_w);
            let h = original.height().min(bounds_h);
            let x0 = (original.x0 + delta.x).clamp(0.0, bounds_w - w);
            let y0 = (original.y0 + delta.y).clamp(0.0, bounds_h - h);
            Box2::new(x0, y0, x0 + w, y0 + h)
        }
        corner => {
            let mut b = original;
            let (moves_left, moves_top) = match corner {
                Handle::TopLeft => (true, true),
                Handle::TopRight => (false, true),
                Handle::BottomLeft => (true, false),
                Handle::BottomRight => (false, false),
                Handle::Move => unreachable!(),
            };
            if moves_left {
                b.x0 = (original.x0 + delta.x).clamp(0.0, (b.x1 - min_w).max(0.0));
            } else {
                b.x1 = (original.x1 + delta.x).clamp((b.x0 + min_w).min(bounds_w), bounds_w);
            }
            if moves_top {
                b.y0 = (original.y0 + delta.y).clamp(0.0, (b.y1 - min_h).max(0.0));
            } else {
                b.y1 = (original.y1 + delta.y).clamp((b.y0 + min_h).min(bounds_h), bounds_h);
            }
            b
        }
    }
}

/// Alt-resize: only the edge nearest the press moves (the other axis of the
/// grabbed corner stays put). `axis_x` selects whether the vertical (x) edge
/// or the horizontal (y) edge moves.
pub fn apply_edge_drag(
    original: Box2,
    handle: Handle,
    delta: Point,
    axis_x: bool,
    limits: DragLimits,
) -> Box2 {
    let d = if axis_x {
        Point::new(delta.x, 0.0)
    } else {
        Point::new(0.0, delta.y)
    };
    apply_drag(original, handle, d, limits)
}

/// Stretch a box from a `source_w × source_h` image to `target_w × target_h`
/// (Lege's contract: the page is stretched to a square, not letterboxed).
pub fn stretch_box(
    original: Box2,
    source_w: u32,
    source_h: u32,
    target_w: u32,
    target_h: u32,
) -> Box2 {
    assert!(source_w > 0 && source_h > 0);
    assert!(target_w > 0 && target_h > 0);
    let sx = target_w as f64 / source_w as f64;
    let sy = target_h as f64 / source_h as f64;
    Box2 {
        x0: original.x0 * sx,
        y0: original.y0 * sy,
        x1: original.x1 * sx,
        y1: original.y1 * sy,
    }
}

pub fn coco_xywh(b: Box2) -> [f64; 4] {
    [b.x0, b.y0, b.x1 - b.x0, b.y1 - b.y0]
}

/// The box as a closed rectangle polygon, COCO `segmentation` style.
pub fn coco_polygon(b: Box2) -> Vec<f64> {
    vec![b.x0, b.y0, b.x1, b.y0, b.x1, b.y1, b.x0, b.y1]
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIM: DragLimits = DragLimits {
        min_size: 2.0,
        bounds_w: 1000.0,
        bounds_h: 1000.0,
    };

    #[test]
    fn perimeter_halves_select_corners() {
        let b = Box2::new(100.0, 100.0, 300.0, 200.0);
        assert_eq!(
            hit_box(b, Point::new(150.0, 101.0), 8.0),
            Some(Handle::TopLeft)
        );
        assert_eq!(
            hit_box(b, Point::new(250.0, 99.0), 8.0),
            Some(Handle::TopRight)
        );
        assert_eq!(
            hit_box(b, Point::new(150.0, 205.0), 8.0),
            Some(Handle::BottomLeft)
        );
        assert_eq!(
            hit_box(b, Point::new(298.0, 190.0), 8.0),
            Some(Handle::BottomRight)
        );
        assert_eq!(
            hit_box(b, Point::new(95.0, 120.0), 8.0),
            Some(Handle::TopLeft)
        );
        assert_eq!(
            hit_box(b, Point::new(95.0, 180.0), 8.0),
            Some(Handle::BottomLeft)
        );
        assert_eq!(
            hit_box(b, Point::new(200.0, 150.0), 8.0),
            Some(Handle::Move)
        );
        assert_eq!(hit_box(b, Point::new(50.0, 150.0), 8.0), None);
    }

    #[test]
    fn narrow_box_keeps_a_move_target() {
        // 6 px wide on screen: bands of 8 px would swallow the interior.
        let b = Box2::new(100.0, 100.0, 106.0, 400.0);
        assert_eq!(
            hit_box(b, Point::new(103.0, 250.0), 8.0),
            Some(Handle::Move)
        );
        assert_eq!(
            hit_box(b, Point::new(100.5, 120.0), 8.0),
            Some(Handle::TopLeft)
        );
        assert_eq!(
            hit_box(b, Point::new(110.0, 390.0), 8.0),
            Some(Handle::BottomRight)
        );
    }

    #[test]
    fn resize_clamps_instead_of_flipping() {
        let b = Box2::new(100.0, 100.0, 200.0, 200.0);
        let r = apply_drag(b, Handle::TopLeft, Point::new(500.0, 500.0), LIM);
        assert_eq!(r, Box2::new(198.0, 198.0, 200.0, 200.0));
        let r = apply_drag(b, Handle::BottomRight, Point::new(-500.0, -500.0), LIM);
        assert_eq!(r, Box2::new(100.0, 100.0, 102.0, 102.0));
    }

    #[test]
    fn opposite_corner_stays_fixed() {
        let b = Box2::new(100.0, 100.0, 200.0, 200.0);
        let r = apply_drag(b, Handle::TopRight, Point::new(15.0, -7.0), LIM);
        assert_eq!(r, Box2::new(100.0, 93.0, 215.0, 200.0));
    }

    #[test]
    fn move_stays_in_bounds() {
        let b = Box2::new(100.0, 100.0, 200.0, 200.0);
        let r = apply_drag(b, Handle::Move, Point::new(-500.0, 2000.0), LIM);
        assert_eq!(r, Box2::new(0.0, 900.0, 100.0, 1000.0));
    }

    #[test]
    fn stretch_matches_spec_example() {
        let b = Box2::from_xywh(100.0, 400.0, 200.0, 100.0);
        let s = coco_xywh(stretch_box(b, 850, 1280, 640, 640));
        let want = [75.29, 200.00, 150.59, 50.00];
        for i in 0..4 {
            assert!((s[i] - want[i]).abs() < 0.01, "{s:?}");
        }
    }
}
