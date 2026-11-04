//! The scalar kernel set — the isolated, contiguous arithmetic inner loops
//! (performance advice §7). Orchestration (clipping, paint dispatch, resource
//! lookup) happens *outside* these functions; a kernel receives contiguous
//! buffers and does nothing but math over a span.
//!
//! Each kernel is a simple counted loop with no iterator adapters, closures,
//! per-pixel format branches, or allocation — written to translate cleanly to
//! SIMD later. The public [`KernelSet`] is a table of function pointers chosen
//! **once** (per backend/worker), never per pixel; a SIMD build swaps the
//! table (`KernelSet::sse41()`, `::avx2()`, …) without touching the executor.
//!
//! Pixels are RGBA8 with **premultiplied alpha**, in memory byte order
//! `[R, G, B, A]`. Alpha math uses the shared [`mul_div_255`] primitive.

/// `round(a * b / 255)` for `a, b` in `0..=255`, exact and branchless.
#[inline(always)]
pub fn mul_div_255(a: u16, b: u16) -> u16 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

/// Overwrite `dst` (one span, `len*4` bytes) with an opaque premultiplied
/// pixel. No destination read — the fast path for opaque, fully-covered spans.
pub fn fill_opaque(dst: &mut [u8], premul: [u8; 4]) {
    // One whole-pixel copy per step (not four byte stores) so the loop lowers
    // to a 32-bit splat the compiler vectorizes.
    for px in dst.chunks_exact_mut(4) {
        px.copy_from_slice(&premul);
    }
}

/// Source-over a straight-alpha color `rgb` at constant coverage-alpha `a`
/// over a full span. Reads and writes premultiplied destination.
pub fn blend_const(dst: &mut [u8], rgb: [u8; 3], a: u8) {
    if a == 0 {
        return;
    }
    let ia = 255 - a as u16;
    let sr = mul_div_255(rgb[0] as u16, a as u16) as u8;
    let sg = mul_div_255(rgb[1] as u16, a as u16) as u8;
    let sb = mul_div_255(rgb[2] as u16, a as u16) as u8;
    for px in dst.chunks_exact_mut(4) {
        px[0] = sr + mul_div_255(px[0] as u16, ia) as u8;
        px[1] = sg + mul_div_255(px[1] as u16, ia) as u8;
        px[2] = sb + mul_div_255(px[2] as u16, ia) as u8;
        px[3] = a + mul_div_255(px[3] as u16, ia) as u8;
    }
}

/// Source-over a straight-alpha color `rgb` with per-pixel coverage `cov`,
/// scaled by constant `alpha`. `cov.len()` must equal the pixel count and
/// `dst.len()` must be `cov.len()*4`.
pub fn blend_mask(dst: &mut [u8], cov: &[u8], rgb: [u8; 3], alpha: u8) {
    debug_assert_eq!(dst.len(), cov.len() * 4);
    for (px, &c) in dst.chunks_exact_mut(4).zip(cov) {
        blend_mask_px(px, c, rgb, alpha);
    }
}

/// The per-pixel body of [`blend_mask`], shared with [`blend_mask_rect`] so the
/// two scalar references cannot drift apart.
#[inline(always)]
fn blend_mask_px(px: &mut [u8], c: u8, rgb: [u8; 3], alpha: u8) {
    if c == 0 {
        return;
    }
    let a = if alpha == 255 {
        c as u16
    } else {
        mul_div_255(c as u16, alpha as u16)
    };
    if a == 0 {
        return;
    }
    let ia = 255 - a;
    px[0] = (mul_div_255(rgb[0] as u16, a) + mul_div_255(px[0] as u16, ia)) as u8;
    px[1] = (mul_div_255(rgb[1] as u16, a) + mul_div_255(px[1] as u16, ia)) as u8;
    px[2] = (mul_div_255(rgb[2] as u16, a) + mul_div_255(px[2] as u16, ia)) as u8;
    px[3] = (a + mul_div_255(px[3] as u16, ia)) as u8;
}

/// [`blend_mask`] over a whole coverage rectangle — one glyph bitmap per call
/// instead of one call per row. Row `r` of `height` blends the `width`
/// coverage bytes at `cov[r * cov_stride..]` onto the `width` pixels at
/// `dst[r * dst_stride..]`. Returns the number of non-zero coverage bytes
/// (the glyph blit's `covered_pixels` statistic), counted in the same pass so
/// the caller needs no second walk over the coverage.
///
/// Panics (slice bounds) if either buffer is shorter than the rectangle.
#[allow(clippy::too_many_arguments)]
pub fn blend_mask_rect(
    dst: &mut [u8],
    dst_stride: usize,
    cov: &[u8],
    cov_stride: usize,
    width: usize,
    height: usize,
    rgb: [u8; 3],
    alpha: u8,
) -> u64 {
    let mut covered = 0u64;
    for r in 0..height {
        let d = &mut dst[r * dst_stride..r * dst_stride + width * 4];
        let c = &cov[r * cov_stride..r * cov_stride + width];
        for (px, &c) in d.chunks_exact_mut(4).zip(c) {
            covered += (c != 0) as u64;
            blend_mask_px(px, c, rgb, alpha);
        }
    }
    covered
}

