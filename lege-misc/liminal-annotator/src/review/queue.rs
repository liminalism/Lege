//! Review-priority ordering, diversified by book so one unusual book cannot
//! consume the review budget.

use std::collections::{BTreeMap, VecDeque};

/// `rows` are `(page_key, book_id, priority)`. Pages are ranked by priority
/// within each book; books then take turns, the book with the highest
/// remaining page going first in every round.
pub fn diversify(rows: Vec<(String, String, f64)>) -> Vec<String> {
    let mut by_book: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for (key, book, p) in rows {
        by_book.entry(book).or_default().push((key, p));
    }
    let mut queues: Vec<VecDeque<(String, f64)>> = by_book
        .into_values()
        .map(|mut v| {
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            v.into()
        })
        .collect();
    let mut out = Vec::new();
    loop {
        // Order books for this round by their best remaining page.
        let mut round: Vec<usize> = (0..queues.len())
            .filter(|&i| !queues[i].is_empty())
            .collect();
        if round.is_empty() {
            break;
        }
        round.sort_by(|&a, &b| queues[b][0].1.total_cmp(&queues[a][0].1));
        for i in round {
            if let Some((k, _)) = queues[i].pop_front() {
                out.push(k);
            }
        }
    }
    out
}

/// Priority signals for one page (spec §11). Each is in `0..=1` or a small
/// count; weights follow the spec's importance column.
#[derive(Clone, Debug, Default)]
pub struct Signals {
    /// Teacher/student text-vs-image disagreement (fraction of area).
    pub text_image_disagreement: f64,
    /// Large figure regions overlapped by text detections.
    pub text_in_figure: f64,
    /// Low-confidence boxes near other classes / class conflicts.
    pub class_conflicts: f64,
    /// Ignore regions (unresolved boxes).
    pub ignore_regions: f64,
    /// Missing or clipped boxes (unmatched predictions/truth).
    pub unmatched: f64,
    /// Dark/aged scans, multi-column, gutter crossings.
    pub hard_layout: f64,
    /// Rare classes present.
    pub rare_classes: f64,
}

impl Signals {
    pub fn score(&self) -> f64 {
        3.0 * self.text_image_disagreement.min(1.0)
            + 3.0 * self.text_in_figure.min(1.0)
            + 2.0 * (self.class_conflicts / 3.0).min(1.0)
            + 2.0 * (self.unmatched / 3.0).min(1.0)
            + 1.5 * self.hard_layout.min(1.0)
            + 1.5 * (self.rare_classes / 2.0).min(1.0)
            + 1.0 * (self.ignore_regions / 4.0).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diversify_interleaves_books() {
        let rows = vec![
            ("a1".into(), "A".into(), 9.0),
            ("a2".into(), "A".into(), 8.0),
            ("a3".into(), "A".into(), 7.0),
            ("b1".into(), "B".into(), 1.0),
            ("c1".into(), "C".into(), 5.0),
        ];
        assert_eq!(diversify(rows), vec!["a1", "c1", "b1", "a2", "a3"]);
    }
}
