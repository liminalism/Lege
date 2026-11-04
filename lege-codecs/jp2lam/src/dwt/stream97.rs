//! Fused, streaming inverse 9/7 synthesis for lattice-aligned tiles.
//!
//! The classic in-place inverse runs two sweeps per resolution level — every
//! row through the horizontal lifting, then every column band through the
//! vertical lifting — so a plane that does not fit in cache is read and
//! written twice per level. This module instead walks each level once: every
//! input row is interleaved and lifted horizontally as it enters an eight-row
//! ring, the four vertical lifting steps run pipelined behind it (a step for
//! row `y` as soon as its neighbours have reached the previous step), and each
//! finished row leaves the ring once. Output goes to a separate buffer, so the
//! subband rows still waiting to be read are never overwritten; levels
//! ping-pong between two buffers.
//!
//! [`LevelSynth`] exposes one level as a row producer. A consumer can take the
//! finest level's rows straight out of the ring — the packed-RGB path pairs
//! three components' producers with the colour transform and byte packing, so
//! the full-resolution planes are never materialised. A producer can also
//! start part-way down (`first_row`): output row `y` depends on input rows
//! `y - 4 ..= y + 4` only, so a pipeline primed four rows early yields the
//! same values as one run from the top, which makes row bands independent.
//!
//! The arithmetic is exactly the sequence of `irrev97`'s in-place path —
//! scale, then `x += c * (left + right)` per lifting step, each in IEEE
//! single precision with no fused multiply-add — so the reconstructed samples
//! are bit-identical to it. `tests` checks that for both the scalar and the
//! `wide` kernels across sizes, levels, degenerate axes and band starts.

use super::InverseDwtTiming;
use super::irrev97::{ALPHA, BETA, DELTA, GAMMA, INV_K, K};
use crate::simd::Kernel;
#[cfg(feature = "simd")]
use wide::f32x8;

/// Output row `y` is final once input row `y + LAG` has entered the ring.
const LAG: usize = 4;
/// Ring slots. Rows referenced at step `r` lie in `[r - 5, r]` (the mirrored
/// bottom neighbour reaches back furthest), so eight slots never alias.
const RING: usize = 8;

/// Sizes of the resolutions `0 ..= levels` (coarse to fine): `sizes[k]` is the
/// output of synthesis level `k`, `sizes[0]` the LL band.
fn resolution_sizes(width: usize, height: usize, levels: usize) -> Vec<(usize, usize)> {
    let mut sizes = Vec::with_capacity(levels + 1);
    let (mut w, mut h) = (width, height);
    sizes.push((w, h));
    for _ in 0..levels {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
        sizes.push((w, h));
    }
    sizes.reverse();
    sizes
}

/// Inverse 9/7 synthesis of `data` (standard nested subband layout, origin on
/// the decomposition lattice) through `levels` levels. Returns the
/// reconstructed plane; `data` is consumed and may be returned as-is when
/// there is nothing to do.
pub(crate) fn inverse_97_2d_even(
    data: Vec<f32>,
    width: usize,
    height: usize,
    levels: u8,
    kernel: Kernel,
    mut timing: Option<&mut InverseDwtTiming>,
) -> Vec<f32> {
    debug_assert_eq!(data.len(), width * height);
    if width == 0 || height == 0 || levels == 0 {
        return data;
    }
    let prefix =
        inverse_97_2d_even_but_last(data, width, height, levels, kernel, timing.as_deref_mut());
    let level_start = timing.as_ref().map(|_| std::time::Instant::now());
    let mut synth = prefix.final_level(kernel, 0);
    let mut out = vec![0f32; width * height];
    for y in 0..height {
        out[y * width..(y + 1) * width].copy_from_slice(synth.next_row());
    }
    if let (Some(timing), Some(start)) = (timing, level_start) {
        record_level(timing, start, synth.horizontal_ns);
    }
    out
}

