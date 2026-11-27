//! Word-parallel shape comparison for the glyph dictionary.
//!
//! Two glyph bitmaps aligned by the comparator are laid into one frame as
//! rows of 64-bit words (bit `x` of a row is pixel `x`, least significant
//! bit first), a pixel of padding all round. Every measure the matching
//! gates use is then a few word operations per row: the differing pixels
//! are the XOR, a two-pixel-thick difference is the AND of a row with the
//! next and of that with itself shifted, and "ink within a pixel" is the
//! OR of a row with its shifts and its neighbours. Each is a popcount.

use jbig2enc_rust::jbig2sym::BitImage;

/// A bitmap as rows of 64-bit words, least significant bit first.
pub struct BitRows {
    /// Words per row.
    pub words: usize,
    /// Frame size in pixels.
    pub width: usize,
    pub height: usize,
    pub bits: Vec<u64>,
}

impl BitRows {
    /// A frame of `width × height` pixels, blank.
    pub fn blank(width: usize, height: usize) -> Self {
        let words = width.div_ceil(64).max(1);
        Self {
            words,
            width,
            height,
            bits: vec![0; words * height],
        }
    }

    /// `image` drawn with its top-left at `(ox, oy)` in a `width × height`
    /// frame; pixels outside the frame are dropped.
    pub fn place(image: &BitImage, ox: i32, oy: i32, width: usize, height: usize) -> Self {
        let mut rows = Self::blank(width, height);
        rows.draw(image, ox, oy);
        rows
    }

    /// The image itself, one pixel of padding all round.
    pub fn padded(image: &BitImage) -> Self {
        Self::place(image, 1, 1, image.width + 2, image.height + 2)
    }

    /// OR `image` in with its top-left at `(ox, oy)`.
    pub fn draw(&mut self, image: &BitImage, ox: i32, oy: i32) {
        let stride = image.width.div_ceil(32);
        let src = image.packed_words();
        let words = self.words;
        let width_bits = words * 64;
        // Pixels past the frame width would sit in the last word's spare bits.
        let spare = width_bits - self.width;
        for y in 0..image.height {
            let fy = y as i32 + oy;
            if fy < 0 || fy as usize >= self.height {
                continue;
            }
            let row = &mut self.bits[fy as usize * words..(fy as usize + 1) * words];
            for (i, &word) in src[y * stride..(y + 1) * stride].iter().enumerate() {
                if word == 0 {
                    continue;
                }
                // The source packs pixel `i·32 + k` at bit `31 − k`; reverse
                // it so bit `k` is pixel `k`, then shift into place.
                let v = word.reverse_bits() as u64;
                let p = ox as i64 + (i as i64) * 32;
                if p >= width_bits as i64 || p <= -32 {
                    continue;
                }
                if p < 0 {
                    row[0] |= v >> (-p) as u32;
                    continue;
                }
                let (w, s) = ((p / 64) as usize, (p % 64) as u32);
                row[w] |= v << s;
                if s > 32 && w + 1 < words {
                    row[w + 1] |= v >> (64 - s);
                }
            }
            if spare > 0 {
                let last = row.len() - 1;
                row[last] &= u64::MAX >> spare;
            }
        }
    }

    /// The row at `y`.
    #[inline]
    pub fn row(&self, y: usize) -> &[u64] {
        &self.bits[y * self.words..(y + 1) * self.words]
    }

    /// Ink pixels.
    pub fn count(&self) -> u32 {
        self.bits.iter().map(|w| w.count_ones()).sum()
    }

    /// Ink pixels whose four neighbours are all ink.
    pub fn interior_count(&self) -> u32 {
        let mut total = 0u32;
        let n = self.words;
        for y in 1..self.height.saturating_sub(1) {
            let row = self.row(y);
            let above = self.row(y - 1);
            let below = self.row(y + 1);
            for k in 0..n {
                let left = shl1(row, k);
                let right = shr1(row, k);
                total += (row[k] & left & right & above[k] & below[k]).count_ones();
            }
        }
        total
    }
}

/// Row word `k` shifted one pixel right (towards higher x), the carry
/// coming from the word before.
#[inline]
fn shl1(row: &[u64], k: usize) -> u64 {
    (row[k] << 1) | if k > 0 { row[k - 1] >> 63 } else { 0 }
}

/// Row word `k` shifted one pixel left (towards lower x), the carry coming
/// from the word after.
#[inline]
fn shr1(row: &[u64], k: usize) -> u64 {
    (row[k] >> 1)
        | if k + 1 < row.len() {
            row[k + 1] << 63
        } else {
            0
        }
}

