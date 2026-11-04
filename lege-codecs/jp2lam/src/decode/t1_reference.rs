//! The previous per-sample `FlagGrid` Tier-1 decoder, kept verbatim as the
//! bit-exact reference for the packed stripe-column decoder in `t1.rs`.
//! Only compiled for tests.

use crate::error::Result;
use crate::j2k::decode_markers::CodeBlockStyle;
use crate::mq::{MqDecoder, RawDecoder, T1_CTXNO_AGG, T1_CTXNO_UNI, T1_CTXNO_ZC};
use crate::plan::BandOrientation;
use crate::tier1::consts::{T1_SIGMA_S, T1_SIGMA_SE, T1_SIGMA_SW};
use crate::tier1::flags::FlagGrid;
use crate::tier1::helpers::{
    magnitude_context, sign_context, sign_prediction_bit, zero_coding_context,
};

use super::stats::StatsSink;
use super::t1::{PassKind, invalid, pass_kind_and_bitplane, read_segmentation_symbol};
use super::t2::DecodedCodewordSegment;

struct ReferenceScratch {
    coefficients: Vec<i32>,
    flags: FlagGrid,
}

impl ReferenceScratch {
    fn new(width: usize, height: usize) -> Self {
        Self {
            coefficients: vec![0; width * height],
            flags: FlagGrid::new(width, height),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn decode_codeblock_segments_reference(
    width: usize,
    height: usize,
    band: BandOrientation,
    max_bitplanes: u8,
    zero_bitplanes: u32,
    pass_count: u32,
    style: CodeBlockStyle,
    segments: &[DecodedCodewordSegment<'_>],
) -> Result<Vec<i32>> {
    let mut scratch = ReferenceScratch::new(width, height);
    let scratch = &mut scratch;
    let mut stats = StatsSink::disabled();
    let stats = &mut stats;
    if pass_count == 0 {
        return Ok(scratch.coefficients.clone());
    }
    let segment_passes = segments.iter().try_fold(0u32, |total, segment| {
        total
            .checked_add(segment.passes)
            .ok_or_else(|| invalid("codeword-segment pass count overflow"))
    })?;
    if segment_passes != pass_count {
        return Err(invalid(format!(
            "codeword segments carry {segment_passes} passes, expected {pass_count}"
        )));
    }

    let zero_bitplanes = u8::try_from(zero_bitplanes)
        .map_err(|_| invalid("zero-bitplane count exceeds supported range"))?;
    let magnitude_bitplanes = max_bitplanes
        .checked_sub(zero_bitplanes)
        .ok_or_else(|| invalid("zero-bitplane count exceeds subband bit-plane count"))?;
    if magnitude_bitplanes == 0 {
        return Err(invalid("non-empty code-block has no magnitude bit-planes"));
    }
    // Coefficients are represented as signed i32 magnitudes throughout this
    // decoder. A 32nd magnitude bit would make `1i32 << bitplane` negative,
    // and any still-higher bit would overflow the shift entirely. Reject such
    // signaling before entering the coding passes instead of panicking or
    // silently wrapping on malformed QCD/QCC metadata.
    if magnitude_bitplanes > 31 {
        return Err(invalid(format!(
            "code-block magnitude bit-plane count {magnitude_bitplanes} exceeds the i32 decoder limit of 31"
        )));
    }
    let max_passes = 1u32 + 3u32 * u32::from(magnitude_bitplanes.saturating_sub(1));
    if pass_count > max_passes {
        return Err(invalid(format!(
            "code-block pass count {pass_count} exceeds maximum {max_passes} for {magnitude_bitplanes} magnitude bit-planes"
        )));
    }

    let top = magnitude_bitplanes - 1;
    let mut pass_index = 0u32;
    let mut contexts = None;

    for segment in segments {
        if segment.passes == 0 {
            return Err(invalid("codeword segment carries zero coding passes"));
        }
        // D.6: a raw segment holds significance and refinement passes of the
        // fifth and later significant bit-planes; MQ contexts are untouched.
        if style.pass_is_raw(pass_index) {
            let mut raw = RawDecoder::new(&segment.data);
            for _ in 0..segment.passes {
                let (pass_kind, bitplane) = pass_kind_and_bitplane(pass_index, top)?;
                let pass_start = stats.start();
                match pass_kind {
                    PassKind::Significance => sigpass_decode_raw(
                        &mut raw,
                        bitplane,
                        &mut scratch.flags,
                        &mut scratch.coefficients,
                        width,
                        height,
                        style.vertical_causal,
                    ),
                    PassKind::Refinement => refpass_decode_raw(
                        &mut raw,
                        bitplane,
                        &mut scratch.flags,
                        &mut scratch.coefficients,
                        width,
                        height,
                    ),
                    PassKind::Cleanup => {
                        return Err(invalid("cleanup pass inside a raw bypass codeword segment"));
                    }
                }
                stats.finish(pass_start, |stats, elapsed| match pass_kind {
                    PassKind::Significance => {
                        stats.tier1_significance_ns =
                            stats.tier1_significance_ns.saturating_add(elapsed);
                    }
                    PassKind::Refinement => {
                        stats.tier1_refinement_ns =
                            stats.tier1_refinement_ns.saturating_add(elapsed);
                    }
                    PassKind::Cleanup => {}
                });
                pass_index += 1;
            }
            continue;
        }
        let mut decoder = match contexts {
            Some(saved) if !style.reset_contexts => {
                MqDecoder::new_with_contexts(&segment.data, saved)
            }
            _ => MqDecoder::new(&segment.data),
        };

        for _ in 0..segment.passes {
            let (pass_kind, bitplane) = pass_kind_and_bitplane(pass_index, top)?;
            if style.pass_is_raw(pass_index) {
                return Err(invalid("bypass-coded pass inside an MQ codeword segment"));
            }
            let pass_start = stats.start();
            match pass_kind {
                PassKind::Significance => sigpass_decode(
                    &mut decoder,
                    band,
                    bitplane,
                    &mut scratch.flags,
                    &mut scratch.coefficients,
                    width,
                    height,
                    style.vertical_causal,
                ),
                PassKind::Refinement => refpass_decode(
                    &mut decoder,
                    bitplane,
                    &mut scratch.flags,
                    &mut scratch.coefficients,
                    width,
                    height,
                    style.vertical_causal,
                ),
                PassKind::Cleanup => {
                    cleanup_decode(
                        &mut decoder,
                        band,
                        bitplane,
                        &mut scratch.flags,
                        &mut scratch.coefficients,
                        width,
                        height,
                        style.vertical_causal,
                    );
                    if style.segmentation_symbols {
                        read_segmentation_symbol(&mut decoder, pass_index + 1 == pass_count)?;
                    }
                    debug_assert!(
                        !scratch.flags.has_visited(),
                        "cleanup pass must clear every visited flag"
                    );
                }
            }
            stats.finish(pass_start, |stats, elapsed| match pass_kind {
                PassKind::Significance => {
                    stats.tier1_significance_ns =
                        stats.tier1_significance_ns.saturating_add(elapsed);
                }
                PassKind::Refinement => {
                    stats.tier1_refinement_ns = stats.tier1_refinement_ns.saturating_add(elapsed);
                }
                PassKind::Cleanup => {
                    stats.tier1_cleanup_ns = stats.tier1_cleanup_ns.saturating_add(elapsed);
                }
            });
            pass_index += 1;
            if style.reset_contexts {
                decoder.reset_contexts();
            }
        }
        contexts = Some(decoder.contexts());
    }

    for (index, coefficient) in scratch.coefficients.iter_mut().enumerate() {
        if scratch.flags.is_negative_idx(index) {
            *coefficient = -*coefficient;
        }
    }
    Ok(scratch.coefficients.clone())
}

fn sigpass_decode(
    decoder: &mut MqDecoder<'_>,
    band: BandOrientation,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    height: usize,
    vertical_causal: bool,
) {
    let full_stripes = height / 4;
    let rem = height % 4;

    for k in (0..full_stripes * 4).step_by(4) {
        for x in 0..width {
            for ci in 0..4 {
                sigpass_step(
                    decoder,
                    band,
                    bitplane,
                    flags,
                    magnitudes,
                    width,
                    x,
                    k + ci,
                    vertical_causal,
                );
            }
        }
    }
    if rem > 0 {
        let k = full_stripes * 4;
        for x in 0..width {
            for ci in 0..rem {
                sigpass_step(
                    decoder,
                    band,
                    bitplane,
                    flags,
                    magnitudes,
                    width,
                    x,
                    k + ci,
                    vertical_causal,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sigpass_step(
    decoder: &mut MqDecoder<'_>,
    band: BandOrientation,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    x: usize,
    y: usize,
    vertical_causal: bool,
) {
    if flags.is_significant(x, y) {
        return;
    }
    let neighbour_mask = neighbour_mask(flags, x, y, vertical_causal);
    if neighbour_mask == 0 {
        return;
    }
    let ctx = zero_coding_context(band, neighbour_mask);
    let plane_bit = decoder.decode_with_ctx(ctx);
    flags.mark_visited(x, y);
    if plane_bit != 0 {
        decode_new_significant(
            decoder,
            bitplane,
            flags,
            magnitudes,
            width,
            x,
            y,
            vertical_causal,
        );
    }
}

/// Raw significance pass (D.6): the same traversal and membership test as
/// [`sigpass_decode`], with the significance bit and then the sign read as
/// plain bits, no context and no sign prediction.
fn sigpass_decode_raw(
    raw: &mut RawDecoder<'_>,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    height: usize,
    vertical_causal: bool,
) {
    let one = 1i32 << bitplane;
    let mut step = |x: usize, y: usize, flags: &mut FlagGrid, magnitudes: &mut [i32]| {
        if flags.is_significant(x, y) || neighbour_mask(flags, x, y, vertical_causal) == 0 {
            return;
        }
        flags.mark_visited(x, y);
        if raw.decode_bit() != 0 {
            let sign_bit = raw.decode_bit();
            magnitudes[y * width + x] = one | (one >> 1);
            flags.mark_significant(x, y, sign_bit);
        }
    };
    let full_stripes = height / 4;
    for k in (0..full_stripes * 4).step_by(4) {
        for x in 0..width {
            for ci in 0..4 {
                step(x, k + ci, flags, magnitudes);
            }
        }
    }
    let rem = height % 4;
    if rem > 0 {
        let k = full_stripes * 4;
        for x in 0..width {
            for ci in 0..rem {
                step(x, k + ci, flags, magnitudes);
            }
        }
    }
}

/// Raw magnitude refinement pass (D.6): [`refpass_decode`] with the
/// refinement bit read as a plain bit.
fn refpass_decode_raw(
    raw: &mut RawDecoder<'_>,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    height: usize,
) {
    let one = 1i32 << bitplane;
    let half = one >> 1;
    let mut step = |x: usize, y: usize, flags: &mut FlagGrid, magnitudes: &mut [i32]| {
        if !flags.is_significant(x, y) || flags.is_visited(x, y) {
            return;
        }
        let idx = y * width + x;
        if raw.decode_bit() != 0 {
            magnitudes[idx] += half;
        } else {
            magnitudes[idx] -= one - half;
        }
        flags.mark_refined(x, y);
    };
    let full_stripes = height / 4;
    for k in (0..full_stripes * 4).step_by(4) {
        for x in 0..width {
            for ci in 0..4 {
                step(x, k + ci, flags, magnitudes);
            }
        }
    }
    let rem = height % 4;
    if rem > 0 {
        let k = full_stripes * 4;
        for x in 0..width {
            for ci in 0..rem {
                step(x, k + ci, flags, magnitudes);
            }
        }
    }
}

fn refpass_decode(
    decoder: &mut MqDecoder<'_>,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    height: usize,
    vertical_causal: bool,
) {
    let full_stripes = height / 4;
    let rem = height % 4;

    for k in (0..full_stripes * 4).step_by(4) {
        for x in 0..width {
            for ci in 0..4 {
                refpass_step(
                    decoder,
                    bitplane,
                    flags,
                    magnitudes,
                    width,
                    x,
                    k + ci,
                    vertical_causal,
                );
            }
        }
    }
    if rem > 0 {
        let k = full_stripes * 4;
        for x in 0..width {
            for ci in 0..rem {
                refpass_step(
                    decoder,
                    bitplane,
                    flags,
                    magnitudes,
                    width,
                    x,
                    k + ci,
                    vertical_causal,
                );
            }
        }
    }
}

fn refpass_step(
    decoder: &mut MqDecoder<'_>,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    x: usize,
    y: usize,
    vertical_causal: bool,
) {
    if !flags.is_significant(x, y) || flags.is_visited(x, y) {
        return;
    }
    let has_sig_neighbor = neighbour_mask(flags, x, y, vertical_causal) != 0;
    let ctx = magnitude_context(has_sig_neighbor, flags.has_refinement_history(x, y));
    let plane_bit = decoder.decode_with_ctx(ctx);
    // Midpoint update. Entering MR at plane b the magnitude carries a 2^b
    // offset (midpoint of the previous uncertainty interval [0, 2^(b+1))).
    // Learning bit b halves the interval: offset becomes 2^(b-1).
    //   bit=1: known += 2^b            → mag += 2^(b-1)
    //   bit=0: known unchanged         → mag -= 2^b - 2^(b-1)
    // At b=0 this reduces to bit=1: exact, bit=0: mag -= 1 (exact).
    let one = 1i32 << bitplane;
    let half = one >> 1;
    let idx = y * width + x;
    if plane_bit != 0 {
        magnitudes[idx] += half;
    } else {
        magnitudes[idx] -= one - half;
    }
    flags.mark_refined(x, y);
}

fn cleanup_decode(
    decoder: &mut MqDecoder<'_>,
    band: BandOrientation,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    height: usize,
    vertical_causal: bool,
) {
    let full_stripes = height / 4;
    let rem = height % 4;

    for k in (0..full_stripes * 4).step_by(4) {
        for x in 0..width {
            cleanup_stripe(
                decoder,
                band,
                bitplane,
                flags,
                magnitudes,
                width,
                x,
                k,
                4,
                vertical_causal,
            );
        }
    }
    if rem > 0 {
        let k = full_stripes * 4;
        for x in 0..width {
            for ci in 0..rem {
                cleanup_sample_regular(
                    decoder,
                    band,
                    bitplane,
                    flags,
                    magnitudes,
                    width,
                    x,
                    k + ci,
                    vertical_causal,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cleanup_stripe(
    decoder: &mut MqDecoder<'_>,
    band: BandOrientation,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    x: usize,
    k: usize,
    lim: usize,
    vertical_causal: bool,
) {
    debug_assert_eq!(lim, 4);
    if flags.stripe_is_clean_with_causal(x, k, lim, vertical_causal) {
        if decoder.decode_with_ctx(T1_CTXNO_AGG) == 0 {
            for ci in 0..lim {
                flags.clear_visited(x, k + ci);
            }
            return;
        }

        let hi = decoder.decode_with_ctx(T1_CTXNO_UNI);
        let lo = decoder.decode_with_ctx(T1_CTXNO_UNI);
        let runlen = ((hi << 1) | lo) as usize;
        let y = k + runlen;
        decode_new_significant(
            decoder,
            bitplane,
            flags,
            magnitudes,
            width,
            x,
            y,
            vertical_causal,
        );
        flags.clear_visited(x, y);

        for ci in (runlen + 1)..lim {
            cleanup_sample_regular(
                decoder,
                band,
                bitplane,
                flags,
                magnitudes,
                width,
                x,
                k + ci,
                vertical_causal,
            );
        }
    } else {
        for ci in 0..lim {
            cleanup_sample_regular(
                decoder,
                band,
                bitplane,
                flags,
                magnitudes,
                width,
                x,
                k + ci,
                vertical_causal,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cleanup_sample_regular(
    decoder: &mut MqDecoder<'_>,
    band: BandOrientation,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    x: usize,
    y: usize,
    vertical_causal: bool,
) {
    let visited = flags.is_visited(x, y);
    let significant = flags.is_significant(x, y);
    if significant || visited {
        flags.clear_visited(x, y);
        return;
    }

    let neighbour_mask = neighbour_mask(flags, x, y, vertical_causal);
    let ctx = if neighbour_mask != 0 {
        zero_coding_context(band, neighbour_mask)
    } else {
        T1_CTXNO_ZC
    };
    let plane_bit = decoder.decode_with_ctx(ctx);
    flags.clear_visited(x, y);
    if plane_bit != 0 {
        decode_new_significant(
            decoder,
            bitplane,
            flags,
            magnitudes,
            width,
            x,
            y,
            vertical_causal,
        );
    }
}

fn decode_new_significant(
    decoder: &mut MqDecoder<'_>,
    bitplane: u8,
    flags: &mut FlagGrid,
    magnitudes: &mut [i32],
    width: usize,
    x: usize,
    y: usize,
    vertical_causal: bool,
) {
    let (sign_lut_index, _) = if vertical_causal && y % 4 == 3 {
        flags.cardinal_sign_context_without_south(x, y)
    } else {
        flags.cardinal_sign_context(x, y)
    };
    let sign_ctx = sign_context(sign_lut_index);
    let prediction = sign_prediction_bit(sign_lut_index);
    let sign_bit = decoder.decode_with_ctx(sign_ctx) ^ prediction;
    let idx = y * width + x;
    // Midpoint reconstruction: a newly significant coefficient at plane b is
    // known to lie in [2^b, 2^(b+1)); reconstruct at 1.5·2^b so truncated
    // streams (bit-planes below b never decoded) land mid-interval. Later MR
    // passes shrink the offset; a fully decoded block ends up bit-exact.
    let one = 1i32 << bitplane;
    magnitudes[idx] = one | (one >> 1);
    flags.mark_significant(x, y, sign_bit);
}

#[inline]
fn neighbour_mask(flags: &FlagGrid, x: usize, y: usize, vertical_causal: bool) -> u32 {
    let mask = flags.neighbour_mask(x, y);
    if vertical_causal && y % 4 == 3 {
        mask & !(T1_SIGMA_S | T1_SIGMA_SW | T1_SIGMA_SE)
    } else {
        mask
    }
}
