//! Character-style runs over a block's rope.
//!
//! Runs are half-open character ranges. Inside a block they are contiguous,
//! non-overlapping and ordered. An empty block has one empty run so the
//! caret still has marks to inherit.

use crate::error::ModelError;

/// Vertical position of a run relative to the baseline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Script {
    /// On the baseline.
    #[default]
    Normal,
    /// Superscript.
    Super,
    /// Subscript.
    Sub,
}

/// Marks carried by a run. Named paragraph and character styles are a later
/// milestone; these are the direct marks stored with the text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunMarks {
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Small caps.
    pub small_caps: bool,
    /// Superscript or subscript.
    pub script: Script,
    /// BCP 47 language tag, when the run overrides the paragraph language.
    pub language: Option<String>,
    /// Link target. The string is the href; resolution is not this crate's job.
    pub link: Option<String>,
    /// Citation key referring to a source record. The record itself arrives in a later milestone.
    pub citation: Option<String>,
}

/// One styled span of a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// First character, inclusive.
    pub start: usize,
    /// First character after the run.
    pub end: usize,
    /// Marks applied to every character in the span.
    pub marks: RunMarks,
}

impl Run {
    pub(crate) fn span(start: usize, end: usize, marks: RunMarks) -> Self {
        Self { start, end, marks }
    }
}

pub(crate) fn char_count(text: &str) -> usize {
    text.chars().count()
}

pub(crate) fn marks_before(runs: &[Run], at: usize) -> RunMarks {
    if runs.is_empty() || at == 0 {
        return runs
            .first()
            .map(|run| run.marks.clone())
            .unwrap_or_default();
    }
    for run in runs.iter().rev() {
        if run.start < at {
            return run.marks.clone();
        }
    }
    runs[0].marks.clone()
}

/// Runs covering `[from, to)`, rebased so the slice starts at 0.
pub(crate) fn slice_runs(runs: &[Run], from: usize, to: usize) -> Vec<Run> {
    if to <= from {
        return Vec::new();
    }
    let mut out = Vec::new();
    for run in runs {
        let start = run.start.max(from);
        let end = run.end.min(to);
        if start < end {
            out.push(Run::span(start - from, end - from, run.marks.clone()));
        }
    }
    out
}

pub(crate) fn covering(len: usize, marks: RunMarks) -> Vec<Run> {
    vec![Run::span(0, len, marks)]
}

/// Replace the character span `[start, start + delete_len)` and insert `inserted`
/// (ranges relative to the inserted text) at `start`.
pub(crate) fn rewrite_runs(
    old: &[Run],
    start: usize,
    delete_len: usize,
    inserted: &[Run],
) -> Result<Vec<Run>, ModelError> {
    let Some(delete_end) = start.checked_add(delete_len) else {
        return Err(ModelError::EditConflict);
    };
    let insert_len = inserted.last().map(|run| run.end).unwrap_or(0);
    if !inserted_cover(inserted, insert_len) {
        return Err(ModelError::InvalidRuns);
    }
    let empty_marks = old.first().map(|run| run.marks.clone()).unwrap_or_default();
    let mut out = Vec::new();
    for run in old {
        if run.end <= start {
            if run.start != run.end {
                out.push(run.clone());
            }
            continue;
        }
        if run.start >= delete_end {
            if let Some(shifted) = shift_run(run, delete_len, insert_len) {
                if shifted.start != shifted.end {
                    out.push(shifted);
                }
            } else {
                return Err(ModelError::EditConflict);
            }
            continue;
        }
        if run.start < start {
            out.push(Run::span(run.start, start, run.marks.clone()));
        }
        if run.end > delete_end {
            let kept = run.end - delete_end;
            let new_start = start + insert_len;
            out.push(Run::span(new_start, new_start + kept, run.marks.clone()));
        }
    }
    for run in inserted {
        if run.start == run.end {
            continue;
        }
        out.push(Run::span(
            run.start + start,
            run.end + start,
            run.marks.clone(),
        ));
    }
    out.sort_by_key(|run| (run.start, run.end));
    Ok(coalesce(out, empty_marks))
}

fn inserted_cover(inserted: &[Run], len: usize) -> bool {
    if len == 0 {
        return inserted.iter().all(|run| run.start == run.end);
    }
    if inserted.is_empty() || inserted[0].start != 0 {
        return false;
    }
    let mut at = 0;
    for run in inserted {
        if run.start != at || run.end < run.start {
            return false;
        }
        at = run.end;
    }
    at == len
}

fn shift_run(run: &Run, delete_len: usize, insert_len: usize) -> Option<Run> {
    let start = run.start.checked_sub(delete_len)?.checked_add(insert_len)?;
    let end = run.end.checked_sub(delete_len)?.checked_add(insert_len)?;
    Some(Run::span(start, end, run.marks.clone()))
}

pub(crate) fn coalesce(runs: Vec<Run>, empty_marks: RunMarks) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for run in runs {
        if run.start == run.end {
            continue;
        }
        if let Some(prev) = out.last_mut()
            && prev.end == run.start
            && prev.marks == run.marks
        {
            prev.end = run.end;
            continue;
        }
        out.push(run);
    }
    if out.is_empty() {
        out.push(Run::span(0, 0, empty_marks));
    }
    out
}

pub(crate) fn runs_cover(runs: &[Run], len: usize) -> bool {
    if len == 0 {
        return runs.len() == 1 && runs[0].start == 0 && runs[0].end == 0;
    }
    if runs.is_empty() || runs[0].start != 0 {
        return false;
    }
    let mut at = 0;
    for run in runs {
        if run.start != at || run.end <= run.start {
            return false;
        }
        at = run.end;
    }
    at == len
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn bold_italic() -> (RunMarks, RunMarks) {
        let bold = RunMarks {
            bold: true,
            ..RunMarks::default()
        };
        let italic = RunMarks {
            italic: true,
            ..RunMarks::default()
        };
        (bold, italic)
    }

    #[test]
    fn insert_at_a_boundary_extends_the_left_run() {
        let (bold, italic) = bold_italic();
        let old = vec![
            Run::span(0, 5, bold.clone()),
            Run::span(5, 10, italic.clone()),
        ];
        let inserted = vec![Run::span(0, 3, bold.clone())];
        let got = rewrite_runs(&old, 5, 0, &inserted).expect("runs");
        assert_eq!(got, vec![Run::span(0, 8, bold), Run::span(8, 13, italic)]);
    }

    #[test]
    fn delete_across_two_runs_keeps_both_marks() {
        let (bold, italic) = bold_italic();
        let old = vec![
            Run::span(0, 5, bold.clone()),
            Run::span(5, 10, italic.clone()),
        ];
        let got = rewrite_runs(&old, 3, 5, &[]).expect("runs");
        assert_eq!(got, vec![Run::span(0, 3, bold), Run::span(3, 5, italic)]);
    }

    #[test]
    fn deleting_everything_leaves_one_empty_run() {
        let (bold, _) = bold_italic();
        let old = vec![Run::span(0, 4, bold.clone())];
        let got = rewrite_runs(&old, 0, 4, &[]).expect("runs");
        assert_eq!(got, vec![Run::span(0, 0, bold)]);
    }
}