/// A row with its one-pixel horizontal shifts OR'd in.
fn spread(row: &[u64], out: &mut [u64]) {
    for k in 0..row.len() {
        out[k] = row[k] | shl1(row, k) | shr1(row, k);
    }
}

/// How two aligned shapes differ; see the gates in `glyphfont`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// Pixels that are ink in `a` and paper in `b`.
    pub only_a: u32,
    /// Pixels that are ink in `b` and paper in `a`.
    pub only_b: u32,
    /// Pixels that are ink in exactly one of the two (`only_a + only_b`).
    pub total: u32,
    /// 2×2 blocks of differing pixels in `a ^ b`.
    pub thick: u32,
    /// 2×2 blocks of pixels present only in `a` (`a & !b`).
    pub thick_a: u32,
    /// 2×2 blocks of pixels present only in `b` (`b & !a`).
    pub thick_b: u32,
    /// Differing pixels in `a` with no ink of `b` within one pixel.
    pub far_a: u32,
    /// Differing pixels in `b` with no ink of `a` within one pixel.
    pub far_b: u32,
    /// Differing pixels with no ink of the other shape within one pixel (`far_a + far_b`).
    pub far: u32,
    /// The largest 8-connected group of `far` pixels.
    pub far_blob: u32,
}

/// Compare `a` against `b` placed at `(dx, dy)` in `a`'s frame. Stops as
/// soon as the difference exceeds `total_limit`, or the far pixels exceed
/// `far_limit`, returning `None`, so a hopeless candidate costs one XOR
/// pass. The far measures are only computed when `far_limit` is given.
pub fn diff(
    a: &BitImage,
    b: &BitImage,
    dx: i32,
    dy: i32,
    total_limit: u32,
    far_limit: Option<u32>,
) -> Option<Diff> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("popcnt") {
            // SAFETY: the instruction set the kernel is compiled for was
            // just detected on this CPU.
            return unsafe { diff_popcnt(a, b, dx, dy, total_limit, far_limit) };
        }
    }
    diff_impl(a, b, dx, dy, total_limit, far_limit)
}

/// [`diff_impl`] compiled with hardware bit counting.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "popcnt")]
unsafe fn diff_popcnt(
    a: &BitImage,
    b: &BitImage,
    dx: i32,
    dy: i32,
    total_limit: u32,
    far_limit: Option<u32>,
) -> Option<Diff> {
    diff_impl(a, b, dx, dy, total_limit, far_limit)
}

#[inline(always)]
fn diff_impl(
    a: &BitImage,
    b: &BitImage,
    dx: i32,
    dy: i32,
    total_limit: u32,
    far_limit: Option<u32>,
) -> Option<Diff> {
    let min_x = 0.min(dx);
    let min_y = 0.min(dy);
    let max_x = (a.width as i32).max(dx + b.width as i32);
    let max_y = (a.height as i32).max(dy + b.height as i32);
    // One pixel of padding all round, so shifts never lose ink.
    let w = (max_x - min_x) as usize + 2;
    let h = (max_y - min_y) as usize + 2;
    let ra = BitRows::place(a, 1 - min_x, 1 - min_y, w, h);
    let rb = BitRows::place(b, 1 + dx - min_x, 1 + dy - min_y, w, h);
    let n = ra.words;

    let mut xor = vec![0u64; n * h];
    let mut sub_a = vec![0u64; n * h];
    let mut sub_b = vec![0u64; n * h];
    let mut only_a = 0u32;
    let mut only_b = 0u32;
    for (((x, sa), sb), (&pa, &pb)) in xor
        .iter_mut()
        .zip(&mut sub_a)
        .zip(&mut sub_b)
        .zip(ra.bits.iter().zip(&rb.bits))
    {
        let a_only = pa & !pb;
        let b_only = pb & !pa;
        *sa = a_only;
        *sb = b_only;
        *x = pa ^ pb;
        only_a += a_only.count_ones();
        only_b += b_only.count_ones();
    }
    let total = only_a + only_b;
    if total > total_limit {
        return None;
    }

    let thick = count_thick(&xor, n, h);
    let thick_a = if only_a >= 4 {
        count_thick(&sub_a, n, h)
    } else {
        0
    };
    let thick_b = if only_b >= 4 {
        count_thick(&sub_b, n, h)
    } else {
        0
    };
    let Some(far_limit) = far_limit else {
        return Some(Diff {
            only_a,
            only_b,
            total,
            thick,
            thick_a,
            thick_b,
            far_a: 0,
            far_b: 0,
            far: 0,
            far_blob: 0,
        });
    };

    // Each shape spread by a pixel (horizontally, then with the rows
    // above and below); a differing pixel is far when the other shape's
    // spread does not cover it.
    let mut spread_a = vec![0u64; n * h];
    let mut spread_b = vec![0u64; n * h];
    for y in 0..h {
        spread(ra.row(y), &mut spread_a[y * n..(y + 1) * n]);
        spread(rb.row(y), &mut spread_b[y * n..(y + 1) * n]);
    }
    let mut far_mask = vec![0u64; n * h];
    let mut far_a = 0u32;
    let mut far_b = 0u32;
    for y in 0..h {
        for k in 0..n {
            let i = y * n + k;
            let mut near_a = spread_a[i];
            let mut near_b = spread_b[i];
            if y > 0 {
                near_a |= spread_a[i - n];
                near_b |= spread_b[i - n];
            }
            if y + 1 < h {
                near_a |= spread_a[i + n];
                near_b |= spread_b[i + n];
            }
            let fa = ra.bits[i] & !near_b;
            let fb = rb.bits[i] & !near_a;
            far_a += fa.count_ones();
            far_b += fb.count_ones();
            far_mask[i] = fa | fb;
        }
        if far_a + far_b > far_limit {
            return None;
        }
    }
    let far = far_a + far_b;
    let far_blob = if far == 0 {
        0
    } else {
        largest_blob(&far_mask, n, h)
    };
    Some(Diff {
        only_a,
        only_b,
        total,
        thick,
        thick_a,
        thick_b,
        far_a,
        far_b,
        far,
        far_blob,
    })
}

