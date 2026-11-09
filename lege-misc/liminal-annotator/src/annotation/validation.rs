//! Box validation against page bounds.

use super::geometry::Box2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    Degenerate,
    OutOfBounds,
    NotFinite,
}

/// Problems with a box on a `w × h` page, if any.
pub fn check(b: &Box2, w: f64, h: f64) -> Option<Problem> {
    if !(b.x0.is_finite() && b.y0.is_finite() && b.x1.is_finite() && b.y1.is_finite()) {
        return Some(Problem::NotFinite);
    }
    if !(b.x1 > b.x0 && b.y1 > b.y0) {
        return Some(Problem::Degenerate);
    }
    if b.x0 < 0.0 || b.y0 < 0.0 || b.x1 > w || b.y1 > h {
        return Some(Problem::OutOfBounds);
    }
    None
}

/// Clamp into the page; `None` when nothing is left.
pub fn clamp(b: &Box2, w: f64, h: f64) -> Option<Box2> {
    let c = b.clamp_to(w, h);
    c.is_valid().then_some(c)
}
