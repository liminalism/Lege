//! Picking the annotation and handle under the pointer, in screen space.
//!
//! Priority (spec §4): a corner of the selected box, then the closest edge of
//! any box, then the innermost (smallest) box whose interior contains the
//! pointer — which is also the box drawn on top.

use crate::annotation::commands::Annotation;
use crate::annotation::geometry::{Handle, Point, hit_box_scored};

use super::viewport::ViewTransform;

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub index: usize,
    pub handle: Handle,
}

pub fn pick(
    annotations: &[Annotation],
    selected: Option<&str>,
    view: &ViewTransform,
    screen: Point,
    slop: f64,
) -> Option<Hit> {
    // 1. Selected box handles.
    if let Some(sel) = selected {
        if let Some(i) = annotations.iter().position(|a| a.id == sel) {
            let b = view.box_to_screen(annotations[i].bbox);
            if let Some((_, h)) = hit_box_scored(b, screen, slop) {
                if h.is_corner() {
                    return Some(Hit {
                        index: i,
                        handle: h,
                    });
                }
            }
        }
    }
    // 2. Closest edge anywhere; 3. smallest interior.
    let mut best_edge: Option<(f64, f64, usize, Handle)> = None; // (dist, area, idx, handle)
    let mut best_inner: Option<(f64, usize)> = None;
    for (i, a) in annotations.iter().enumerate() {
        let b = view.box_to_screen(a.bbox);
        // Cheap reject.
        if screen.x < b.x0 - slop
            || screen.x > b.x1 + slop
            || screen.y < b.y0 - slop
            || screen.y > b.y1 + slop
        {
            continue;
        }
        match hit_box_scored(b, screen, slop) {
            Some((d, h)) if h.is_corner() => {
                let area = b.area();
                let better = match best_edge {
                    None => true,
                    // Ties (shared edges) go to the smaller box.
                    Some((bd, ba, _, _)) => d < bd - 1e-9 || ((d - bd).abs() <= 1e-9 && area < ba),
                };
                if better {
                    best_edge = Some((d, area, i, h));
                }
            }
            Some((_, Handle::Move)) => {
                let area = b.area();
                if best_inner.is_none_or(|(ba, _)| area < ba) {
                    best_inner = Some((area, i));
                }
            }
            _ => {}
        }
    }
    if let Some((_, _, i, h)) = best_edge {
        return Some(Hit {
            index: i,
            handle: h,
        });
    }
    best_inner.map(|(_, i)| Hit {
        index: i,
        handle: Handle::Move,
    })
}