/// [`blend_mask_rect`] with every row run eight pixels at a time (AVX2), the
/// constants set up once per rectangle, and a row remainder of one to seven
/// pixels done as one masked eight-pixel step (`vpmaskmovd` load/store, so the
/// pixels past the row are neither read nor written) instead of a scalar
/// tail — glyph rows are mostly 3–16 pixels, so the remainder is most of the
/// work. The non-zero coverage count comes from a byte compare + `movemask` +
/// `popcnt` on the coverage already in a register.
///
/// Bit-for-bit identical to [`blend_mask_rect`] (same lane arithmetic as
/// [`blend_mask_avx2`]); verified by
/// `tests::blend_mask_rect_avx2_matches_scalar_bit_exact`. Re-probes AVX2 on
/// call (a cached atomic load) and falls back to the scalar kernel without it.
#[cfg(target_arch = "x86_64")]
#[allow(
    unsafe_code,
    clippy::too_many_arguments,
    reason = "dispatches the AVX2 rectangle blend behind a runtime probe; see SAFETY comment"
)]
pub fn blend_mask_rect_avx2(
    dst: &mut [u8],
    dst_stride: usize,
    cov: &[u8],
    cov_stride: usize,
    width: usize,
    height: usize,
    rgb: [u8; 3],
    alpha: u8,
) -> u64 {
    if width == 0 || height == 0 {
        return 0;
    }
    if !(is_x86_feature_detected!("avx2") && is_x86_feature_detected!("popcnt")) {
        return blend_mask_rect(dst, dst_stride, cov, cov_stride, width, height, rgb, alpha);
    }
    // Establish the rectangle's extent with checked slicing, once: the last
    // row ends furthest into each buffer, so every row lies inside these.
    let dst_end = (height - 1)
        .checked_mul(dst_stride)
        .and_then(|v| v.checked_add(width.checked_mul(4)?));
    let cov_end = (height - 1)
        .checked_mul(cov_stride)
        .and_then(|v| v.checked_add(width));
    let (Some(dst_end), Some(cov_end)) = (dst_end, cov_end) else {
        panic!("blend_mask_rect_avx2: rectangle extent overflows usize");
    };
    let dst = &mut dst[..dst_end];
    // `cov` keeps its full length: bytes past `cov_end` may be *read* (never
    // used) by a masked remainder step, which is cheaper than assembling it.
    assert!(
        cov_end <= cov.len(),
        "blend_mask_rect_avx2: coverage too short"
    );
    // SAFETY: AVX2 and POPCNT are present (runtime probe above); `dst` and `cov` cover
    // every row of the rectangle (checked just above), which is the body's
    // precondition.
    unsafe {
        blend_mask_rect_avx2_body(dst, dst_stride, cov, cov_stride, width, height, rgb, alpha)
    }
}

