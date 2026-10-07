//! Strict `/ByteRange` validation over the original bytes.
//!
//! A signature's `/ByteRange` is an array of offset/length number pairs
//! straddling the `/Contents` hole: the ranges the signature claims to
//! cover. [`assess`] classifies one such array against the original file
//! length as neutral facts — shape (at least four numbers, an even count,
//! all non-negative integers), ordering, and bounds — without touching
//! cryptography. Whether the bytes in the covered ranges verify is a
//! separate, later question this layer never asks.

/// One offset/length pair of a `/ByteRange`, as claimed.
///
/// Absolute source positions: `/ByteRange` offsets count from the start
/// of the file, not from the `%PDF` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRangeSpan {
    /// Absolute source offset where the covered run starts.
    pub offset: u64,
    /// Length of the covered run in bytes.
    pub length: u64,
}

impl ByteRangeSpan {
    /// One past the last covered byte, or `None` when the pair overflows.
    #[must_use]
    pub const fn end(self) -> Option<u64> {
        self.offset.checked_add(self.length)
    }
}

/// Strict classification of a `/ByteRange` against the original file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRangeValidity {
    /// Well-formed and spanning the whole file: starts at zero, ends at
    /// the final byte, pairs ordered and non-overlapping, everything in
    /// bounds. The single uncovered run between the pairs is the
    /// `/Contents` hole.
    Valid,
    /// Well-formed but leaving file bytes uncovered: a leading gap, a
    /// trailing gap, or both. Ordinary for a signature with legitimate
    /// incremental updates appended after signing — the revision check
    /// decides whether the gap is explained.
    GapContaining,
    /// A pair reaches past the end of the file.
    Truncated,
    /// Pairs overlap or run out of order.
    Overlapping,
    /// Wrong shape: fewer than four numbers, an odd count, a negative
    /// value, or a non-integer entry.
    Malformed,
}

/// The strict assessment of one `/ByteRange`: its classification, the
/// claimed covered runs, and a human-readable note when not valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteRangeAssessment {
    /// The classification.
    pub validity: ByteRangeValidity,
    /// The claimed covered runs, in `/ByteRange` order. Populated whenever
    /// the shape parses as offset/length pairs (at least four numbers, an
    /// even count, all non-negative integers) — including overlapping and
    /// out-of-bounds claims, so consumers see what was claimed. Empty for
    /// malformed shapes.
    pub covered: Vec<ByteRangeSpan>,
    /// Why the range is not [`ByteRangeValidity::Valid`]. Always `None`
    /// for valid ranges.
    pub detail: Option<String>,
}

/// Strictly assess a `/ByteRange` against the original file length.
///
/// `entries` is the array's every entry in order — `None` for an entry
/// that is not an integer (a name, a string, a null) — and `file_len` the
/// original source length in bytes. Checks run shape first, then ordering
/// and bounds pair by pair, then whole-file coverage, so the first failure
/// in that order is the one reported.
#[must_use]
pub fn assess(entries: &[Option<i64>], file_len: u64) -> ByteRangeAssessment {
    let malformed = |detail: String| ByteRangeAssessment {
        validity: ByteRangeValidity::Malformed,
        covered: Vec::new(),
        detail: Some(detail),
    };
    if entries.len() < 4 {
        return malformed(format!("fewer than four numbers (found {})", entries.len()));
    }
    if !entries.len().is_multiple_of(2) {
        return malformed(format!("odd entry count ({})", entries.len()));
    }
    if entries.iter().any(Option::is_none) {
        return malformed("a non-integer entry is present".to_owned());
    }
    let values: Vec<i64> = entries.iter().map(|entry| entry.unwrap_or(0)).collect();
    if values.iter().any(|value| *value < 0) {
        return malformed("a negative value is present".to_owned());
    }
    let covered: Vec<ByteRangeSpan> = values
        .chunks_exact(2)
        .map(|pair| ByteRangeSpan {
            offset: pair[0] as u64,
            length: pair[1] as u64,
        })
        .collect();

    let mut reach: u64 = 0;
    for (index, span) in covered.iter().enumerate() {
        let offset = span.offset;
        let end = span.end();
        if offset < reach {
            return ByteRangeAssessment {
                validity: ByteRangeValidity::Overlapping,
                covered,
                detail: Some(format!(
                    "pair {index} starts at {offset} before the previous pair ends at {reach}"
                )),
            };
        }
        match end {
            Some(end) if end <= file_len => reach = end,
            _ => {
                return ByteRangeAssessment {
                    validity: ByteRangeValidity::Truncated,
                    covered,
                    detail: Some(format!(
                        "pair {index} ends past the file end ({file_len} bytes)"
                    )),
                };
            }
        }
    }

    let starts_at_zero = covered.first().is_some_and(|span| span.offset == 0);
    if starts_at_zero && reach == file_len {
        ByteRangeAssessment {
            validity: ByteRangeValidity::Valid,
            covered,
            detail: None,
        }
    } else {
        ByteRangeAssessment {
            validity: ByteRangeValidity::GapContaining,
            covered,
            detail: Some(format!("covered runs end at byte {reach} of {file_len}")),
        }
    }
}
