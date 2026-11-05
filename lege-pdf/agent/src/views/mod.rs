//! Serde view types over engine data (engine crates stay serde-free).

pub mod content;
pub mod image;
pub mod print;
pub mod render;
pub mod report;
pub mod search;
pub mod tables;
pub mod text;

use serde::{Serialize, Serializer};

/// A coordinate as it is reported: to a hundredth of a point.
///
/// Engine geometry is `f64`, and fifteen significant digits per coordinate
/// made word boxes most of a text record's bytes. A hundredth of a point is
/// 1/7200 inch — finer than any use of these boxes — and the value written is
/// the value a reader gets back.
pub(crate) fn round2(value: f64) -> f64 {
    // Adding zero turns the `-0.0` that rounding a tiny negative leaves into `0.0`.
    (value * 100.0).round() / 100.0 + 0.0
}

/// Text as one tidy line: a run of leader dots (`Chapter 1 ........ 12`)
/// becomes a single ellipsis, and runs of spaces one space.
pub(crate) fn tidy_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut words = text.split_whitespace().peekable();
    let mut in_leader = false;
    while let Some(word) = words.next() {
        let leader = word.len() >= 3 && word.chars().all(|character| character == '.');
        // A leader glued to the word before it: `CONTENTS.......`
        let (stem, trailing) = match word.find("...") {
            Some(at) if !leader && word[at..].chars().all(|character| character == '.') => {
                (&word[..at], true)
            }
            _ => (word, false),
        };
        if leader {
            if !in_leader {
                out.push_str(" …");
            }
            in_leader = true;
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(stem);
        in_leader = trailing;
        if trailing {
            out.push_str(" …");
        }
        let _ = words.peek();
    }
    out
}

pub(crate) fn rounded_box<S: Serializer>(value: &[f64; 4], out: S) -> Result<S::Ok, S::Error> {
    value.map(round2).serialize(out)
}

pub(crate) fn rounded_box_opt<S: Serializer>(
    value: &Option<[f64; 4]>,
    out: S,
) -> Result<S::Ok, S::Error> {
    value.map(|rect| rect.map(round2)).serialize(out)
}