fn record_level(timing: &mut InverseDwtTiming, start: std::time::Instant, horizontal_ns: u64) {
    let total = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let horizontal = horizontal_ns.min(total);
    timing.horizontal_ns = timing.horizontal_ns.saturating_add(horizontal);
    timing.vertical_ns = timing.vertical_ns.saturating_add(total - horizontal);
    timing.level_ns.push(total);
}

/// A plane with every synthesis level but the finest applied: the finest
/// level's LL input is `ll` (or the LL corner of `data` when there was only
/// one level) and its HL/LH/HH bands are still in `data`.
pub(crate) struct SynthesisPrefix {
    data: Vec<f32>,
    width: usize,
    height: usize,
    /// Output of the second-finest level, `ll_width` wide; `None` for a
    /// single-level transform.
    ll: Option<Vec<f32>>,
    ll_width: usize,
}

/// Run all levels but the last. `levels` must be at least 1 and the plane
/// non-empty.
pub(crate) fn inverse_97_2d_even_but_last(
    data: Vec<f32>,
    width: usize,
    height: usize,
    levels: u8,
    kernel: Kernel,
    mut timing: Option<&mut InverseDwtTiming>,
) -> SynthesisPrefix {
    debug_assert!(levels >= 1 && width > 0 && height > 0);
    debug_assert_eq!(data.len(), width * height);
    let levels = usize::from(levels);
    let sizes = resolution_sizes(width, height, levels);
    let mut prev: Option<Vec<f32>> = None;
    let mut spare: Vec<f32> = Vec::new();
    for level in 1..levels {
        let (rw, rh) = sizes[level];
        let source = match &prev {
            None => LlSource {
                rows: &data,
                stride: width,
            },
            Some(prev) => LlSource {
                rows: prev,
                stride: sizes[level - 1].0,
            },
        };
        let level_start = timing.as_ref().map(|_| std::time::Instant::now());
        let mut out = std::mem::take(&mut spare);
        if out.len() < rw * rh {
            out.resize(rw * rh, 0.0);
        }
        let mut synth = LevelSynth::new(source, &data, width, rw, rh, kernel, 0, timing.is_some());
        for y in 0..rh {
            out[y * rw..(y + 1) * rw].copy_from_slice(synth.next_row());
        }
        if let (Some(timing), Some(start)) = (timing.as_deref_mut(), level_start) {
            record_level(timing, start, synth.horizontal_ns);
        }
        spare = prev.replace(out).unwrap_or_default();
    }
    let ll_width = sizes[levels - 1].0;
    SynthesisPrefix {
        data,
        width,
        height,
        ll: prev,
        ll_width,
    }
}

impl SynthesisPrefix {
    /// A producer for the finest level's rows, starting at output row
    /// `first_row`.
    pub(crate) fn final_level(&self, kernel: Kernel, first_row: usize) -> LevelSynth<'_> {
        let source = match &self.ll {
            None => LlSource {
                rows: &self.data,
                stride: self.width,
            },
            Some(ll) => LlSource {
                rows: ll,
                stride: self.ll_width,
            },
        };
        LevelSynth::new(
            source,
            &self.data,
            self.width,
            self.width,
            self.height,
            kernel,
            first_row,
            false,
        )
    }

    /// Width of the finest level.
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// Height of the finest level.
    pub(crate) fn height(&self) -> usize {
        self.height
    }
}

/// Where a level's LL rows come from: the caller's plane for the first level,
/// the previous level's output afterwards.
#[derive(Clone, Copy)]
struct LlSource<'a> {
    rows: &'a [f32],
    stride: usize,
}