/// The AVX2 body of [`blend_mask_rect_avx2`].
///
/// # Safety
/// * The CPU must support AVX2 and POPCNT.
/// * `width >= 1`, `height >= 1`, and for every row `r < height`:
///   `r * dst_stride + width * 4 <= dst.len()` and
///   `r * cov_stride + width <= cov.len()`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,popcnt")]
#[allow(unsafe_code, clippy::too_many_arguments)]
unsafe fn blend_mask_rect_avx2_body(
    dst: &mut [u8],
    dst_stride: usize,
    cov: &[u8],
    cov_stride: usize,
    width: usize,
    height: usize,
    rgb: [u8; 3],
    alpha: u8,
) -> u64 {
    use core::arch::x86_64::*;

    let (r, g, b) = (rgb[0] as i16, rgb[1] as i16, rgb[2] as i16);
    let src = _mm256_setr_epi16(r, g, b, 255, r, g, b, 255, r, g, b, 255, r, g, b, 255);
    let c128 = _mm256_set1_epi16(128);
    let c255 = _mm256_set1_epi16(255);
    let byte_mask = _mm256_set1_epi16(0x00FF);
    let alpha8 = _mm_set1_epi16(alpha as i16);
    let half8 = _mm_set1_epi16(128);
    let zero = _mm_setzero_si128();
    let idx_lo = _mm256_setr_epi8(
        0, 1, 0, 1, 0, 1, 0, 1, 2, 3, 2, 3, 2, 3, 2, 3, 4, 5, 4, 5, 4, 5, 4, 5, 6, 7, 6, 7, 6, 7,
        6, 7,
    );
    let idx_hi = _mm256_setr_epi8(
        8, 9, 8, 9, 8, 9, 8, 9, 10, 11, 10, 11, 10, 11, 10, 11, 12, 13, 12, 13, 12, 13, 12, 13, 14,
        15, 14, 15, 14, 15, 14, 15,
    );
    let lane_ids = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);

    #[inline(always)]
    fn mul_div_255_x16(a: __m256i, b: __m256i, c128: __m256i) -> __m256i {
        // SAFETY: plain AVX2 register arithmetic, no memory access; only
        // inlined into the AVX2-enabled body below.
        unsafe {
            let t = _mm256_add_epi16(_mm256_mullo_epi16(a, b), c128);
            _mm256_srli_epi16::<8>(_mm256_add_epi16(t, _mm256_srli_epi16::<8>(t)))
        }
    }

    // Eight pixels `px` blended under the eight coverage bytes in the low
    // half of `c`: the exact lane arithmetic of `blend_mask_avx2`'s `blend8`.
    // Register arithmetic only; the intrinsics are safe in this AVX2 context.
    let blend = |c: __m128i, px: __m256i| -> __m256i {
        let mut a = _mm_cvtepu8_epi16(c);
        if alpha != 255 {
            let t = _mm_add_epi16(_mm_mullo_epi16(a, alpha8), half8);
            a = _mm_srli_epi16::<8>(_mm_add_epi16(t, _mm_srli_epi16::<8>(t)));
        }
        let a_both = _mm256_broadcastsi128_si256(a);
        let a_lo = _mm256_shuffle_epi8(a_both, idx_lo);
        let a_hi = _mm256_shuffle_epi8(a_both, idx_hi);
        let d_lo = _mm256_cvtepu8_epi16(_mm256_castsi256_si128(px));
        let d_hi = _mm256_cvtepu8_epi16(_mm256_extracti128_si256::<1>(px));
        let out_lo = _mm256_and_si256(
            _mm256_add_epi16(
                mul_div_255_x16(src, a_lo, c128),
                mul_div_255_x16(d_lo, _mm256_sub_epi16(c255, a_lo), c128),
            ),
            byte_mask,
        );
        let out_hi = _mm256_and_si256(
            _mm256_add_epi16(
                mul_div_255_x16(src, a_hi, c128),
                mul_div_255_x16(d_hi, _mm256_sub_epi16(c255, a_hi), c128),
            ),
            byte_mask,
        );
        _mm256_permute4x64_epi64::<0b11_01_10_00>(_mm256_packus_epi16(out_lo, out_hi))
    };
    // Bit `k` set for each non-zero byte among the low eight of `c`.
    let nonzero8 =
        |c: __m128i| -> u32 { !(_mm_movemask_epi8(_mm_cmpeq_epi8(c, zero)) as u32) & 0xFF };

    let full = width & !7;
    let rem = width - full;
    // Lanes `0..rem` of the masked remainder step.
    let rem_mask = _mm256_cmpgt_epi32(_mm256_set1_epi32(rem as i32), lane_ids);
    let rem_bits = (1u32 << rem) - 1;
    let cov_len = cov.len();
    let mut covered = 0u64;

    for row in 0..height {
        let d = row * dst_stride;
        let s = row * cov_stride;
        let mut row_covered = 0u32;
        let mut i = 0usize;
        while i < full {
            // SAFETY: `i + 8 <= width`, so the 8 coverage bytes at `s + i` and
            // the 32 pixel bytes at `d + 4i` lie inside this row (precondition).
            unsafe {
                let c = _mm_loadl_epi64(cov.as_ptr().add(s + i) as *const __m128i);
                let p = dst.as_mut_ptr().add(d + i * 4) as *mut __m256i;
                _mm256_storeu_si256(p, blend(c, _mm256_loadu_si256(p)));
                row_covered += nonzero8(c).count_ones();
            }
            i += 8;
        }
        if rem != 0 {
            let at = s + full;
            // Coverage for the remainder: one 8-byte load when the slice has
            // eight bytes from `at` (the extra bytes feed only masked-off
            // lanes); at the end of the bitmap, the eight bytes ending at
            // `cov_len` shifted down; a byte loop only for a bitmap under
            // eight bytes.
            let c = if at + 8 <= cov_len {
                // SAFETY: `at + 8 <= cov.len()`.
                unsafe { _mm_loadl_epi64(cov.as_ptr().add(at) as *const __m128i) }
            } else if cov_len >= 8 {
                let mut w = [0u8; 8];
                w.copy_from_slice(&cov[cov_len - 8..]);
                let v = u64::from_le_bytes(w) >> (8 * (at + 8 - cov_len));
                _mm_cvtsi64_si128(v as i64)
            } else {
                let mut v = 0u64;
                for k in 0..rem {
                    v |= (cov[at + k] as u64) << (8 * k);
                }
                _mm_cvtsi64_si128(v as i64)
            };
            // SAFETY: lanes `0..rem` address pixels `full..width` of this row,
            // inside `dst` (precondition); masked-off lanes are neither read
            // nor written by `vpmaskmovd`.
            unsafe {
                let p = dst.as_mut_ptr().add(d + full * 4) as *mut i32;
                let px = _mm256_maskload_epi32(p, rem_mask);
                _mm256_maskstore_epi32(p, rem_mask, blend(c, px));
            }
            row_covered += (nonzero8(c) & rem_bits).count_ones();
        }
        covered += row_covered as u64;
    }
    covered
}