/// 2×2 blocks of set bits in a plane of `h` rows of `n` words.
#[inline(always)]
fn count_thick(plane: &[u64], n: usize, h: usize) -> u32 {
    let mut count = 0u32;
    for y in 0..h - 1 {
        let row = &plane[y * n..(y + 1) * n];
        let next = &plane[(y + 1) * n..(y + 2) * n];
        for k in 0..n {
            let t = row[k] & next[k];
            let pairs = t & (t >> 1);
            let straddle = if k + 1 < n {
                (t >> 63) & (row[k + 1] & next[k + 1])
            } else {
                0
            };
            count += pairs.count_ones() + straddle.count_ones();
        }
    }
    count
}

/// The largest 8-connected group of set bits in `h` rows of `n` words.
fn largest_blob(mask: &[u64], n: usize, h: usize) -> u32 {
    let mut left = mask.to_vec();
    let mut largest = 0u32;
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for y in 0..h {
        for k in 0..n {
            while left[y * n + k] != 0 {
                let x = k * 64 + left[y * n + k].trailing_zeros() as usize;
                take_bit(&mut left, n, x, y);
                stack.push((x, y));
                largest = largest.max(drain_blob(&mut left, &mut stack, n, h));
            }
        }
    }
    largest
}

/// Take out of `left` every set bit 8-connected to the pixels on `stack`,
/// which leaves it empty, and return the size of the group (the pixels
/// that were on the stack included).
#[inline]
fn drain_blob(left: &mut [u64], stack: &mut Vec<(usize, usize)>, n: usize, h: usize) -> u32 {
    let mut size = 0u32;
    while let Some((x, y)) = stack.pop() {
        size += 1;
        push_taken_neighbours(left, stack, (x, y), n, h);
    }
    size
}

/// Take the set bits around `(x, y)` out of `left` and push them on `stack`.
#[inline]
fn push_taken_neighbours(
    left: &mut [u64],
    stack: &mut Vec<(usize, usize)>,
    (x, y): (usize, usize),
    n: usize,
    h: usize,
) {
    for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
        for nx in x.saturating_sub(1)..=(x + 1).min(n * 64 - 1) {
            if take_bit(left, n, nx, ny) {
                stack.push((nx, ny));
            }
        }
    }
}

