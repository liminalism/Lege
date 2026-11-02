use crate::jbig2comparator::Comparator;
use crate::jbig2sym::BitImage;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SymbolSignature {
    pub black: u32,
    pub area: u32,
    pub left_col: u16,
    pub right_col: u16,
    pub top_row: u16,
    pub bottom_row: u16,
    pub cx_times_256: u16,
    pub cy_times_256: u16,
    pub left_mass: u16,
    pub right_mass: u16,
    pub top_mass: u16,
    pub bottom_mass: u16,
    pub hole_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FamilyBucketKey {
    pub w_bin: u16,
    pub h_bin: u16,
    pub density_bin: u8,
    pub aspect_bin: u8,
    pub centroid_y_bin: u8,
    pub lr_balance_bin: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolScaleProfile {
    pub ref_width: usize,
    pub ref_height: usize,
    pub ref_black: u32,
}

pub fn compute_symbol_signature(img: &BitImage) -> SymbolSignature {
    with_hole_count(compute_symbol_signature_without_holes(img), img)
}

/// Completes a signature from [`compute_symbol_signature_without_holes`] for
/// the same bitmap with its enclosed-background count.
pub(crate) fn with_hole_count(mut signature: SymbolSignature, img: &BitImage) -> SymbolSignature {
    // Hole counting is one of the most expensive signature features. Skip it for
    // symbols that are too small or too sparse to plausibly contain a useful
    // enclosed counter, and keep the full scan for the larger text glyphs where
    // it helps separate classes like a/e/o/b.
    signature.hole_count = if img.width < 6 || img.height < 6 || signature.black < 12 {
        0
    } else {
        count_enclosed_white_components(img).min(u8::MAX as usize) as u8
    };
    signature
}

/// Every signature field except `hole_count`, which is left zero. Only
/// [`family_signatures_are_compatible`] reads the hole count, so matching that
/// does not use it can skip the background scan.
pub(crate) fn compute_symbol_signature_without_holes(img: &BitImage) -> SymbolSignature {
    let mut black = 0u32;
    let mut left_col = img.width;
    let mut right_col = 0usize;
    let mut top_row = img.height;
    let mut bottom_row = 0usize;
    let mut sum_x = 0u32;
    let mut sum_y = 0u32;
    let mut left_mass = 0u32;
    let mut right_mass = 0u32;
    let mut top_mass = 0u32;
    let mut bottom_mass = 0u32;
    let mid_x = img.width / 2;
    let mid_y = img.height / 2;
    let packed = img.packed_words();
    let words_per_row = (img.width + 31) >> 5;
    let tail_bits = img.width & 31;
    let tail_mask = if tail_bits == 0 {
        u32::MAX
    } else {
        u32::MAX << (32 - tail_bits)
    };

    for y in 0..img.height {
        let row = &packed[y * words_per_row..(y + 1) * words_per_row];
        for (word_idx, &row_word) in row.iter().enumerate() {
            let mut word = row_word;
            if tail_bits != 0 && word_idx + 1 == words_per_row {
                word &= tail_mask;
            }
            while word != 0 {
                let bit = word.leading_zeros() as usize;
                let x = word_idx * 32 + bit;
                black += 1;
                left_col = left_col.min(x);
                right_col = right_col.max(x);
                top_row = top_row.min(y);
                bottom_row = bottom_row.max(y);
                sum_x += x as u32;
                sum_y += y as u32;
                if x < mid_x {
                    left_mass += 1;
                } else {
                    right_mass += 1;
                }
                if y < mid_y {
                    top_mass += 1;
                } else {
                    bottom_mass += 1;
                }
                word &= !(1u32 << (31 - bit));
            }
        }
    }

    let (cx, cy) = if black == 0 {
        (0, 0)
    } else {
        (
            ((sum_x * 256) / black).min(u16::MAX as u32) as u16,
            ((sum_y * 256) / black).min(u16::MAX as u32) as u16,
        )
    };

    SymbolSignature {
        black,
        area: img.width.saturating_mul(img.height).min(u32::MAX as usize) as u32,
        left_col: left_col.min(u16::MAX as usize) as u16,
        right_col: right_col.min(u16::MAX as usize) as u16,
        top_row: top_row.min(u16::MAX as usize) as u16,
        bottom_row: bottom_row.min(u16::MAX as usize) as u16,
        cx_times_256: cx,
        cy_times_256: cy,
        left_mass: left_mass.min(u16::MAX as u32) as u16,
        right_mass: right_mass.min(u16::MAX as u32) as u16,
        top_mass: top_mass.min(u16::MAX as u32) as u16,
        bottom_mass: bottom_mass.min(u16::MAX as u32) as u16,
        hole_count: 0,
    }
}

fn count_enclosed_white_components(img: &BitImage) -> usize {
    count_white_components(img, true)
}

#[cfg(feature = "symboldict")]
pub(crate) fn count_enclosed_white_components_4(img: &BitImage) -> usize {
    count_white_components(img, false)
}

/// Counts white components that do not touch the bitmap border.
///
/// `diagonals` selects 8-connectivity for the background; otherwise it is
/// 4-connected. The bitmap is processed as horizontal white runs taken from
/// the packed words, joined between adjacent rows with a union-find, so the
/// cost follows the number of runs rather than the number of pixels. The
/// count is identical to a per-pixel flood fill with the same connectivity.
fn count_white_components(img: &BitImage, diagonals: bool) -> usize {
    if img.width < 3 || img.height < 3 {
        return 0;
    }
    let width = img.width;
    let height = img.height;
    let stride = width.div_ceil(32);
    let words = img.packed_words();
    // Adjacent-row runs [a, b) and [c, d) touch when they overlap; with
    // diagonal connectivity a one-pixel corner contact also joins them.
    let slack = usize::from(diagonals);

    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut parent: Vec<u32> = Vec::new();
    let mut border: Vec<bool> = Vec::new();
    let mut previous = 0..0;
    for y in 0..height {
        let row = &words[y * stride..(y + 1) * stride];
        let first = runs.len();
        let mut x = 0usize;
        while x < width {
            let start = next_pixel_of_color(row, x, width, false);
            if start >= width {
                break;
            }
            let end = next_pixel_of_color(row, start, width, true);
            let index = runs.len() as u32;
            runs.push((start as u32, end as u32));
            parent.push(index);
            border.push(y == 0 || y + 1 == height || start == 0 || end == width);
            x = end;
        }
        let current = first..runs.len();

        let (mut i, mut j) = (previous.start, current.start);
        while i < previous.end && j < current.end {
            let (a_start, a_end) = runs[i];
            let (b_start, b_end) = runs[j];
            if (a_start as usize) < b_end as usize + slack
                && (b_start as usize) < a_end as usize + slack
            {
                let a = find_root(&mut parent, i as u32);
                let b = find_root(&mut parent, j as u32);
                if a != b {
                    let (keep, drop) = if a < b { (a, b) } else { (b, a) };
                    parent[drop as usize] = keep;
                    border[keep as usize] |= border[drop as usize];
                }
            }
            if a_end <= b_end {
                i += 1;
            } else {
                j += 1;
            }
        }
        previous = current;
    }

    let mut holes = 0usize;
    for index in 0..parent.len() {
        if parent[index] as usize == index && !border[index] {
            holes += 1;
        }
    }
    holes
}

/// First pixel at or after `x` whose value is `black`, or `width` if none.
#[inline]
fn next_pixel_of_color(row: &[u32], x: usize, width: usize, black: bool) -> usize {
    let mut word_index = x >> 5;
    let mut word = if black {
        row[word_index]
    } else {
        !row[word_index]
    } & (u32::MAX >> (x & 31));
    loop {
        if word != 0 {
            return (word_index * 32 + word.leading_zeros() as usize).min(width);
        }
        word_index += 1;
        if word_index >= row.len() {
            return width;
        }
        word = if black {
            row[word_index]
        } else {
            !row[word_index]
        };
    }
}

#[inline]
fn find_root(parent: &mut [u32], mut node: u32) -> u32 {
    while parent[node as usize] != node {
        let grandparent = parent[parent[node as usize] as usize];
        parent[node as usize] = grandparent;
        node = grandparent;
    }
    node
}

#[inline]
fn quantize_ratio_u8(num: u32, den: u32, bins: u32) -> u8 {
    if den == 0 || bins == 0 {
        return 0;
    }
    ((num.saturating_mul(bins)) / den).min(bins.saturating_sub(1)) as u8
}

pub fn family_bucket_key_for_symbol(symbol: &BitImage, sig: &SymbolSignature) -> FamilyBucketKey {
    let w = symbol.width as u32;
    let h = symbol.height as u32;
    let area = sig.area.max(1);

    FamilyBucketKey {
        w_bin: ((w + 1) / 2).min(u16::MAX as u32) as u16,
        h_bin: ((h + 1) / 2).min(u16::MAX as u32) as u16,
        density_bin: quantize_ratio_u8(sig.black, area, 16),
        aspect_bin: quantize_ratio_u8(w, h.max(1), 16),
        centroid_y_bin: ((sig.cy_times_256 as u32 * 16) / (h.max(1) * 256)).min(15) as u8,
        lr_balance_bin: quantize_ratio_u8(sig.left_mass as u32, sig.black.max(1), 8),
    }
}

fn weighted_median_usize(values: &mut [(usize, usize)]) -> usize {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable_by_key(|&(value, _)| value);
    let total_weight: usize = values.iter().map(|&(_, weight)| weight.max(1)).sum();
    let target = total_weight.div_ceil(2);
    let mut running = 0usize;
    for &(value, weight) in values.iter() {
        running += weight.max(1);
        if running >= target {
            return value;
        }
    }
    values.last().map(|&(value, _)| value).unwrap_or(0)
}

fn weighted_median_u32(values: &mut [(u32, usize)]) -> u32 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable_by_key(|&(value, _)| value);
    let total_weight: usize = values.iter().map(|&(_, weight)| weight.max(1)).sum();
    let target = total_weight.div_ceil(2);
    let mut running = 0usize;
    for &(value, weight) in values.iter() {
        running += weight.max(1);
        if running >= target {
            return value;
        }
    }
    values.last().map(|&(value, _)| value).unwrap_or(0)
}

pub fn estimate_symbol_scale_profile(
    symbols: &[BitImage],
    signatures: &[SymbolSignature],
    usage: &[usize],
) -> SymbolScaleProfile {
    let mut widths = Vec::with_capacity(symbols.len());
    let mut heights = Vec::with_capacity(symbols.len());
    let mut blacks = Vec::with_capacity(symbols.len());

    for ((symbol, signature), &weight) in symbols.iter().zip(signatures.iter()).zip(usage.iter()) {
        if signature.black < 4 || symbol.width < 2 || symbol.height < 2 {
            continue;
        }
        let weight = weight.max(1);
        widths.push((symbol.width, weight));
        heights.push((symbol.height, weight));
        blacks.push((signature.black, weight));
    }

    SymbolScaleProfile {
        ref_width: weighted_median_usize(&mut widths).max(1),
        ref_height: weighted_median_usize(&mut heights).max(1),
        ref_black: weighted_median_u32(&mut blacks).max(1),
    }
}

pub fn family_bucket_neighbors(key: FamilyBucketKey) -> Vec<FamilyBucketKey> {
    let mut out = Vec::with_capacity(27);
    for dh in -1i32..=1 {
        for dw in -1i32..=1 {
            for dd in -1i32..=1 {
                let candidate = FamilyBucketKey {
                    w_bin: (key.w_bin as i32 + dw).max(0) as u16,
                    h_bin: (key.h_bin as i32 + dh).max(0) as u16,
                    density_bin: (key.density_bin as i32 + dd).clamp(0, 15) as u8,
                    aspect_bin: key.aspect_bin,
                    centroid_y_bin: key.centroid_y_bin,
                    lr_balance_bin: key.lr_balance_bin,
                };
                if !out.contains(&candidate) {
                    out.push(candidate);
                }
            }
        }
    }
    out
}

#[inline]
pub fn for_each_family_bucket_neighbor(key: FamilyBucketKey, mut f: impl FnMut(FamilyBucketKey)) {
    for dh in -1i32..=1 {
        for dw in -1i32..=1 {
            for dd in -1i32..=1 {
                f(FamilyBucketKey {
                    w_bin: (key.w_bin as i32 + dw).max(0) as u16,
                    h_bin: (key.h_bin as i32 + dh).max(0) as u16,
                    density_bin: (key.density_bin as i32 + dd).clamp(0, 15) as u8,
                    aspect_bin: key.aspect_bin,
                    centroid_y_bin: key.centroid_y_bin,
                    lr_balance_bin: key.lr_balance_bin,
                });
            }
        }
    }
}

pub fn family_signatures_are_compatible(
    lhs: SymbolSignature,
    rhs: SymbolSignature,
    lhs_black: usize,
    rhs_black: usize,
) -> bool {
    let area_scale = lhs_black.max(rhs_black).max(4);
    let black_tol = (area_scale / 10).clamp(4, 10) as u32;
    let mass_tol = (area_scale / 8).clamp(4, 12) as u16;

    lhs.black.abs_diff(rhs.black) <= black_tol
        && lhs.left_col.abs_diff(rhs.left_col) <= 2
        && lhs.right_col.abs_diff(rhs.right_col) <= 2
        && lhs.top_row.abs_diff(rhs.top_row) <= 1
        && lhs.bottom_row.abs_diff(rhs.bottom_row) <= 1
        && lhs.cx_times_256.abs_diff(rhs.cx_times_256) <= 96
        && lhs.cy_times_256.abs_diff(rhs.cy_times_256) <= 96
        && lhs.left_mass.abs_diff(rhs.left_mass) <= mass_tol
        && lhs.right_mass.abs_diff(rhs.right_mass) <= mass_tol
        && lhs.top_mass.abs_diff(rhs.top_mass) <= mass_tol
        && lhs.bottom_mass.abs_diff(rhs.bottom_mass) <= mass_tol
        && lhs.hole_count == rhs.hole_count
}

#[inline]
pub fn refine_compare_score(err: u32, dx: i32, dy: i32) -> u32 {
    err.saturating_add(((dx.abs() + dy.abs()) as u32).saturating_mul(2))
}

pub fn family_match_details(
    comparator: &mut Comparator,
    target: &BitImage,
    target_index: usize,
    reference: &BitImage,
    reference_index: usize,
    signatures: &[SymbolSignature],
    black_counts: &[usize],
) -> Option<(u32, i32, i32)> {
    if target.width.abs_diff(reference.width) > 2 || target.height.abs_diff(reference.height) > 2 {
        return None;
    }
    if !family_signatures_are_compatible(
        signatures[target_index],
        signatures[reference_index],
        black_counts[target_index],
        black_counts[reference_index],
    ) {
        return None;
    }

    let area = target
        .width
        .max(reference.width)
        .saturating_mul(target.height.max(reference.height));
    let max_err = ((area as f32 * 0.05).ceil() as u32).clamp(2, 16);
    let result = comparator.compare_for_refine_family(target, reference, max_err, 2, 1)?;
    Some((result.total_err, result.dx, result.dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The per-pixel flood fill this module used before run-based counting.
    fn flood_fill_white_components(img: &BitImage, diagonals: bool) -> usize {
        if img.width < 3 || img.height < 3 {
            return 0;
        }
        let mut visited = vec![false; img.width * img.height];
        let mut stack = Vec::new();
        let mut holes = 0usize;
        for y in 0..img.height {
            for x in 0..img.width {
                let idx = y * img.width + x;
                if visited[idx] || img.get_usize(x, y) {
                    continue;
                }
                visited[idx] = true;
                stack.push((x, y));
                let mut touches_border =
                    x == 0 || y == 0 || x + 1 == img.width || y + 1 == img.height;
                while let Some((cx, cy)) = stack.pop() {
                    for ny in cy.saturating_sub(1)..=(cy + 1).min(img.height - 1) {
                        for nx in cx.saturating_sub(1)..=(cx + 1).min(img.width - 1) {
                            if !diagonals && nx != cx && ny != cy {
                                continue;
                            }
                            let nidx = ny * img.width + nx;
                            if visited[nidx] || img.get_usize(nx, ny) {
                                continue;
                            }
                            visited[nidx] = true;
                            if nx == 0 || ny == 0 || nx + 1 == img.width || ny + 1 == img.height {
                                touches_border = true;
                            }
                            stack.push((nx, ny));
                        }
                    }
                }
                if !touches_border {
                    holes += 1;
                }
            }
        }
        holes
    }

    #[test]
    fn run_based_hole_count_matches_flood_fill() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..3000 {
            let width = 1 + (next() % 70) as usize;
            let height = 1 + (next() % 40) as usize;
            let density = 20 + (next() % 60);
            let mut img = BitImage::new(width as u32, height as u32).unwrap();
            for y in 0..height {
                for x in 0..width {
                    if next() % 100 < density {
                        img.set_usize(x, y, true);
                    }
                }
            }
            for diagonals in [true, false] {
                assert_eq!(
                    count_white_components(&img, diagonals),
                    flood_fill_white_components(&img, diagonals),
                    "case {case} {width}x{height} diagonals={diagonals}"
                );
            }
        }
    }

    #[test]
    fn run_based_hole_count_handles_rings_and_corners() {
        // A ring with a hole, and a diagonal gap that only 8-connectivity crosses.
        let rows = ["#####", "#...#", "#.#.#", "#...#", "#####"];
        let mut ring = BitImage::new(5, 5).unwrap();
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.bytes().enumerate() {
                ring.set_usize(x, y, c == b'#');
            }
        }
        assert_eq!(count_white_components(&ring, true), 1);
        assert_eq!(count_white_components(&ring, false), 1);
        let rows = ["####", "#.##", "##.#", "####"];
        let mut diagonal = BitImage::new(4, 4).unwrap();
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.bytes().enumerate() {
                diagonal.set_usize(x, y, c == b'#');
            }
        }
        assert_eq!(count_white_components(&diagonal, true), 1);
        assert_eq!(count_white_components(&diagonal, false), 2);
    }
}