/// [`blend_mask`] with the body vectorized eight pixels at a time (AVX2) and
/// the scalar reference for the tail. Installed by [`KernelSet::select`] when
/// the CPU has AVX2; it also re-probes (a cached atomic load) so a direct call
/// on a CPU without AVX2 is still sound and simply runs the scalar kernel.
///
/// Bit-for-bit identical to [`blend_mask`]: every lane runs the same `u16`
/// `mul_div_255` op sequence, the sum is truncated with `& 0xFF` exactly as
/// the scalar `as u8` does, and the scalar early-outs are no-ops in this
/// arithmetic (`mul_div_255(x, 0) == 0`, `mul_div_255(x, 255) == x`, so a
/// zero-coverage pixel composites to itself). Verified by
/// `tests::blend_mask_avx2_matches_scalar_bit_exact`.
#[cfg(target_arch = "x86_64")]
#[allow(
    unsafe_code,
    reason = "dispatches the AVX2 mask-blend kernel behind a runtime probe; see SAFETY comment"
)]
pub fn blend_mask_avx2(dst: &mut [u8], cov: &[u8], rgb: [u8; 3], alpha: u8) {
    debug_assert_eq!(dst.len(), cov.len() * 4);
    let n = cov.len().min(dst.len() / 4);
    if n > 0 && is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 is present (runtime probe above), and the slices are
        // trimmed to exactly `n` pixels, which is the body's precondition.
        unsafe {
            blend_mask_avx2_body(&mut dst[..n * 4], &cov[..n], rgb, alpha);
        }
        return;
    }
    blend_mask(dst, cov, rgb, alpha);
}