/// One synthesis level as a row producer: `next_row` returns the output rows
/// in order from `first_row`.
pub(crate) struct LevelSynth<'a> {
    source: LlSource<'a>,
    bands: &'a [f32],
    band_stride: usize,
    rw: usize,
    rh: usize,
    /// Low-pass sample counts horizontally and vertically.
    sn: usize,
    sv: usize,
    kernel: Kernel,
    /// Padded low-pass row: one guard sample on each side.
    lo: Vec<f32>,
    /// Padded high-pass row: one guard sample on each side.
    hi: Vec<f32>,
    /// `RING` interleaved rows, each `rw` long.
    ring: Vec<f32>,
    /// Next pipeline step (the input row it loads).
    step: usize,
    /// Next output row `next_row` returns.
    next: usize,
    /// Time spent in the horizontal synthesis, when timing was requested.
    timed: bool,
    horizontal_ns: u64,
}

impl<'a> LevelSynth<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        source: LlSource<'a>,
        bands: &'a [f32],
        band_stride: usize,
        rw: usize,
        rh: usize,
        kernel: Kernel,
        first_row: usize,
        timed: bool,
    ) -> Self {
        debug_assert!(rw > 0 && rh > 0 && first_row < rh);
        let sn = rw.div_ceil(2);
        let sv = rh.div_ceil(2);
        let mut synth = Self {
            source,
            bands,
            band_stride,
            rw,
            rh,
            sn,
            sv,
            kernel,
            lo: vec![0.0; sn + 2],
            hi: vec![0.0; (rw - sn) + 2],
            ring: vec![0.0; rw * RING],
            step: 0,
            next: first_row,
            timed,
            horizontal_ns: 0,
        };
        // Output row `y` depends on input rows `y - LAG ..= y + LAG` only, so
        // the pipeline can start `LAG` rows early: the rows it completes
        // before `first_row` are contaminated by the never-loaded rows above
        // the start, but nothing from `first_row` on reads them.
        synth.step = first_row.saturating_sub(LAG);
        synth
    }

    /// The next output row, as a slice into the ring (valid until the next
    /// call).
    pub(crate) fn next_row(&mut self) -> &[f32] {
        debug_assert!(self.next < self.rh);
        let y = self.next;
        self.next += 1;
        if self.rh < 2 {
            // A one-row resolution is an identity vertically (no scaling
            // either, see `irrev97::inverse_97_vertical_in_place`).
            let (lo_row, hi_row) = self.input_row(0);
            let row_start = self.timed.then(std::time::Instant::now);
            synth_row(
                lo_row,
                hi_row,
                &mut self.ring[..self.rw],
                1.0,
                &mut self.lo,
                &mut self.hi,
                self.kernel,
            );
            self.add_elapsed(row_start);
            return &self.ring[..self.rw];
        }
        // Row `y` is final once step `y + LAG` has run.
        while self.step <= y + LAG {
            self.advance();
        }
        let slot = (y % RING) * self.rw;
        &self.ring[slot..slot + self.rw]
    }

    /// The deinterleaved halves of interleaved input row `r` and the vertical
    /// scale it carries: even rows come from the LL/HL pair (row `r / 2`),
    /// odd rows from the LH/HH pair (row `sv + (r - 1) / 2`).
    fn input_row(&self, r: usize) -> (&'a [f32], &'a [f32]) {
        let (sn, rw) = (self.sn, self.rw);
        if r % 2 == 0 {
            let i = r / 2;
            (
                &self.source.rows[i * self.source.stride..i * self.source.stride + sn],
                &self.bands[i * self.band_stride + sn..i * self.band_stride + rw],
            )
        } else {
            let i = self.sv + (r - 1) / 2;
            (
                &self.bands[i * self.band_stride..i * self.band_stride + sn],
                &self.bands[i * self.band_stride + sn..i * self.band_stride + rw],
            )
        }
    }

    /// One pipeline step: load input row `step` (if any) into the ring, then
    /// run lifting stage `k` on row `step - k` for `k = 1 ..= 4`. Step `k` for
    /// row `x` needs rows `x ± 1` at stage `k - 1`, which with rows arriving
    /// in order is exactly one step behind per row of lag.
    fn advance(&mut self) {
        let r = self.step;
        self.step += 1;
        let rw = self.rw;
        let rh = self.rh;
        if r < rh {
            let (lo_row, hi_row) = self.input_row(r);
            let row_scale = if r % 2 == 0 { K } else { INV_K };
            let row_start = self.timed.then(std::time::Instant::now);
            let slot = (r % RING) * rw;
            synth_row(
                lo_row,
                hi_row,
                &mut self.ring[slot..slot + rw],
                row_scale,
                &mut self.lo,
                &mut self.hi,
                self.kernel,
            );
            self.add_elapsed(row_start);
        }
        let mirror = |x: isize| -> usize {
            if x < 0 {
                (-x) as usize
            } else if x as usize >= rh {
                2 * (rh - 1) - x as usize
            } else {
                x as usize
            }
        };
        let r = r as isize;
        for (back, parity, coeff) in [
            (1isize, 0usize, -DELTA),
            (2, 1, -GAMMA),
            (3, 0, -BETA),
            (4, 1, -ALPHA),
        ] {
            let x = r - back;
            if x < 0 || x as usize >= rh || (x as usize) % 2 != parity {
                continue;
            }
            let target = (x as usize % RING) * rw;
            let above = (mirror(x - 1) % RING) * rw;
            let below = (mirror(x + 1) % RING) * rw;
            lift_ring_row(&mut self.ring, rw, target, above, below, coeff, self.kernel);
        }
    }

    #[inline]
    fn add_elapsed(&mut self, start: Option<std::time::Instant>) {
        if let Some(start) = start {
            self.horizontal_ns = self
                .horizontal_ns
                .saturating_add(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }
}

/// `ring[target] += coeff * (ring[above] + ring[below])` over `rw` samples.
/// The target row always has the opposite parity of its neighbours, so the
/// three slots are distinct unless `above == below` (a mirrored boundary).
fn lift_ring_row(
    ring: &mut [f32],
    rw: usize,
    target: usize,
    above: usize,
    below: usize,
    coeff: f32,
    kernel: Kernel,
) {
    debug_assert_ne!(target, above);
    debug_assert_ne!(target, below);
    if above == below {
        let [t, n] = ring
            .get_disjoint_mut([target..target + rw, above..above + rw])
            .expect("target and mirrored neighbour rows are disjoint ring slots");
        lift_row(t, n, n, coeff, kernel);
    } else {
        let [t, a, b] = ring
            .get_disjoint_mut([target..target + rw, above..above + rw, below..below + rw])
            .expect("target and neighbour rows are disjoint ring slots");
        lift_row(t, a, b, coeff, kernel);
    }
}

/// Horizontal 1-D synthesis of one even-origin row from its deinterleaved
/// low-pass (`lo_in`, `ceil(n/2)` samples) and high-pass (`hi_in`) halves,
/// interleaved into `out` and multiplied by `row_scale` (the vertical scaling
/// step, applied here so the row is written once).
///
/// Lifting runs on the two halves while they are still separate, in padded
/// scratch rows whose guard samples carry the whole-sample symmetric
/// extension (`X[-1] = X[1]`, `X[n] = X[n-2]`), then the halves are
/// interleaved. Per sample this is the same `*= K` / `*= 1/K`, then
/// `x += c * (left + right)` four times, as `irrev97`'s in-place row path.
fn synth_row(
    lo_in: &[f32],
    hi_in: &[f32],
    out: &mut [f32],
    row_scale: f32,
    lo: &mut [f32],
    hi: &mut [f32],
    kernel: Kernel,
) {
    let sn = lo_in.len();
    let dn = hi_in.len();
    let n = sn + dn;
    debug_assert_eq!(out.len(), n);
    if n < 2 {
        // Even-origin length-one signal: the horizontal synthesis is an identity.
        if n == 1 {
            out[0] = lo_in[0] * row_scale;
        }
        return;
    }
    debug_assert!(lo.len() >= sn + 2 && hi.len() >= dn + 2);
    let lo = &mut lo[..sn + 2];
    let hi = &mut hi[..dn + 2];
    scale_into(&mut lo[1..sn + 1], lo_in, K, kernel);
    scale_into(&mut hi[1..dn + 1], hi_in, INV_K, kernel);

    // Even samples: X[2i] += c * (X[2i-1] + X[2i+1]) = c * (H[i-1] + H[i]),
    // with H[-1] = H[0] and, for odd n, H[sn-1] = H[dn-1].
    // Odd samples: X[2i+1] += c * (L[i] + L[i+1]), with L[sn] = L[sn-1] for
    // even n.
    hi[0] = hi[1];
    hi[dn + 1] = hi[dn];
    lift_row(
        &mut lo[1..sn + 1],
        &hi[0..sn],
        &hi[1..sn + 1],
        -DELTA,
        kernel,
    );
    lo[sn + 1] = lo[sn];
    lift_row(
        &mut hi[1..dn + 1],
        &lo[1..dn + 1],
        &lo[2..dn + 2],
        -GAMMA,
        kernel,
    );
    hi[0] = hi[1];
    hi[dn + 1] = hi[dn];
    lift_row(
        &mut lo[1..sn + 1],
        &hi[0..sn],
        &hi[1..sn + 1],
        -BETA,
        kernel,
    );
    lo[sn + 1] = lo[sn];
    lift_row(
        &mut hi[1..dn + 1],
        &lo[1..dn + 1],
        &lo[2..dn + 2],
        -ALPHA,
        kernel,
    );

    interleave_scaled(&lo[1..sn + 1], &hi[1..dn + 1], out, row_scale, kernel);
}

/// `target[i] += coeff * (left[i] + right[i])`.
#[inline]
fn lift_row(target: &mut [f32], left: &[f32], right: &[f32], coeff: f32, kernel: Kernel) {
    match kernel {
        Kernel::Scalar => super::irrev97::lift_row(target, left, right, coeff, false),
        Kernel::Wide => super::irrev97::lift_row(target, left, right, coeff, true),
        Kernel::Avx2 => avx2::lift_row(target, left, right, coeff),
    }
}

/// `out[i] = input[i] * scale`.
fn scale_into(out: &mut [f32], input: &[f32], scale: f32, kernel: Kernel) {
    debug_assert_eq!(out.len(), input.len());
    if kernel == Kernel::Avx2 {
        avx2::scale_into(out, input, scale);
        return;
    }
    let mut i = 0usize;
    #[cfg(feature = "simd")]
    if kernel == Kernel::Wide {
        let scale_v = f32x8::splat(scale);
        while i + 8 <= out.len() {
            let v = f32x8::new(input[i..i + 8].try_into().expect("8 lanes")) * scale_v;
            out[i..i + 8].copy_from_slice(&v.to_array());
            i += 8;
        }
    }
    let _ = kernel;
    while i < out.len() {
        out[i] = input[i] * scale;
        i += 1;
    }
}

/// `out[2i] = lo[i] * scale`, `out[2i+1] = hi[i] * scale`, with the odd
/// trailing low-pass sample when `out` has odd length.
fn interleave_scaled(lo: &[f32], hi: &[f32], out: &mut [f32], scale: f32, kernel: Kernel) {
    let dn = hi.len();
    debug_assert_eq!(out.len(), lo.len() + dn);
    if kernel == Kernel::Avx2 {
        avx2::interleave_scaled(lo, hi, out, scale);
        return;
    }
    let mut i = 0usize;
    #[cfg(feature = "simd")]
    if kernel == Kernel::Wide {
        let scale_v = f32x8::splat(scale);
        while i + 8 <= dn {
            let l = (f32x8::new(lo[i..i + 8].try_into().expect("8 lanes")) * scale_v).to_array();
            let h = (f32x8::new(hi[i..i + 8].try_into().expect("8 lanes")) * scale_v).to_array();
            let dst = &mut out[2 * i..2 * i + 16];
            for k in 0..8 {
                dst[2 * k] = l[k];
                dst[2 * k + 1] = h[k];
            }
            i += 8;
        }
    }
    let _ = kernel;
    while i < dn {
        out[2 * i] = lo[i] * scale;
        out[2 * i + 1] = hi[i] * scale;
        i += 1;
    }
    if lo.len() > dn {
        out[2 * dn] = lo[dn] * scale;
    }
}

/// The row kernels compiled with AVX2 enabled: plain loops that LLVM
/// vectorises to 256-bit lanes, with the same per-sample operation order as
/// the scalar and `wide` paths (no fused multiply-add: only `avx2` is
/// enabled), so the results are bit-identical.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod avx2 {
    #[target_feature(enable = "avx2")]
    unsafe fn lift_row_impl(target: &mut [f32], left: &[f32], right: &[f32], coeff: f32) {
        let n = target.len().min(left.len()).min(right.len());
        let (target, left, right) = (&mut target[..n], &left[..n], &right[..n]);
        for x in 0..n {
            target[x] += coeff * (left[x] + right[x]);
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn scale_into_impl(out: &mut [f32], input: &[f32], scale: f32) {
        let n = out.len().min(input.len());
        let (out, input) = (&mut out[..n], &input[..n]);
        for x in 0..n {
            out[x] = input[x] * scale;
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn interleave_scaled_impl(lo: &[f32], hi: &[f32], out: &mut [f32], scale: f32) {
        let dn = hi.len().min(lo.len());
        let (pairs, tail) = out.split_at_mut((2 * dn).min(out.len()));
        for (i, pair) in pairs.chunks_exact_mut(2).enumerate() {
            pair[0] = lo[i] * scale;
            pair[1] = hi[i] * scale;
        }
        if let (Some(last), Some(&value)) = (tail.first_mut(), lo.get(dn)) {
            *last = value * scale;
        }
    }

    // SAFETY (all three): `Kernel::Avx2` is only selected after runtime
    // detection of AVX2 (`simd::x86`).
    #[inline]
    pub(super) fn lift_row(target: &mut [f32], left: &[f32], right: &[f32], coeff: f32) {
        unsafe { lift_row_impl(target, left, right, coeff) }
    }

    #[inline]
    pub(super) fn scale_into(out: &mut [f32], input: &[f32], scale: f32) {
        unsafe { scale_into_impl(out, input, scale) }
    }

    #[inline]
    pub(super) fn interleave_scaled(lo: &[f32], hi: &[f32], out: &mut [f32], scale: f32) {
        unsafe { interleave_scaled_impl(lo, hi, out, scale) }
    }
}

/// Without x86 the AVX2 flavour is never selected; route to scalar so the
/// match arms stay total.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
mod avx2 {
    pub(super) fn lift_row(target: &mut [f32], left: &[f32], right: &[f32], coeff: f32) {
        super::super::irrev97::lift_row(target, left, right, coeff, false)
    }

    pub(super) fn scale_into(out: &mut [f32], input: &[f32], scale: f32) {
        super::scale_into(out, input, scale, crate::simd::Kernel::Scalar)
    }

    pub(super) fn interleave_scaled(lo: &[f32], hi: &[f32], out: &mut [f32], scale: f32) {
        super::interleave_scaled(lo, hi, out, scale, crate::simd::Kernel::Scalar)
    }
}

#[cfg(test)]
mod tests {
    use super::{inverse_97_2d_even, inverse_97_2d_even_but_last};
    use crate::simd::Kernel;

    fn pattern(width: usize, height: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
        (0..width * height)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let noise = (state % 2001) as f32 / 8.0 - 125.0;
                let smooth = ((i % width) as f32 * 0.31).sin() * 60.0
                    + ((i / width) as f32 * 0.17).cos() * 40.0;
                smooth + noise
            })
            .collect()
    }

    fn reference(width: usize, height: usize, levels: u8, seed: u32) -> (Vec<f32>, Vec<f32>) {
        let original = pattern(width, height, seed);
        let mut reference = original.clone();
        super::super::irrev97::inverse_97_2d_in_place_reference(
            &mut reference,
            width,
            height,
            levels,
            false,
        );
        (original, reference)
    }

    fn assert_bits(streamed: &[f32], reference: &[f32], width: usize, what: &str) {
        assert_eq!(streamed.len(), reference.len());
        for (i, (a, b)) in streamed.iter().zip(reference).enumerate() {
            assert!(
                a.to_bits() == b.to_bits(),
                "{what}: sample {i} ({}, {}) {a} != {b}",
                i % width,
                i / width
            );
        }
    }

    fn check(width: usize, height: usize, levels: u8, kernel: Kernel) {
        let seed = (width * 131 + height * 7) as u32 + levels as u32;
        let (original, reference) = reference(width, height, levels, seed);
        let streamed = inverse_97_2d_even(original, width, height, levels, kernel, None);
        assert_bits(
            &streamed,
            &reference,
            width,
            &format!("{width}x{height} levels={levels} {kernel:?}"),
        );
    }

    /// Row bands started part-way down must reproduce the full run exactly.
    fn check_banded(width: usize, height: usize, levels: u8, kernel: Kernel, first_row: usize) {
        let seed = (width * 17 + height * 3) as u32 + levels as u32 + first_row as u32;
        let (original, reference) = reference(width, height, levels, seed);
        let prefix = inverse_97_2d_even_but_last(original, width, height, levels, kernel, None);
        let mut synth = prefix.final_level(kernel, first_row);
        for y in first_row..height {
            let row = synth.next_row();
            assert_bits(
                row,
                &reference[y * width..(y + 1) * width],
                width,
                &format!(
                    "{width}x{height} levels={levels} {kernel:?} band from {first_row}, row {y}"
                ),
            );
        }
    }

    #[test]
    fn streaming_matches_in_place_reference_small_sizes() {
        let kernels = Kernel::available();
        for height in 1..=19usize {
            for width in 1..=19usize {
                for levels in 0..=4u8 {
                    for &kernel in &kernels {
                        check(width, height, levels, kernel);
                    }
                }
            }
        }
    }

    #[test]
    fn streaming_matches_in_place_reference_larger_sizes() {
        let kernels = Kernel::available();
        for &(width, height, levels) in &[
            (97usize, 61usize, 5u8),
            (128, 128, 5),
            (257, 33, 3),
            (33, 257, 3),
            (300, 211, 5),
            (1, 300, 5),
            (300, 1, 5),
            (2, 2, 1),
        ] {
            for &kernel in &kernels {
                check(width, height, levels, kernel);
            }
        }
    }

    #[test]
    fn banded_final_level_matches_full_run() {
        let kernels = Kernel::available();
        for &(width, height, levels) in
            &[(37usize, 29usize, 1u8), (64, 50, 3), (13, 2, 2), (5, 9, 1)]
        {
            for first_row in 0..height {
                for &kernel in &kernels {
                    check_banded(width, height, levels, kernel, first_row);
                }
            }
        }
    }

    #[test]
    fn profiled_timing_reports_every_level() {
        let data = pattern(64, 48, 9);
        let mut timing = super::InverseDwtTiming::default();
        let out = inverse_97_2d_even(data, 64, 48, 3, Kernel::Scalar, Some(&mut timing));
        assert_eq!(out.len(), 64 * 48);
        assert_eq!(timing.level_ns.len(), 3);
    }
}