/// Clear bit `(x, y)` of `left` (rows of `n` words); whether it was set.
#[inline]
fn take_bit(left: &mut [u64], n: usize, x: usize, y: usize) -> bool {
    let i = y * n + x / 64;
    let bit = 1u64 << (x % 64);
    if left[i] & bit != 0 {
        left[i] &= !bit;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(rows: &[&str]) -> BitImage {
        let w = rows[0].len();
        let mut b = BitImage::new(w as u32, rows.len() as u32).unwrap();
        for (y, r) in rows.iter().enumerate() {
            for (x, c) in r.chars().enumerate() {
                if c == '#' {
                    b.set_usize(x, y, true);
                }
            }
        }
        b
    }

    /// The per-pixel reference the word-parallel kernel replaced.
    fn reference(a: &BitImage, b: &BitImage, dx: i32, dy: i32) -> Diff {
        let min_x = 0.min(dx);
        let min_y = 0.min(dy);
        let max_x = (a.width as i32).max(dx + b.width as i32);
        let max_y = (a.height as i32).max(dy + b.height as i32);
        let w = (max_x - min_x) as usize;
        let h = (max_y - min_y) as usize;
        let on = |img: &BitImage, x: i32, y: i32| -> bool {
            x >= 0
                && y >= 0
                && (x as usize) < img.width
                && (y as usize) < img.height
                && img.get_usize(x as usize, y as usize)
        };
        let mut xor = vec![false; w * h];
        let mut sub_a = vec![false; w * h];
        let mut sub_b = vec![false; w * h];
        let mut only_a = 0;
        let mut only_b = 0;
        for gy in min_y..max_y {
            for gx in min_x..max_x {
                let pa = on(a, gx, gy);
                let pb = on(b, gx - dx, gy - dy);
                let idx = ((gy - min_y) as usize) * w + (gx - min_x) as usize;
                if pa && !pb {
                    sub_a[idx] = true;
                    only_a += 1;
                } else if pb && !pa {
                    sub_b[idx] = true;
                    only_b += 1;
                }
                if pa != pb {
                    xor[idx] = true;
                }
            }
        }
        let total = only_a + only_b;
        let thick = count_2x2(&xor, w, h);
        let thick_a = count_2x2(&sub_a, w, h);
        let thick_b = count_2x2(&sub_b, w, h);
        let near = |img: &BitImage, x: i32, y: i32| -> bool {
            (-1..=1).any(|ny| (-1..=1).any(|nx| on(img, x + nx, y + ny)))
        };
        let mut far_mask = vec![false; w * h];
        let mut far_a = 0;
        let mut far_b = 0;
        for gy in min_y..max_y {
            for gx in min_x..max_x {
                let i = ((gy - min_y) as usize) * w + (gx - min_x) as usize;
                if !xor[i] {
                    continue;
                }
                // A differing pixel of one shape is far when the other
                // shape has no ink within a pixel of it.
                let (near_other, far_count) = if on(a, gx, gy) {
                    (near(b, gx - dx, gy - dy), &mut far_a)
                } else {
                    (near(a, gx, gy), &mut far_b)
                };
                if !near_other {
                    far_mask[i] = true;
                    *far_count += 1;
                }
            }
        }
        let far = far_a + far_b;
        let far_blob = largest_blob_per_pixel(&far_mask, w, h);
        Diff {
            only_a,
            only_b,
            total,
            thick,
            thick_a,
            thick_b,
            far_a,
            far_b,
            far,
            far_blob,
        }
    }

    /// 2x2 blocks of set pixels in a `w` x `h` plane, per pixel.
    fn count_2x2(plane: &[bool], w: usize, h: usize) -> u32 {
        let mut count = 0;
        for y in 0..h.saturating_sub(1) {
            for x in 0..w.saturating_sub(1) {
                if plane[y * w + x]
                    && plane[y * w + x + 1]
                    && plane[(y + 1) * w + x]
                    && plane[(y + 1) * w + x + 1]
                {
                    count += 1;
                }
            }
        }
        count
    }

    /// Largest 8-connected blob of a `w` x `h` mask, per pixel.
    fn largest_blob_per_pixel(mask: &[bool], w: usize, h: usize) -> u32 {
        let mut seen = vec![false; w * h];
        let mut largest = 0u32;
        for start in 0..w * h {
            if !mask[start] || seen[start] {
                continue;
            }
            seen[start] = true;
            let mut stack = vec![start];
            let mut size = 0;
            while let Some(i) = stack.pop() {
                size += 1;
                push_unseen_neighbours(mask, &mut seen, &mut stack, i, (w, h));
            }
            largest = largest.max(size);
        }
        largest
    }

    /// Mark the unseen set neighbours of pixel `i` seen and push them.
    fn push_unseen_neighbours(
        mask: &[bool],
        seen: &mut [bool],
        stack: &mut Vec<usize>,
        i: usize,
        (w, h): (usize, usize),
    ) {
        let (x, y) = ((i % w) as i32, (i / w) as i32);
        for ny in -1..=1 {
            for nx in -1..=1 {
                let (qx, qy) = (x + nx, y + ny);
                if qx < 0 || qy < 0 || qx >= w as i32 || qy >= h as i32 {
                    continue;
                }
                let j = qy as usize * w + qx as usize;
                if mask[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
    }

    /// Ink pixels whose four neighbours are all ink, per pixel.
    fn interior_per_pixel(img: &BitImage) -> u32 {
        let mut count = 0u32;
        for y in 1..img.height - 1 {
            for x in 1..img.width - 1 {
                if img.get_usize(x, y)
                    && img.get_usize(x - 1, y)
                    && img.get_usize(x + 1, y)
                    && img.get_usize(x, y - 1)
                    && img.get_usize(x, y + 1)
                {
                    count += 1;
                }
            }
        }
        count
    }

    /// A deterministic pseudo-random bitmap.
    fn noise(w: usize, h: usize, seed: u64, density: u64) -> BitImage {
        let mut b = BitImage::new(w as u32, h as u32).unwrap();
        let mut s = seed;
        for y in 0..h {
            for x in 0..w {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                if s % 100 < density {
                    b.set_usize(x, y, true);
                }
            }
        }
        b
    }

    #[test]
    fn placement_reproduces_the_bitmap() {
        let img = noise(70, 9, 3, 50);
        let rows = BitRows::padded(&img);
        assert_eq!(rows.words, 2);
        for y in 0..img.height {
            for x in 0..img.width {
                let r = rows.row(y + 1);
                let bit = (r[(x + 1) / 64] >> ((x + 1) % 64)) & 1 == 1;
                assert_eq!(bit, img.get_usize(x, y), "({x},{y})");
            }
        }
        assert_eq!(rows.count() as usize, img.count_ones());
        // Nothing outside the frame.
        let clipped = BitRows::place(&img, -5, -2, 20, 4);
        let mut expected = 0;
        for y in 2..min(img.height, 6) {
            for x in 5..min(img.width, 25) {
                expected += img.get_usize(x, y) as u32;
            }
        }
        assert_eq!(clipped.count(), expected);
    }

    fn min(a: usize, b: usize) -> usize {
        a.min(b)
    }

    #[test]
    fn diff_agrees_with_the_per_pixel_reference() {
        let cases: Vec<(BitImage, BitImage, i32, i32)> = vec![
            (noise(20, 30, 1, 40), noise(21, 29, 2, 40), 0, 0),
            (noise(20, 30, 1, 40), noise(20, 30, 1, 40), 0, 0),
            (noise(20, 30, 5, 30), noise(18, 31, 6, 30), -2, 1),
            (noise(20, 30, 5, 30), noise(18, 31, 6, 30), 2, -2),
            (noise(70, 12, 9, 35), noise(66, 14, 10, 35), 1, -1),
            (noise(130, 5, 11, 45), noise(129, 6, 12, 45), -1, 2),
            (noise(64, 8, 13, 50), noise(64, 8, 14, 50), 0, 0),
            (noise(3, 40, 15, 60), noise(4, 40, 16, 60), 1, 0),
        ];
        for (a, b, dx, dy) in &cases {
            let expected = reference(a, b, *dx, *dy);
            let got = diff(a, b, *dx, *dy, u32::MAX, Some(u32::MAX)).unwrap();
            assert_eq!(
                got, expected,
                "{}x{} vs {}x{} at ({dx},{dy})",
                a.width, a.height, b.width, b.height
            );
            let strict = diff(a, b, *dx, *dy, u32::MAX, None).unwrap();
            assert_eq!(
                (strict.total, strict.thick),
                (expected.total, expected.thick)
            );
            if expected.total > 0 {
                assert_eq!(diff(a, b, *dx, *dy, expected.total - 1, None), None);
                if expected.far > 0 {
                    assert_eq!(diff(a, b, *dx, *dy, u32::MAX, Some(expected.far - 1)), None);
                }
            }
        }
    }

    #[test]
    fn interior_matches_the_per_pixel_count() {
        for (seed, density) in [(1, 60), (2, 80), (3, 95)] {
            let img = noise(90, 40, seed, density);
            let expected = interior_per_pixel(&img);
            assert_eq!(BitRows::padded(&img).interior_count(), expected);
        }
    }

    #[test]
    fn a_crossbar_is_a_thick_far_blob() {
        let e = bitmap(&[
            ".####.", "#....#", "#....#", "######", "#.....", "#....#", ".####.",
        ]);
        let c = bitmap(&[
            ".####.", "#....#", "#.....", "#.....", "#.....", "#....#", ".####.",
        ]);
        let d = diff(&e, &c, 0, 0, u32::MAX, Some(u32::MAX)).unwrap();
        // The bar, and the e's right stem where the c is open.
        assert_eq!(d.total, 6);
        assert_eq!(d.thick, 0);
        // The bar is far from the c except where it meets the left stem.
        assert_eq!(d.far, 4);
        assert_eq!(d.far_blob, 4);
    }
}