/// The AVX2 body of [`blend_mask_avx2`]: eight RGBA8 pixels per iteration in
/// two 16-lane `u16` registers (pixels 0–3 and 4–7), the per-pixel alpha
/// broadcast to its four channels with an in-lane byte shuffle. A remainder
/// of four to seven pixels — glyph rows at 72 DPI are mostly this wide —
/// takes one half-width step (128-bit pixel load and store, the same
/// arithmetic in the low register); the last zero to three pixels run the
/// scalar reference. Staging a short row through a padded stack buffer was
/// measured at twice the scalar cost (two copies plus a store-forwarding
/// stall), so the tail stays direct.
///
/// # Safety
/// * The CPU must support AVX2 (the caller's runtime probe establishes this).
/// * `dst.len() == cov.len() * 4`; the loads and stores below are unchecked
///   over exactly whole 8- and 4-pixel groups of that range.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(unsafe_code)]
unsafe fn blend_mask_avx2_body(dst: &mut [u8], cov: &[u8], rgb: [u8; 3], alpha: u8) {
    use core::arch::x86_64::*;

    let n = cov.len();
    debug_assert_eq!(dst.len(), n * 4);
    let full = n & !7;

    // Source colour per channel, premultiplied by `a` in the loop; channel 3's
    // source is 255 so `mul_div_255(255, a) == a` reproduces the scalar
    // `a + mul_div_255(px[3], ia)`.
    let (r, g, b) = (rgb[0] as i16, rgb[1] as i16, rgb[2] as i16);
    let src = _mm256_setr_epi16(r, g, b, 255, r, g, b, 255, r, g, b, 255, r, g, b, 255);
    let c128 = _mm256_set1_epi16(128);
    let c255 = _mm256_set1_epi16(255);
    let byte_mask = _mm256_set1_epi16(0x00FF);
    let alpha8 = _mm_set1_epi16(alpha as i16);
    let half8 = _mm_set1_epi16(128);
    // Byte-shuffle indices (per 128-bit lane) that spread the eight `u16`
    // alphas `a0..a7` to four lanes each: `[a0 a0 a0 a0 a1 a1 a1 a1 | a2 .. a3 ..]`.
    let idx_lo = _mm256_setr_epi8(
        0, 1, 0, 1, 0, 1, 0, 1, 2, 3, 2, 3, 2, 3, 2, 3, 4, 5, 4, 5, 4, 5, 4, 5, 6, 7, 6, 7, 6, 7,
        6, 7,
    );
    let idx_hi = _mm256_setr_epi8(
        8, 9, 8, 9, 8, 9, 8, 9, 10, 11, 10, 11, 10, 11, 10, 11, 12, 13, 12, 13, 12, 13, 12, 13, 14,
        15, 14, 15, 14, 15, 14, 15,
    );

    // `mul_div_255` on 16 `u16` lanes: `t = a*b + 128; (t + (t >> 8)) >> 8`.
    // `a*b + 128 <= 65153` and `t + (t >> 8) <= 65407`, so nothing wraps.
    #[inline(always)]
    fn mul_div_255_x16(a: __m256i, b: __m256i, c128: __m256i) -> __m256i {
        // SAFETY: plain AVX2 register arithmetic, no memory access; the
        // enclosing function is compiled with AVX2 enabled.
        unsafe {
            let t = _mm256_add_epi16(_mm256_mullo_epi16(a, b), c128);
            _mm256_srli_epi16::<8>(_mm256_add_epi16(t, _mm256_srli_epi16::<8>(t)))
        }
    }

    // One vector step: eight coverage bytes at `cov` against 32 pixel bytes at
    // `dst`. Contract for callers: both pointers must be valid for those reads
    // and the 32-byte write (AVX2 is enabled in this function).
    let blend8 = |cov: *const u8, dst: *mut u8| {
        // SAFETY: the pointer contract stated above.
        unsafe {
            let c = _mm_loadl_epi64(cov as *const __m128i);
            let mut a = _mm_cvtepu8_epi16(c);
            if alpha != 255 {
                let t = _mm_add_epi16(_mm_mullo_epi16(a, alpha8), half8);
                a = _mm_srli_epi16::<8>(_mm_add_epi16(t, _mm_srli_epi16::<8>(t)));
            }
            let a_both = _mm256_broadcastsi128_si256(a);
            let a_lo = _mm256_shuffle_epi8(a_both, idx_lo);
            let a_hi = _mm256_shuffle_epi8(a_both, idx_hi);

            let px = _mm256_loadu_si256(dst as *const __m256i);
            let d_lo = _mm256_cvtepu8_epi16(_mm256_castsi256_si128(px));
            let d_hi = _mm256_cvtepu8_epi16(_mm256_extracti128_si256::<1>(px));

            let out_lo = _mm256_and_si256(
                _mm256_add_epi16(
                    mul_div_255_x16(src, a_lo, c128),
                    mul_div_255_x16(d_lo, _mm256_sub_epi16(c255, a_lo), c128),
                ),
                byte_mask,
            );
            let out_hi = _mm256_and_si256(
                _mm256_add_epi16(
                    mul_div_255_x16(src, a_hi, c128),
                    mul_div_255_x16(d_hi, _mm256_sub_epi16(c255, a_hi), c128),
                ),
                byte_mask,
            );
            // `packus` interleaves per 128-bit lane (`[p0 p1 p4 p5 | p2 p3 p6 p7]`);
            // the 64-bit permute restores pixel order.
            let packed = _mm256_packus_epi16(out_lo, out_hi);
            let out = _mm256_permute4x64_epi64::<0b11_01_10_00>(packed);
            _mm256_storeu_si256(dst as *mut __m256i, out);
        }
    };

    // Half step: four coverage bytes against 16 pixel bytes, same contract
    // (valid for a 4-byte read, a 16-byte read and a 16-byte write).
    let blend4 = |cov: *const u8, dst: *mut u8| {
        // SAFETY: the pointer contract stated above.
        unsafe {
            let c = _mm_cvtsi32_si128((cov as *const i32).read_unaligned());
            let mut a = _mm_cvtepu8_epi16(c);
            if alpha != 255 {
                let t = _mm_add_epi16(_mm_mullo_epi16(a, alpha8), half8);
                a = _mm_srli_epi16::<8>(_mm_add_epi16(t, _mm_srli_epi16::<8>(t)));
            }
            let a_lo = _mm256_shuffle_epi8(_mm256_broadcastsi128_si256(a), idx_lo);
            let px = _mm_loadu_si128(dst as *const __m128i);
            let d_lo = _mm256_cvtepu8_epi16(px);
            let out_lo = _mm256_and_si256(
                _mm256_add_epi16(
                    mul_div_255_x16(src, a_lo, c128),
                    mul_div_255_x16(d_lo, _mm256_sub_epi16(c255, a_lo), c128),
                ),
                byte_mask,
            );
            // Per-lane pack puts pixels 0–1 in the low lane's low half and
            // 2–3 in the high lane's low half; gather both low halves.
            let packed = _mm256_packus_epi16(out_lo, out_lo);
            let out = _mm256_permute4x64_epi64::<0b00_00_10_00>(packed);
            _mm_storeu_si128(dst as *mut __m128i, _mm256_castsi256_si128(out));
        }
    };

    let mut i = 0usize;
    while i < full {
        // SAFETY: `i + 8 <= full <= n == cov.len()` and `(i + 8) * 4 <= dst.len()`.
        unsafe {
            blend8(cov.as_ptr().add(i), dst.as_mut_ptr().add(i * 4));
        }
        i += 8;
    }
    if n - i >= 4 {
        // SAFETY: `i + 4 <= n == cov.len()` and `(i + 4) * 4 <= dst.len()`.
        unsafe {
            blend4(cov.as_ptr().add(i), dst.as_mut_ptr().add(i * 4));
        }
        i += 4;
    }
    if i < n {
        blend_mask(&mut dst[i * 4..], &cov[i..], rgb, alpha);
    }
}

/// A separable per-channel blend function on straight components in `[0, 1]`
/// (ISO 32000-1 §11.3.5). Selected once per command, never branched per pixel.
pub type BlendFn = fn(f32, f32) -> f32;

