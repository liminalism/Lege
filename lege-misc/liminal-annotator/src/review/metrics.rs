//! Model-vs-human metrics on reviewed pages: per-class precision/recall at
//! IoU 0.5, text/figure group confusion, figure recall and clipping. Ignore
//! regions are excluded from both sides (a prediction mostly inside one is
//! neither a hit nor a false positive).

use serde::Serialize;

use crate::annotation::commands::Annotation;
use crate::annotation::labels::{class, Group, CLASSES, NUM_CLASSES};
use crate::dataset::database::Prediction;

use super::comparison::{match_boxes, Det};

#[derive(Clone, Debug, Default, Serialize)]
pub struct ClassStats {
    pub name: String,
    pub tp: usize,
    pub fp: usize,
    pub fn_: usize,
    pub precision: f64,
    pub recall: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub model: String,
    pub pages: usize,
    /// Which pages served as ground truth (the queue description).
    pub truth: String,
    pub score_threshold: f64,
    pub classes: Vec<ClassStats>,
    /// [truth group][pred group] counts of group-level matches (IoU >= 0.5, class ignored).
    pub group_confusion: Vec<(String, String, usize)>,
    pub figure_recall: f64,
    /// Matched figures whose prediction covers < 95% of the true figure.
    pub figure_clipped_rate: f64,
    /// Matched text-like boxes whose prediction covers < 95% of the truth height or width.
    pub text_clipped_rate: f64,
    /// Truth text regions matched (any class) by a figure prediction, and vice versa.
    pub text_as_figure: usize,
    pub figure_as_text: usize,
}

#[derive(Default)]
pub struct Accumulator {
    tp: [usize; NUM_CLASSES],
    fp: [usize; NUM_CLASSES],
    fn_: [usize; NUM_CLASSES],
    group: std::collections::BTreeMap<(&'static str, &'static str), usize>,
    fig_truth: usize,
    fig_found: usize,
    fig_matched: usize,
    fig_clipped: usize,
    text_matched: usize,
    text_clipped: usize,
    text_as_figure: usize,
    figure_as_text: usize,
    pages: usize,
}

fn coverage(
    pred: &crate::annotation::geometry::Box2,
    truth: &crate::annotation::geometry::Box2,
) -> f64 {
    let a = truth.area();
    if a <= 0.0 {
        return 1.0;
    }
    pred.intersection(truth).area() / a
}

impl Accumulator {
    pub fn add_page(&mut self, truth: &[Annotation], preds: &[Prediction], score_min: f64) {
        self.pages += 1;
        let ignore: Vec<_> = truth
            .iter()
            .filter(|a| a.is_ignore)
            .map(|a| a.bbox)
            .collect();
        let t: Vec<Det> = truth
            .iter()
            .filter(|a| !a.is_ignore)
            .map(|a| Det {
                class_id: a.class_id,
                bbox: a.bbox,
                score: 1.0,
            })
            .collect();
        let p: Vec<Det> = preds
            .iter()
            .filter(|p| p.score >= score_min)
            .filter(|p| {
                let area = p.bbox.area();
                area <= 0.0
                    || !ignore
                        .iter()
                        .any(|ig| p.bbox.intersection(ig).area() / area >= 0.5)
            })
            .map(|p| Det {
                class_id: p.class_id,
                bbox: p.bbox,
                score: p.score,
            })
            .collect();

        let m = match_boxes(&p, &t, 0.5, |a, b| a == b);
        for &(pi, ti, _) in &m.matched {
            self.tp[t[ti].class_id as usize] += 1;
            let c = coverage(&p[pi].bbox, &t[ti].bbox);
            match class(t[ti].class_id).group {
                Group::Figure => {
                    self.fig_matched += 1;
                    if c < 0.95 {
                        self.fig_clipped += 1;
                    }
                }
                Group::TextLike => {
                    self.text_matched += 1;
                    if c < 0.95 {
                        self.text_clipped += 1;
                    }
                }
                _ => {}
            }
        }
        for &pi in &m.unmatched_pred {
            self.fp[p[pi].class_id as usize] += 1;
        }
        for &ti in &m.unmatched_truth {
            self.fn_[t[ti].class_id as usize] += 1;
        }

        // Group-level, class-agnostic matching for confusion.
        let g = match_boxes(&p, &t, 0.5, |_, _| true);
        for &(pi, ti, _) in &g.matched {
            let tg = class(t[ti].class_id).group;
            let pg = class(p[pi].class_id).group;
            *self.group.entry((tg.name(), pg.name())).or_default() += 1;
            if tg == Group::TextLike && pg == Group::Figure {
                self.text_as_figure += 1;
            }
            if tg == Group::Figure && pg == Group::TextLike {
                self.figure_as_text += 1;
            }
        }
        self.fig_truth += t
            .iter()
            .filter(|d| class(d.class_id).group == Group::Figure)
            .count();
        for &(pi, ti, _) in &g.matched {
            if class(t[ti].class_id).group == Group::Figure
                && class(p[pi].class_id).group == Group::Figure
            {
                self.fig_found += 1;
            }
        }
    }

    pub fn finish(self, model: &str, score_threshold: f64, truth: &str) -> Report {
        let ratio = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
        let classes = CLASSES
            .iter()
            .map(|c| {
                let i = c.id as usize;
                ClassStats {
                    name: c.name.into(),
                    tp: self.tp[i],
                    fp: self.fp[i],
                    fn_: self.fn_[i],
                    precision: ratio(self.tp[i], self.tp[i] + self.fp[i]),
                    recall: ratio(self.tp[i], self.tp[i] + self.fn_[i]),
                }
            })
            .collect();
        Report {
            model: model.into(),
            pages: self.pages,
            truth: truth.into(),
            score_threshold,
            classes,
            group_confusion: self
                .group
                .into_iter()
                .map(|((a, b), n)| (a.into(), b.into(), n))
                .collect(),
            figure_recall: ratio(self.fig_found, self.fig_truth),
            figure_clipped_rate: ratio(self.fig_clipped, self.fig_matched),
            text_clipped_rate: ratio(self.text_clipped, self.text_matched),
            text_as_figure: self.text_as_figure,
            figure_as_text: self.figure_as_text,
        }
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "model {} on {} pages [{}] (score >= {})",
            self.model, self.pages, self.truth, self.score_threshold
        )?;
        writeln!(
            f,
            "{:<16} {:>6} {:>6} {:>6} {:>7} {:>7}",
            "class", "tp", "fp", "fn", "prec", "recall"
        )?;
        for c in &self.classes {
            if c.tp + c.fp + c.fn_ == 0 {
                continue;
            }
            writeln!(
                f,
                "{:<16} {:>6} {:>6} {:>6} {:>7.3} {:>7.3}",
                c.name, c.tp, c.fp, c.fn_, c.precision, c.recall
            )?;
        }
        writeln!(
            f,
            "figure recall (group) {:.3}, figure clipped {:.3}, text clipped {:.3}",
            self.figure_recall, self.figure_clipped_rate, self.text_clipped_rate
        )?;
        writeln!(
            f,
            "text predicted as figure {}, figure predicted as text {}",
            self.text_as_figure, self.figure_as_text
        )?;
        write!(f, "group confusion (truth -> pred):")?;
        for (t, p, n) in &self.group_confusion {
            write!(f, " {t}->{p}:{n}")?;
        }
        Ok(())
    }
}
