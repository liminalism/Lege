//! What the reviewer changed relative to the imported layer, and how model
//! predictions match a reference layer.

use std::collections::HashMap;

use crate::annotation::commands::Annotation;
use crate::annotation::geometry::Box2;

/// Geometry within this many image pixels counts as unchanged (float noise
/// from storage, never a real edit).
pub const GEOMETRY_EPS: f64 = 1e-6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Unchanged,
    Modified {
        geometry: bool,
        class: bool,
        ignore: bool,
    },
    Added,
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Change::Unchanged => "unchanged",
            Change::Modified { .. } => "modified",
            Change::Added => "added",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PageDiff {
    /// Parallel to the current annotations.
    pub changes: Vec<Change>,
    /// Imported boxes the reviewer deleted.
    pub deleted: Vec<Annotation>,
}

impl PageDiff {
    pub fn is_untouched(&self) -> bool {
        self.deleted.is_empty() && self.changes.iter().all(|c| *c == Change::Unchanged)
    }

    pub fn count(&self) -> (usize, usize, usize, usize) {
        let mut u = 0;
        let mut m = 0;
        let mut a = 0;
        for c in &self.changes {
            match c {
                Change::Unchanged => u += 1,
                Change::Modified { .. } => m += 1,
                Change::Added => a += 1,
            }
        }
        (u, m, a, self.deleted.len())
    }
}

pub fn diff(current: &[Annotation], imported: &[Annotation]) -> PageDiff {
    let by_id: HashMap<&str, &Annotation> = imported.iter().map(|a| (a.id.as_str(), a)).collect();
    let mut seen = std::collections::HashSet::new();
    let changes = current
        .iter()
        .map(|a| match by_id.get(a.id.as_str()) {
            None => Change::Added,
            Some(orig) => {
                seen.insert(a.id.as_str());
                let geometry = !a.bbox.approx_eq(&orig.bbox, GEOMETRY_EPS);
                let class = a.class_id != orig.class_id;
                let ignore = a.is_ignore != orig.is_ignore;
                if geometry || class || ignore {
                    Change::Modified {
                        geometry,
                        class,
                        ignore,
                    }
                } else {
                    Change::Unchanged
                }
            }
        })
        .collect();
    let deleted = imported
        .iter()
        .filter(|a| !seen.contains(a.id.as_str()))
        .cloned()
        .collect();
    PageDiff { changes, deleted }
}

/// A box to be matched: class, geometry, score (1 for ground truth).
#[derive(Clone, Copy, Debug)]
pub struct Det {
    pub class_id: u8,
    pub bbox: Box2,
    pub score: f64,
}

#[derive(Clone, Debug, Default)]
pub struct MatchResult {
    /// (pred index, truth index, iou)
    pub matched: Vec<(usize, usize, f64)>,
    pub unmatched_pred: Vec<usize>,
    pub unmatched_truth: Vec<usize>,
}

/// Greedy matching by descending score then IoU. `same_class` restricts
/// matches to equal classes; otherwise `class_ok` decides.
pub fn match_boxes(
    preds: &[Det],
    truth: &[Det],
    iou_min: f64,
    class_ok: impl Fn(u8, u8) -> bool,
) -> MatchResult {
    let mut order: Vec<usize> = (0..preds.len()).collect();
    order.sort_by(|&a, &b| preds[b].score.total_cmp(&preds[a].score));
    let mut used = vec![false; truth.len()];
    let mut res = MatchResult::default();
    for pi in order {
        let p = &preds[pi];
        let mut best: Option<(usize, f64)> = None;
        for (ti, t) in truth.iter().enumerate() {
            if used[ti] || !class_ok(p.class_id, t.class_id) {
                continue;
            }
            let iou = p.bbox.iou(&t.bbox);
            if iou >= iou_min && best.is_none_or(|(_, b)| iou > b) {
                best = Some((ti, iou));
            }
        }
        match best {
            Some((ti, iou)) => {
                used[ti] = true;
                res.matched.push((pi, ti, iou));
            }
            None => res.unmatched_pred.push(pi),
        }
    }
    res.unmatched_truth = (0..truth.len()).filter(|&i| !used[i]).collect();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(id: &str, x: f64, c: u8) -> Annotation {
        Annotation {
            id: id.into(),
            class_id: c,
            bbox: Box2::new(x, 0.0, x + 5.0, 5.0),
            is_ignore: false,
            provenance_json: None,
        }
    }

    #[test]
    fn diff_classifies() {
        let imported = vec![a("1", 0.0, 0), a("2", 10.0, 0), a("3", 20.0, 0)];
        let current = vec![a("1", 0.0, 0), a("2", 11.0, 0), a("h", 40.0, 6)];
        let d = diff(&current, &imported);
        assert_eq!(d.changes[0], Change::Unchanged);
        assert!(matches!(
            d.changes[1],
            Change::Modified {
                geometry: true,
                class: false,
                ..
            }
        ));
        assert_eq!(d.changes[2], Change::Added);
        assert_eq!(d.deleted.len(), 1);
        assert_eq!(d.deleted[0].id, "3");
    }
}