/// The blend function for a mode, or `None` for `Normal` (use the fast
/// source-over path) and the non-separable modes (not yet implemented).
pub fn separable_blend(mode: pdf_page_ir::BlendMode) -> Option<BlendFn> {
    use pdf_page_ir::BlendMode as M;
    Some(match mode {
        M::Multiply => |b, s| b * s,
        M::Screen => |b, s| b + s - b * s,
        M::Overlay => |b, s| hard_light(s, b),
        M::Darken => |b, s| b.min(s),
        M::Lighten => |b, s| b.max(s),
        M::ColorDodge => |b, s| {
            if b == 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        },
        M::ColorBurn => |b, s| {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        },
        M::HardLight => |b, s| hard_light(b, s),
        M::SoftLight => |b, s| soft_light(b, s),
        M::Difference => |b, s| (b - s).abs(),
        M::Exclusion => |b, s| b + s - 2.0 * b * s,
        // Normal → fast path; non-separable → deferred.
        _ => return None,
    })
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * (2.0 * s)
    } else {
        screen(b, 2.0 * s - 1.0)
    }
}

fn screen(b: f32, s: f32) -> f32 {
    b + s - b * s
}

fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        let d = if b <= 0.25 {
            ((16.0 * b - 12.0) * b + 4.0) * b
        } else {
            b.sqrt()
        };
        b + (2.0 * s - 1.0) * (d - b)
    }
}

/// A non-separable blend function on straight RGB triples in `[0, 1]`
/// (ISO 32000-1 §11.3.5.3 — Hue/Saturation/Color/Luminosity).
pub type NonSepBlendFn = fn([f32; 3], [f32; 3]) -> [f32; 3];

