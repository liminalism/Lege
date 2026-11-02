// "The comparator's performance is critical; rewriting it to pixel-by-pixel is unacceptable.
// Fix bugs without changing the high-level algorithm."

//! jbig2comparator.rs
//! ==========================================================
//! Stage 1 keeps the existing word-wise shifted XOR/popcount
//! search to find the best alignment quickly.
//!
//! Stage 2 runs only once for that best shift and extracts
//! richer glyph metrics so family matching can distinguish
//! structural mismatch from harmless edge noise.
//! ==========================================================

use crate::jbig2simd::{shift_xor_popcnt_u32_rows, xor_popcnt_u32_rows};
use crate::jbig2sym::BitImage;

/// Maximum absolute shift (in pixels) that we search in x/y.
const SEARCH_RADIUS: i32 = 5;
const SEARCH_OFFSETS: [i32; (SEARCH_RADIUS as usize) * 2 + 1] =
    [0, -1, 1, -2, 2, -3, 3, -4, 4, -5, 5];
/// Maximum width/height delta that can still produce a match.
pub const MAX_DIMENSION_DELTA: usize = (SEARCH_RADIUS as usize) * 2;

#[derive(Debug, Clone, Copy, Default)]
pub struct CompareResult {
    pub total_err: u32,
    pub overlap_err: u32,
    pub outside_ink_err: u32,
    pub row_profile_err: u32,
    pub col_profile_err: u32,
    pub black_delta: u32,
    pub dx: i32,
    pub dy: i32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CollapseCompareLimits {
    pub outside_limit: u32,
    pub row_limit: u32,
    pub col_limit: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct CompareWeights {
    pub overlap_err: u32,
    pub outside_ink_err: u32,
    pub row_profile_err: u32,
    pub col_profile_err: u32,
    pub black_delta: u32,
}

impl CompareWeights {
    pub const LIVE_MATCH: Self = Self {
        overlap_err: 8,
        outside_ink_err: 5,
        row_profile_err: 2,
        col_profile_err: 2,
        black_delta: 1,
    };

    pub const COLLAPSE: Self = Self {
        overlap_err: 6,
        outside_ink_err: 3,
        row_profile_err: 2,
        col_profile_err: 2,
        black_delta: 1,
    };

    pub const REFINE: Self = Self {
        overlap_err: 6,
        outside_ink_err: 4,
        row_profile_err: 2,
        col_profile_err: 2,
        black_delta: 1,
    };

    #[inline]
    pub fn score(&self, r: &CompareResult) -> u32 {
        self.overlap_err
            .saturating_mul(r.overlap_err)
            .saturating_add(self.outside_ink_err.saturating_mul(r.outside_ink_err))
            .saturating_add(self.row_profile_err.saturating_mul(r.row_profile_err))
            .saturating_add(self.col_profile_err.saturating_mul(r.col_profile_err))
            .saturating_add(self.black_delta.saturating_mul(r.black_delta))
    }
}

#[derive(Default)]
pub struct Comparator {
    col_a: Vec<u32>,
    col_b: Vec<u32>,
}

impl Comparator {
    #[inline]
    fn exact_match(a: &BitImage, b: &BitImage) -> bool {
        a.width == b.width && a.height == b.height && a.packed_words() == b.packed_words()
    }

    pub fn distance(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
    ) -> Option<(u32, i32, i32)> {
        self.best_alignment_result(a, b, max_err)
    }

    pub fn compare_overlap_only(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
    ) -> Option<CompareResult> {
        if Self::exact_match(a, b) {
            return Some(CompareResult {
                dx: 0,
                dy: 0,
                black_delta: 0,
                ..Default::default()
            });
        }

        if a.width.abs_diff(b.width) > MAX_DIMENSION_DELTA
            || a.height.abs_diff(b.height) > MAX_DIMENSION_DELTA
        {
            return None;
        }

        let (overlap_err, dx, dy) = self.best_alignment_by_xor(a, b, max_err)?;
        Some(CompareResult {
            total_err: overlap_err,
            overlap_err,
            black_delta: a.count_ones().abs_diff(b.count_ones()) as u32,
            dx,
            dy,
            ..Default::default()
        })
    }

    pub fn compare_detailed(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
    ) -> Option<CompareResult> {
        self.compare_detailed_with_limits(a, b, max_err, SEARCH_RADIUS, SEARCH_RADIUS)
    }

    pub fn compare_detailed_with_limits(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
        max_dx: i32,
        max_dy: i32,
    ) -> Option<CompareResult> {
        if Self::exact_match(a, b) {
            return Some(CompareResult {
                dx: 0,
                dy: 0,
                ..Default::default()
            });
        }

        if a.width.abs_diff(b.width) > MAX_DIMENSION_DELTA
            || a.height.abs_diff(b.height) > MAX_DIMENSION_DELTA
        {
            return None;
        }

        let (overlap_err, dx, dy) =
            self.best_alignment_by_xor_limited(a, b, max_err, max_dx, max_dy)?;
        let mut result = self.metrics_for_alignment(a, b, dx, dy);
        result.overlap_err = overlap_err.max(result.overlap_err);
        result.total_err = result.overlap_err.saturating_add(result.outside_ink_err);

        if result.total_err <= max_err {
            Some(result)
        } else {
            None
        }
    }

    pub fn compare_for_refine_family(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
        max_dx: i32,
        max_dy: i32,
    ) -> Option<CompareResult> {
        let (overlap_err, dx, dy) =
            self.best_alignment_result_with_limits(a, b, max_err, max_dx, max_dy)?;
        let result = CompareResult {
            total_err: overlap_err,
            overlap_err,
            dx,
            dy,
            ..Default::default()
        };
        if result.dx.abs() > max_dx || result.dy.abs() > max_dy {
            return None;
        }
        Some(result)
    }

    #[inline]
    fn best_alignment_result(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
    ) -> Option<(u32, i32, i32)> {
        self.best_alignment_result_with_limits(a, b, max_err, SEARCH_RADIUS, SEARCH_RADIUS)
    }

    #[inline]
    fn best_alignment_result_with_limits(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
        max_dx: i32,
        max_dy: i32,
    ) -> Option<(u32, i32, i32)> {
        if Self::exact_match(a, b) {
            return Some((0, 0, 0));
        }

        if a.width.abs_diff(b.width) > MAX_DIMENSION_DELTA
            || a.height.abs_diff(b.height) > MAX_DIMENSION_DELTA
        {
            return None;
        }

        self.best_alignment_by_xor_limited(a, b, max_err, max_dx, max_dy)
    }

    pub fn compare_for_collapse_family(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
        max_dx: i32,
        max_dy: i32,
    ) -> Option<CompareResult> {
        let result = self.compare_detailed_with_limits(a, b, max_err, max_dx, max_dy)?;
        if result.dx.abs() > max_dx || result.dy.abs() > max_dy {
            return None;
        }

        let limits = Self::collapse_compare_limits(&result);

        if result.outside_ink_err > limits.outside_limit
            || result.row_profile_err > limits.row_limit
            || result.col_profile_err > limits.col_limit
        {
            return None;
        }

        Some(result)
    }

    pub fn compare_for_symbol_unify(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
        max_dx: i32,
        max_dy: i32,
    ) -> Option<CompareResult> {
        let result = self.compare_detailed_with_limits(a, b, max_err, max_dx, max_dy)?;
        if result.dx.abs() > max_dx || result.dy.abs() > max_dy {
            return None;
        }
        Some(result)
    }

    pub fn collapse_compare_limits(result: &CompareResult) -> CollapseCompareLimits {
        let shift_slack = (result.dx.abs() + result.dy.abs()) as u32;
        CollapseCompareLimits {
            outside_limit: (result.overlap_err / 2).max(2)
                + result.black_delta.min(4)
                + shift_slack,
            row_limit: result.overlap_err.saturating_mul(6).saturating_add(24),
            col_limit: result.overlap_err.saturating_mul(6).saturating_add(24),
        }
    }

    fn best_alignment_by_xor(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
    ) -> Option<(u32, i32, i32)> {
        self.best_alignment_by_xor_limited(a, b, max_err, SEARCH_RADIUS, SEARCH_RADIUS)
    }

    fn best_alignment_by_xor_limited(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        max_err: u32,
        max_dx: i32,
        max_dy: i32,
    ) -> Option<(u32, i32, i32)> {
        match (a.width.div_ceil(32), b.width.div_ceil(32)) {
            (1, 1) => best_alignment_narrow::<1, 1>(a, b, max_err, max_dx, max_dy),
            (1, 2) => best_alignment_narrow::<1, 2>(a, b, max_err, max_dx, max_dy),
            (2, 1) => best_alignment_narrow::<2, 1>(a, b, max_err, max_dx, max_dy),
            (2, 2) => best_alignment_narrow::<2, 2>(a, b, max_err, max_dx, max_dy),
            _ => best_alignment_words(a, b, max_err, max_dx, max_dy),
        }
    }

    fn ensure_profile_buffers(&mut self, cols: usize) {
        if self.col_a.len() < cols {
            self.col_a.resize(cols, 0);
            self.col_b.resize(cols, 0);
        }
    }

    fn metrics_for_alignment(
        &mut self,
        a: &BitImage,
        b: &BitImage,
        dx: i32,
        dy: i32,
    ) -> CompareResult {
        let min_x = 0.min(dx);
        let min_y = 0.min(dy);
        let max_x = (a.width as i32).max(dx + b.width as i32);
        let max_y = (a.height as i32).max(dy + b.height as i32);
        let used_cols = (max_x - min_x).max(0) as usize;

        self.ensure_profile_buffers(used_cols);
        self.col_a[..used_cols].fill(0);
        self.col_b[..used_cols].fill(0);

        let mut overlap_err = 0u32;
        let mut outside_ink_err = 0u32;
        let mut row_profile_err = 0u32;
        for gy in min_y..max_y {
            let mut a_row_count = 0u32;
            let mut b_row_count = 0u32;

            for gx in min_x..max_x {
                let ax = gx;
                let ay = gy;
                let bx = gx - dx;
                let by = gy - dy;

                let in_a =
                    ax >= 0 && ay >= 0 && (ax as usize) < a.width && (ay as usize) < a.height;
                let in_b =
                    bx >= 0 && by >= 0 && (bx as usize) < b.width && (by as usize) < b.height;
                let a_on = in_a && a.get_usize(ax as usize, ay as usize);
                let b_on = in_b && b.get_usize(bx as usize, by as usize);

                if !(a_on || b_on) {
                    continue;
                }

                let rx = (gx - min_x) as usize;
                if a_on {
                    a_row_count += 1;
                    self.col_a[rx] += 1;
                }
                if b_on {
                    b_row_count += 1;
                    self.col_b[rx] += 1;
                }

                if a_on != b_on {
                    if in_a && in_b {
                        overlap_err += 1;
                    } else {
                        outside_ink_err += 1;
                    }
                }
            }

            row_profile_err += a_row_count.abs_diff(b_row_count);
        }

        let mut col_profile_err = 0u32;
        for i in 0..used_cols {
            col_profile_err += self.col_a[i].abs_diff(self.col_b[i]);
        }

        CompareResult {
            total_err: overlap_err.saturating_add(outside_ink_err),
            overlap_err,
            outside_ink_err,
            row_profile_err,
            col_profile_err,
            black_delta: a.count_ones().abs_diff(b.count_ones()) as u32,
            dx,
            dy,
        }
    }
}

/// General word-wise shifted XOR search behind
/// `Comparator::best_alignment_by_xor_limited`.
fn best_alignment_words(
    a: &BitImage,
    b: &BitImage,
    max_err: u32,
    max_dx: i32,
    max_dy: i32,
) -> Option<(u32, i32, i32)> {
    let awpr = (a.width + 31) >> 5;
    let bwpr = (b.width + 31) >> 5;
    let a_words = a.packed_words();
    let b_words = b.packed_words();

    let mut best_err = max_err + 1;
    let mut best_dx = 0;
    let mut best_dy = 0;

    for &dy in &SEARCH_OFFSETS {
        if dy.abs() > max_dy {
            continue;
        }
        let top_i = dy.max(0);
        let bottom_i = (dy + b.height as i32).min(a.height as i32);
        if top_i >= bottom_i {
            continue;
        }
        let top = top_i as usize;
        let bottom = bottom_i as usize;
        let overlap_height = bottom - top;
        let b_row_start = (top as i32 - dy) as usize;

        for &dx in &SEARCH_OFFSETS {
            if dx.abs() > max_dx {
                continue;
            }
            let left_i = dx.max(0);
            let right_i = (dx + b.width as i32).min(a.width as i32);
            if left_i >= right_i {
                continue;
            }

            let overlap_width = (right_i - left_i) as usize;
            let cols_words = (overlap_width + 31) >> 5;
            let bit_shift = (dx & 31) as u32;
            let word_shift = (dx >> 5) as isize;
            let mut err = 0u32;
            let mut early_break = false;

            for row in 0..overlap_height {
                let a_start = (top + row) * awpr;
                let b_start = (b_row_start + row) * bwpr;
                let a_row = &a_words[a_start..a_start + awpr];
                let b_row = &b_words[b_start..b_start + bwpr];

                let result = if bit_shift == 0 {
                    row_kernel_noshift(a_row, b_row, cols_words, word_shift, err, best_err, max_err)
                } else {
                    row_kernel_shift(
                        a_row, b_row, cols_words, word_shift, bit_shift, err, best_err, max_err,
                    )
                };
                err = result.0;
                if result.1 {
                    early_break = true;
                    break;
                }
            }

            if err < best_err {
                best_err = err;
                best_dx = dx;
                best_dy = dy;
            } else if err == best_err && !early_break {
                let curr_dist = dx.abs() + dy.abs();
                let best_dist = best_dx.abs() + best_dy.abs();
                if curr_dist < best_dist {
                    best_dx = dx;
                    best_dy = dy;
                }
            }
        }
    }

    if best_err <= max_err {
        Some((best_err, best_dx, best_dy))
    } else {
        None
    }
}

/// `best_alignment_by_xor_limited` for bitmaps at most 64 pixels wide, with
/// each row held in one `u64` (`AW`/`BW` packed words per row of `a`/`b`).
///
/// It visits the same offsets in the same order with the same tie rule and
/// returns the same alignment as [`best_alignment_words`]. Per row that
/// kernel XORs the first `ceil(overlap / 32)` words of `b` against `a`
/// shifted by `dx` with zero fill, which is `a << dx` (or `a >> -dx`) of the
/// row as one `u64`, masked to those words. It may stop a row after its first
/// word; that only happens once the error already reaches the best error,
/// and then the row total does too, so the candidate is rejected either way.
fn best_alignment_narrow<const AW: usize, const BW: usize>(
    a: &BitImage,
    b: &BitImage,
    max_err: u32,
    max_dx: i32,
    max_dy: i32,
) -> Option<(u32, i32, i32)> {
    #[inline(always)]
    fn row<const W: usize>(words: &[u32], index: usize) -> u64 {
        if W == 1 {
            u64::from(words[index]) << 32
        } else {
            (u64::from(words[2 * index]) << 32) | u64::from(words[2 * index + 1])
        }
    }
    let a_words = a.packed_words();
    let b_words = b.packed_words();
    debug_assert!(a_words.len() == a.height * AW && b_words.len() == b.height * BW);
    let mut best_err = max_err + 1;
    let mut best_dx = 0;
    let mut best_dy = 0;

    for &dy in &SEARCH_OFFSETS {
        if dy.abs() > max_dy {
            continue;
        }
        let top_i = dy.max(0);
        let bottom_i = (dy + b.height as i32).min(a.height as i32);
        if top_i >= bottom_i {
            continue;
        }
        let top = top_i as usize;
        let overlap_height = bottom_i as usize - top;
        let b_row_start = (top_i - dy) as usize;

        for &dx in &SEARCH_OFFSETS {
            if dx.abs() > max_dx {
                continue;
            }
            let left_i = dx.max(0);
            let right_i = (dx + b.width as i32).min(a.width as i32);
            if left_i >= right_i {
                continue;
            }
            let mask = if right_i - left_i <= 32 {
                0xffff_ffff_0000_0000u64
            } else {
                u64::MAX
            };
            let mut err = 0u32;
            let mut early_break = false;
            for offset in 0..overlap_height {
                let a_row = row::<AW>(a_words, top + offset);
                let b_row = row::<BW>(b_words, b_row_start + offset);
                let aligned = if dx >= 0 { a_row << dx } else { a_row >> -dx };
                err = err.saturating_add(((aligned ^ b_row) & mask).count_ones());
                if err >= best_err || err > max_err {
                    early_break = true;
                    break;
                }
            }

            if err < best_err {
                best_err = err;
                best_dx = dx;
                best_dy = dy;
            } else if err == best_err && !early_break {
                let curr_dist = dx.abs() + dy.abs();
                let best_dist = best_dx.abs() + best_dy.abs();
                if curr_dist < best_dist {
                    best_dx = dx;
                    best_dy = dy;
                }
            }
        }
    }

    if best_err <= max_err {
        Some((best_err, best_dx, best_dy))
    } else {
        None
    }
}

/// Row and column ink counts of a bitmap at most 32 pixels wide, for
/// [`narrow_alignment_impossible`]. Column `c` is stored at `columns[c + 1]`
/// so that one-pixel shifts read zero past either edge.
#[derive(Debug, Clone)]
pub(crate) struct NarrowProfile {
    rows: Vec<u8>,
    columns: [u16; 34],
}

impl NarrowProfile {
    pub(crate) fn new(image: &BitImage) -> Option<Self> {
        if image.width > 32 || image.height > usize::from(u16::MAX) {
            return None;
        }
        let words = image.packed_words();
        let mut columns = [0u16; 34];
        let mut rows = Vec::with_capacity(words.len());
        for &word in words {
            rows.push(word.count_ones() as u8);
            add_columns(&mut columns, word, 1);
        }
        Some(Self { rows, columns })
    }
}

#[inline]
fn add_columns(columns: &mut [u16; 34], word: u32, sign: i16) {
    let mut bits = word;
    while bits != 0 {
        let column = bits.leading_zeros() as usize;
        columns[column + 1] = columns[column + 1].wrapping_add_signed(sign);
        bits &= !(1u32 << (31 - column));
    }
}

/// Column counts of `profile`'s bitmap over rows `start..end`.
fn columns_over(profile: &NarrowProfile, image: &BitImage, start: usize, end: usize) -> [u16; 34] {
    let words = image.packed_words();
    let mut columns = profile.columns;
    for &word in words[..start].iter().chain(&words[end..]) {
        add_columns(&mut columns, word, -1);
    }
    columns
}

/// Whether every alignment with `|dx| <= max_dx` and `|dy| <= max_dy` (both
/// at most 1) has an XOR error above `max_err`, judged from ink counts alone.
/// Then `Comparator::compare_for_refine_family(a, b, max_err, max_dx, max_dy)`
/// returns `None`. Both bitmaps must be at most 32 pixels wide, with profiles
/// built from them.
///
/// For each offset the search pairs row `r` of `a`, shifted by `dx` with zero
/// fill inside one word, with row `r - dy` of `b` over the rows both have.
/// Its error is the XOR popcount of those words, which is at least
/// - the sum over row pairs of the ink-count difference, where `a` loses at
///   most `|dx|` pixels per row to the shift, and
/// - the sum over the 32 word columns of the difference between the column
///   counts of `a` (moved by `dx`) and of `b` over the paired rows.
pub(crate) fn narrow_alignment_impossible(
    a: &BitImage,
    a_profile: &NarrowProfile,
    b: &BitImage,
    b_profile: &NarrowProfile,
    max_err: u32,
    max_dx: i32,
    max_dy: i32,
) -> bool {
    debug_assert!(a.width <= 32 && b.width <= 32);
    debug_assert!((0..=1).contains(&max_dx) && (0..=1).contains(&max_dy));
    for dy in -max_dy..=max_dy {
        let top = dy.max(0);
        let bottom = (dy + b.height as i32).min(a.height as i32);
        if top >= bottom {
            continue;
        }
        let (a_start, a_end) = (top as usize, bottom as usize);
        let (b_start, b_end) = ((top - dy) as usize, (bottom - dy) as usize);
        let (mut b_gain, mut a_gain, mut a_gain_after_loss) = (0u32, 0u32, 0u32);
        for (&a_ink, &b_ink) in a_profile.rows[a_start..a_end]
            .iter()
            .zip(&b_profile.rows[b_start..b_end])
        {
            b_gain += u32::from(b_ink.saturating_sub(a_ink));
            a_gain += u32::from(a_ink.saturating_sub(b_ink));
            a_gain_after_loss += u32::from(a_ink.saturating_sub(b_ink).saturating_sub(1));
        }
        let row_bound = [b_gain + a_gain, b_gain + a_gain_after_loss];
        if row_bound[max_dx as usize] > max_err {
            continue;
        }
        let a_columns = columns_over(a_profile, a, a_start, a_end);
        let b_columns = columns_over(b_profile, b, b_start, b_end);
        for dx in -max_dx..=max_dx {
            if row_bound[dx.unsigned_abs() as usize] > max_err {
                continue;
            }
            let start = (1 + dx) as usize;
            let column_bound: u32 = a_columns[start..start + 32]
                .iter()
                .zip(&b_columns[1..33])
                .map(|(&a_ink, &b_ink)| u32::from(a_ink.abs_diff(b_ink)))
                .sum();
            if column_bound <= max_err {
                return false;
            }
        }
    }
    true
}

#[inline(always)]
fn row_kernel_noshift(
    a_row: &[u32],
    b_row: &[u32],
    cols_words: usize,
    word_shift: isize,
    mut err: u32,
    best_err: u32,
    max_err: u32,
) -> (u32, bool) {
    if cols_words >= 8 && word_shift >= 0 {
        let result =
            xor_popcnt_u32_rows(a_row, b_row, word_shift, cols_words, err, best_err, max_err);
        return (result.err, result.broke);
    }

    for w in 0..cols_words {
        let a_idx = w as isize + word_shift;
        let aw = load_word_or_zero(a_row, a_idx);
        let bw = b_row[w];
        err = err.saturating_add((aw ^ bw).count_ones());
        if err >= best_err || err > max_err {
            return (err, true);
        }
    }
    (err, false)
}

#[inline(always)]
fn row_kernel_shift(
    a_row: &[u32],
    b_row: &[u32],
    cols_words: usize,
    word_shift: isize,
    bit_shift: u32,
    mut err: u32,
    best_err: u32,
    max_err: u32,
) -> (u32, bool) {
    if cols_words >= 8 && word_shift >= 0 {
        let result = shift_xor_popcnt_u32_rows(
            a_row, b_row, word_shift, bit_shift, cols_words, err, best_err, max_err,
        );
        return (result.err, result.broke);
    }

    let rshift = 32 - bit_shift;
    for w in 0..cols_words {
        let a_idx = w as isize + word_shift;
        let aw = load_word_or_zero(a_row, a_idx);
        let aw_next = load_word_or_zero(a_row, a_idx + 1);
        let aligned_a = (aw << bit_shift) | (aw_next >> rshift);
        let bw = b_row[w];
        err = err.saturating_add((aligned_a ^ bw).count_ones());
        if err >= best_err || err > max_err {
            return (err, true);
        }
    }
    (err, false)
}

fn load_word_or_zero(row: &[u32], idx: isize) -> u32 {
    if idx < 0 {
        0
    } else {
        row.get(idx as usize).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_count_bounds_never_reject_a_found_alignment() {
        let mut state = 0x243f_6a88_85a3_08d3u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut rejected = 0usize;
        let mut found = 0usize;
        for case in 0..3000u64 {
            let wa = 1 + (next() % 32) as usize;
            let ha = 1 + (next() % 36) as usize;
            let wb = (wa as i64 + (next() % 5) as i64 - 2).clamp(1, 32) as usize;
            let hb = (ha as i64 + (next() % 5) as i64 - 2).max(1) as usize;
            let density = 10 + next() % 70;
            let mut a = BitImage::new(wa as u32, ha as u32).unwrap();
            let mut b = BitImage::new(wb as u32, hb as u32).unwrap();
            let flips = next() % 6;
            for y in 0..ha.max(hb) {
                for x in 0..wa.max(wb) {
                    let ink = next() % 100 < density;
                    a.set_usize(x, y, ink);
                    b.set_usize(x, y, ink ^ (next() % 97 < flips));
                }
            }
            let (a_profile, b_profile) = (
                NarrowProfile::new(&a).unwrap(),
                NarrowProfile::new(&b).unwrap(),
            );
            for max_err in [0, 1, 2, 4, 8, 12, 20] {
                for (max_dx, max_dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let impossible = narrow_alignment_impossible(
                        &a, &a_profile, &b, &b_profile, max_err, max_dx, max_dy,
                    );
                    let result = Comparator::default()
                        .compare_for_refine_family(&a, &b, max_err, max_dx, max_dy);
                    assert!(
                        !(impossible && result.is_some()),
                        "case {case}: bound rejected an alignment {result:?}"
                    );
                    rejected += usize::from(impossible);
                    found += usize::from(result.is_some());
                }
            }
        }
        assert!(rejected > 0 && found > 0);
    }

    #[test]
    fn narrow_search_matches_general_search() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut random = |width: usize, height: usize, density: u64| {
            let mut image = BitImage::new(width as u32, height as u32).unwrap();
            for y in 0..height {
                for x in 0..width {
                    if next() % 100 < density {
                        image.set_usize(x, y, true);
                    }
                }
            }
            image
        };
        let mut checked = 0usize;
        for case in 0..1500u64 {
            let wa = 1 + (case * 7 % 64) as usize;
            let ha = 1 + (case * 13 % 40) as usize;
            let wb = (wa as i64 + (case % 11) as i64 - 5).clamp(1, 64) as usize;
            let hb = (ha as i64 + (case % 7) as i64 - 3).max(1) as usize;
            let a = random(wa, ha, 15 + case % 60);
            // Half the pairs are a noisy copy, so that matches are frequent.
            let b = if case % 2 == 0 {
                let mut b = BitImage::new(wb as u32, hb as u32).unwrap();
                for y in 0..hb.min(ha) {
                    for x in 0..wb.min(wa) {
                        b.set_usize(
                            x,
                            y,
                            a.get_usize(x, y)
                                ^ (case.wrapping_mul(x as u64 + 3 * y as u64) % 23 == 0),
                        );
                    }
                }
                b
            } else {
                random(wb, hb, 15 + case % 60)
            };
            for max_err in [0, 1, 3, 8, 20, 200] {
                for max_dx in 0..=5 {
                    for max_dy in [0, 1, 2, 5] {
                        assert_eq!(
                            Comparator::default()
                                .best_alignment_by_xor_limited(&a, &b, max_err, max_dx, max_dy),
                            best_alignment_words(&a, &b, max_err, max_dx, max_dy),
                            "case {case} {wa}x{ha} vs {wb}x{hb} err={max_err} dx={max_dx} dy={max_dy}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 0);
    }
}