/// The non-separable blend function for a mode, or `None`.
pub fn nonseparable_blend(mode: pdf_page_ir::BlendMode) -> Option<NonSepBlendFn> {
    use pdf_page_ir::BlendMode as M;
    Some(match mode {
        M::Hue => |cb, cs| set_lum(set_sat(cs, sat(cb)), lum(cb)),
        M::Saturation => |cb, cs| set_lum(set_sat(cb, sat(cs)), lum(cb)),
        M::Color => |cb, cs| set_lum(cs, lum(cb)),
        M::Luminosity => |cb, cs| set_lum(cb, lum(cs)),
        _ => return None,
    })
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        for ch in &mut c {
            *ch = l + (*ch - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for ch in &mut c {
            *ch = l + (*ch - l) * (1.0 - l) / (x - l);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(mut c: [f32; 3], s: f32) -> [f32; 3] {
    // Indices of min/mid/max channels.
    let (mut lo, mut mid, mut hi) = (0usize, 1usize, 2usize);
    if c[lo] > c[mid] {
        std::mem::swap(&mut lo, &mut mid);
    }
    if c[mid] > c[hi] {
        std::mem::swap(&mut mid, &mut hi);
    }
    if c[lo] > c[mid] {
        std::mem::swap(&mut lo, &mut mid);
    }
    if c[hi] > c[lo] {
        c[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        c[hi] = s;
    } else {
        c[mid] = 0.0;
        c[hi] = 0.0;
    }
    c[lo] = 0.0;
    c
}

/// Composite one premultiplied pixel: given the straight source `cs`, source
/// alpha `a_s`, backdrop alpha `da`, and the pre-computed blended straight
/// color `b`, write the premultiplied source-over result (ISO §11.3.6).
#[inline(always)]
pub(crate) fn composite_px(px: &mut [u8], cs: [f32; 3], a_s: f32, b: [f32; 3], da: f32) {
    for ch in 0..3 {
        let dc_pm = px[ch] as f32 / 255.0;
        let mixed = (1.0 - da) * cs[ch] + da * b[ch];
        let out = a_s * mixed + (1.0 - a_s) * dc_pm;
        px[ch] = (out * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    }
    let out_a = a_s + (1.0 - a_s) * da;
    px[3] = (out_a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
}

/// General source-over composite of a straight color `rgb` with per-pixel
/// coverage `cov` (scaled by `alpha`) under a *separable* blend mode. The slow,
/// correct path (per-pixel un-premultiply + f32 blend); Normal-blend painting
/// never comes here (advice §5).
pub fn blend_span(dst: &mut [u8], cov: &[u8], rgb: [u8; 3], alpha: u8, blend: BlendFn) {
    debug_assert_eq!(dst.len(), cov.len() * 4);
    let cs = [
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    ];
    for (px, &c) in dst.chunks_exact_mut(4).zip(cov) {
        if c == 0 {
            continue;
        }
        let a_s = c as f32 / 255.0 * (alpha as f32 / 255.0);
        if a_s <= 0.0 {
            continue;
        }
        let da = px[3] as f32 / 255.0;
        let inv_da = if da > 0.0 { 1.0 / da } else { 0.0 };
        let b = [
            blend(px[0] as f32 / 255.0 * inv_da, cs[0]),
            blend(px[1] as f32 / 255.0 * inv_da, cs[1]),
            blend(px[2] as f32 / 255.0 * inv_da, cs[2]),
        ];
        composite_px(px, cs, a_s, b, da);
    }
}

/// As [`blend_span`], for a *non-separable* blend mode (whole-triple blend).
pub fn blend_span_nonsep(
    dst: &mut [u8],
    cov: &[u8],
    rgb: [u8; 3],
    alpha: u8,
    blend: NonSepBlendFn,
) {
    debug_assert_eq!(dst.len(), cov.len() * 4);
    let cs = [
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    ];
    for (px, &c) in dst.chunks_exact_mut(4).zip(cov) {
        if c == 0 {
            continue;
        }
        let a_s = c as f32 / 255.0 * (alpha as f32 / 255.0);
        if a_s <= 0.0 {
            continue;
        }
        let da = px[3] as f32 / 255.0;
        let inv_da = if da > 0.0 { 1.0 / da } else { 0.0 };
        let cb = [
            px[0] as f32 / 255.0 * inv_da,
            px[1] as f32 / 255.0 * inv_da,
            px[2] as f32 / 255.0 * inv_da,
        ];
        let b = blend(cb, cs);
        composite_px(px, cs, a_s, b, da);
    }
}

/// A table of the span kernels, selected once. Extended with image/glyph/mask
/// kernels in later checkpoints; a SIMD variant provides the same signatures.
#[derive(Debug, Clone, Copy)]
pub struct KernelSet {
    pub fill_opaque: fn(&mut [u8], [u8; 4]),
    pub blend_const: fn(&mut [u8], [u8; 3], u8),
    pub blend_mask: fn(&mut [u8], &[u8], [u8; 3], u8),
    /// [`blend_mask_rect`]: a whole coverage rectangle (one glyph) per call,
    /// returning its non-zero coverage count.
    #[allow(clippy::type_complexity)]
    pub blend_mask_rect: fn(&mut [u8], usize, &[u8], usize, usize, usize, [u8; 3], u8) -> u64,
}

impl KernelSet {
    /// The portable scalar reference kernels.
    pub const fn scalar() -> Self {
        Self {
            fill_opaque,
            blend_const,
            blend_mask,
            blend_mask_rect,
        }
    }

    /// Choose the best kernel set for this CPU: the scalar table, with the
    /// mask blends swapped for [`blend_mask_avx2`] / [`blend_mask_rect_avx2`]
    /// when the CPU has AVX2.
    /// `PDF_RENDERER_KERNELS=scalar` keeps the scalar table, for paired A/B
    /// isolation; every SIMD kernel is bit-identical to its scalar reference,
    /// so the switch only ever changes speed.
    pub fn select() -> Self {
        let set = Self::scalar();
        #[cfg(target_arch = "x86_64")]
        if simd_kernels_enabled() && is_x86_feature_detected!("avx2") {
            return Self {
                blend_mask: blend_mask_avx2,
                blend_mask_rect: blend_mask_rect_avx2,
                ..set
            };
        }
        set
    }
}

/// Whether SIMD kernels may be selected (default yes); `PDF_RENDERER_KERNELS=
/// scalar` (or `0`/`off`) forces the scalar table. Read once.
#[cfg(target_arch = "x86_64")]
fn simd_kernels_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("PDF_RENDERER_KERNELS") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "scalar" | "0" | "off"
        ),
        Err(_) => true,
    })
}

impl Default for KernelSet {
    fn default() -> Self {
        Self::scalar()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn mul_div_255_is_exact_at_extremes() {
        assert_eq!(mul_div_255(255, 255), 255);
        assert_eq!(mul_div_255(0, 255), 0);
        assert_eq!(mul_div_255(255, 0), 0);
        assert_eq!(mul_div_255(128, 255), 128);
        // 255*x/255 == x for all x.
        for x in 0..=255u16 {
            assert_eq!(mul_div_255(x, 255), x);
        }
    }

    #[test]
    fn blend_const_over_white_is_source_over() {
        // 50% red over opaque white → premultiplied (255,128,128,255).
        let mut dst = [255u8, 255, 255, 255];
        blend_const(&mut dst, [255, 0, 0], 128);
        assert_eq!(dst[0], 255);
        assert!((dst[1] as i32 - 128).abs() <= 1);
        assert!((dst[2] as i32 - 128).abs() <= 1);
        assert_eq!(dst[3], 255);
    }

    #[test]
    fn blend_mask_zero_coverage_is_noop() {
        let mut dst = [10u8, 20, 30, 40];
        blend_mask(&mut dst, &[0], [255, 255, 255], 255);
        assert_eq!(dst, [10, 20, 30, 40]);
    }

    /// Tiny deterministic xorshift so the fuzz below is reproducible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn byte(&mut self) -> u8 {
            (self.next() >> 24) as u8
        }
    }

    /// The AVX2 mask blend must be byte-for-byte identical to the scalar
    /// reference: every coverage value, every alpha, random colours and
    /// backdrops, and span lengths that exercise the vector body, the scalar
    /// tail, and both together. On a CPU without AVX2 the dispatcher runs the
    /// scalar kernel and the comparison is trivially exact.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn blend_mask_avx2_matches_scalar_bit_exact() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        // Exhaustive coverage × alpha over fixed backdrops, 8 pixels wide.
        for alpha in 0..=255u8 {
            for base in 0..32u16 {
                let cov: Vec<u8> = (0..8).map(|k| (base * 8 + k) as u8).collect();
                let rgb = [rng.byte(), rng.byte(), rng.byte()];
                let mut dst: Vec<u8> = (0..32).map(|_| rng.byte()).collect();
                let mut reference = dst.clone();
                blend_mask(&mut reference, &cov, rgb, alpha);
                blend_mask_avx2(&mut dst, &cov, rgb, alpha);
                assert_eq!(dst, reference, "alpha {alpha} cov {cov:?} rgb {rgb:?}");
            }
        }
        // Random spans of every length up to a few vector widths, including
        // the opaque-destination backdrop text normally composites onto.
        for len in 0..=41usize {
            for _ in 0..40 {
                let alpha = match rng.next() % 4 {
                    0 => 255,
                    1 => 0,
                    _ => rng.byte(),
                };
                let cov: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
                let rgb = [rng.byte(), rng.byte(), rng.byte()];
                let mut dst: Vec<u8> = (0..len * 4)
                    .map(|k| {
                        if k % 4 == 3 || rng.next() & 1 == 0 {
                            255
                        } else {
                            rng.byte()
                        }
                    })
                    .collect();
                let mut reference = dst.clone();
                blend_mask(&mut reference, &cov, rgb, alpha);
                blend_mask_avx2(&mut dst, &cov, rgb, alpha);
                assert_eq!(dst, reference, "len {len} alpha {alpha} rgb {rgb:?}");
            }
        }
    }

    /// The whole-glyph rectangle blend, scalar and AVX2, must equal the
    /// per-row reference the glyph blit used before (one `blend_mask` per row
    /// plus a separate non-zero count), byte for byte and count for count:
    /// widths 0..=41 (vector body, masked remainder, both), several heights,
    /// padded and tight strides, and coverage buffers that end exactly at the
    /// last row (the end-of-bitmap remainder paths, including bitmaps under
    /// eight bytes). Pixels outside the rectangle must be untouched.
    #[test]
    fn blend_mask_rect_avx2_matches_scalar_bit_exact() {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        for width in 0..=41usize {
            for height in [0usize, 1, 2, 3, 5, 9] {
                for _ in 0..12 {
                    let alpha = match rng.next() % 4 {
                        0 => 255,
                        1 => 0,
                        _ => rng.byte(),
                    };
                    let rgb = [rng.byte(), rng.byte(), rng.byte()];
                    let cov_stride = width + (rng.next() % 3) as usize * (rng.next() % 9) as usize;
                    let dst_stride = width * 4 + 4 * (rng.next() % 6) as usize;
                    let cov_len = if height == 0 {
                        (rng.next() % 4) as usize
                    } else {
                        (height - 1) * cov_stride + width + (rng.next() % 2) as usize * 5
                    };
                    let cov: Vec<u8> = (0..cov_len)
                        .map(|_| match rng.next() % 3 {
                            0 => 0,
                            1 => 255,
                            _ => rng.byte(),
                        })
                        .collect();
                    let dst_len = height * dst_stride + 12;
                    let dst0: Vec<u8> = (0..dst_len)
                        .map(|k| {
                            if k % 4 == 3 || rng.next() & 1 == 0 {
                                255
                            } else {
                                rng.byte()
                            }
                        })
                        .collect();

                    // Per-row reference, exactly as the blit did it before.
                    let mut reference = dst0.clone();
                    let mut ref_count = 0u64;
                    for r in 0..height {
                        let src = &cov[r * cov_stride..r * cov_stride + width];
                        blend_mask(
                            &mut reference[r * dst_stride..r * dst_stride + width * 4],
                            src,
                            rgb,
                            alpha,
                        );
                        ref_count += src.iter().filter(|&&c| c != 0).count() as u64;
                    }

                    let mut scalar = dst0.clone();
                    let n = blend_mask_rect(
                        &mut scalar,
                        dst_stride,
                        &cov,
                        cov_stride,
                        width,
                        height,
                        rgb,
                        alpha,
                    );
                    assert_eq!(scalar, reference, "scalar w{width} h{height} a{alpha}");
                    assert_eq!(n, ref_count, "scalar count w{width} h{height}");

                    #[cfg(target_arch = "x86_64")]
                    {
                        let mut simd = dst0.clone();
                        let n = blend_mask_rect_avx2(
                            &mut simd, dst_stride, &cov, cov_stride, width, height, rgb, alpha,
                        );
                        assert_eq!(
                            simd, reference,
                            "avx2 w{width} h{height} a{alpha} cs{cov_stride} ds{dst_stride}"
                        );
                        assert_eq!(n, ref_count, "avx2 count w{width} h{height}");
                    }
                }
            }
        }
    }

    #[test]
    fn fill_opaque_overwrites() {
        let mut dst = [0u8; 12];
        fill_opaque(&mut dst, [1, 2, 3, 255]);
        assert_eq!(dst, [1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255]);
    }
}
