use crate::dwt::norms::reversible_exponent;
#[cfg(feature = "counters")]
use crate::encode::counters;
use crate::encode::profile_enter;
use crate::j2k::decode_markers::CodeBlockStyle;
use crate::mq::{MqCoder, T1_CTXNO_AGG, T1_CTXNO_UNI, T1_CTXNO_ZC};
use crate::plan::BandOrientation;
use crate::profile::{BlockClass, classify_from_nonzero_fraction};
use crate::simd::PRIMITIVES;
use crate::tier1::consts::{T1_SIGMA_S, T1_SIGMA_SE, T1_SIGMA_SW};
use crate::tier1::flags::FlagGrid;
use crate::tier1::helpers::{
    magnitude_context, sign_context, sign_prediction_bit, zero_coding_context,
};

use rayon::prelude::*;

/// Maximum bitplane count signalled for a subband in the codestream.
/// Per ISO 15444-1 Annex E: Mb(b) = guard_bits - 1 + expn(b). For reversible
/// 5/3 with precision P, expn(b) = P + band_gain(b).
pub(crate) fn band_max_bitplanes(precision: u32, guard_bits: u8, band: BandOrientation) -> u8 {
    let expn = reversible_exponent(precision, band);
    guard_bits.saturating_sub(1).saturating_add(expn)
}

use super::NativeComponentCoefficients;
#[cfg(test)]
use super::layout::{NativeCodeBlock, NativeComponentLayout};
use crate::plan::CodeBlockSize;
use crate::tiling::{CodeBlockRect, TileComponentRect, codeblock_rects_for_subband, subband_rects};

// ---------------------------------------------------------------------------
// Public data types (unchanged)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeTier1CodeBlock {
    pub tile_index: u16,
    pub resolution: u8,
    pub band: BandOrientation,
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
    pub width: usize,
    pub height: usize,
    pub coefficient_count: usize,
    pub max_magnitude: u32,
    pub magnitude_bitplanes: u8,
    pub zero_bitplanes: u8,
    /// Fraction of quantized coefficients that are non-zero (0.0–1.0).
    pub nonzero_fraction: f32,
    pub coding_passes: Vec<NativeTier1Pass>,
    pub coefficients: Vec<i32>,
}

/// Superseded, along with [`NativeTier1Layout`], by the tile-aware
/// non-grouped encode path (`encode_component_coefficients_for_tile_with_max_bitplanes`).
/// Kept under `#[cfg(test)]`; see `native::layout`'s module doc comment.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeTier1Band {
    pub resolution: u8,
    pub band: BandOrientation,
    pub blocks: Vec<NativeTier1CodeBlock>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeTier1Layout {
    pub bands: Vec<NativeTier1Band>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeEncodedTier1CodeBlock {
    pub tile_index: u16,
    pub resolution: u8,
    pub band: BandOrientation,
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
    pub magnitude_bitplanes: u8,
    pub zero_bitplanes: u8,
    pub block_class: BlockClass,
    pub payload: Vec<u8>,
    pub passes: Vec<NativeEncodedTier1Pass>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeEncodedTier1Band {
    pub resolution: u8,
    pub band: BandOrientation,
    pub blocks: Vec<NativeEncodedTier1CodeBlock>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeEncodedTier1Layout {
    pub bands: Vec<NativeEncodedTier1Band>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeTier1SelectionBand {
    pub resolution: u8,
    pub band: BandOrientation,
    pub selected_passes: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeTier1SelectionLayout {
    pub bands: Vec<NativeTier1SelectionBand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeTier1PassKind {
    SignificancePropagation,
    MagnitudeRefinement,
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeTier1PassTermination {
    /// Plain FLUSH termination (C.3.5).
    TermAll,
    /// Predictable termination (D.4.2 ERTERM), selected by
    /// [`CodeBlockStyle::predictable_termination`].
    ErTerm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeTier1PassCodingMode {
    Mq,
    /// Bypass (raw) coding of lower bit-planes (ISO/IEC 15444-1 Annex D.6),
    /// selected by [`CodeBlockStyle::bypass`].
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeTier1Pass {
    pub kind: NativeTier1PassKind,
    pub bitplane: u8,
    pub pass_index: u16,
    pub coding_mode: NativeTier1PassCodingMode,
    pub termination: NativeTier1PassTermination,
    pub segmark: bool,
    pub significant_before: usize,
    pub newly_significant: usize,
    pub significant_after: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeEncodedTier1Pass {
    pub kind: NativeTier1PassKind,
    pub bitplane: u8,
    pub pass_index: u16,
    pub coding_mode: NativeTier1PassCodingMode,
    pub termination: NativeTier1PassTermination,
    pub segmark: bool,
    /// True when this pass is followed by an MQ termination, starting a new
    /// length-signaling segment in the packet header.
    pub is_terminated: bool,
    pub newly_significant: usize,
    /// Number of already-significant coefficients present when this pass ran.
    /// Non-zero only for MagnitudeRefinement: these are the samples the MR pass
    /// actually refines, giving them non-zero distortion credit in PCRD.
    pub significant_before: usize,
    pub length: usize,
    pub cumulative_length: usize,
    pub distortion_hint: u32,
    /// Annex J ΔMSE numerator in quantized-coefficient units (pre-scaling by
    /// quant_step² × subband_weight). Computed from actual coefficient values;
    /// rate.rs scales this to image-domain MSE for PCRD slope calculation.
    pub mse_numerator: f64,
}

// ---------------------------------------------------------------------------
// Analysis phase (entry point)
// ---------------------------------------------------------------------------

/// Default 8-bit/2-guard-bit analysis preset. Only exercised by tests; the
/// production encode path always calls [`analyze_component_layout_with`] or
/// [`analyze_component_layout_with_max_bitplanes`] with explicit precision.
#[cfg(test)]
pub(crate) fn analyze_component_layout(layout: &NativeComponentLayout) -> NativeTier1Layout {
    analyze_component_layout_with(layout, 8, 2)
}

#[cfg(test)]
pub(crate) fn analyze_component_layout_with_max_bitplanes<F>(
    layout: &NativeComponentLayout,
    mut max_bitplanes_for_subband: F,
) -> NativeTier1Layout
where
    F: FnMut(u8, BandOrientation) -> u8,
{
    let mut bands = Vec::with_capacity(layout.subbands.len());
    let mut jobs = Vec::new();

    for (band_index, subband) in layout.subbands.iter().enumerate() {
        let mb = max_bitplanes_for_subband(subband.resolution, subband.band);
        bands.push(NativeTier1Band {
            resolution: subband.resolution,
            band: subband.band,
            blocks: Vec::with_capacity(subband.codeblocks.len()),
        });
        jobs.extend(
            subband
                .codeblocks
                .iter()
                .map(|block| (band_index, subband.resolution, subband.band, mb, block)),
        );
    }

    let analyzed: Vec<_> = jobs
        .par_iter()
        .map(|&(band_index, resolution, band, mb, block)| {
            (band_index, analyze_codeblock(resolution, band, mb, block))
        })
        .collect();

    for (band_index, block) in analyzed {
        bands[band_index].blocks.push(block);
    }

    NativeTier1Layout { bands }
}

#[cfg(test)]
pub(crate) fn analyze_component_layout_with(
    layout: &NativeComponentLayout,
    precision: u32,
    guard_bits: u8,
) -> NativeTier1Layout {
    analyze_component_layout_with_max_bitplanes(layout, |_, band| {
        band_max_bitplanes(precision, guard_bits, band)
    })
}

#[cfg(test)]
pub(crate) fn encode_component_coefficients_with_max_bitplanes<F>(
    coefficients: &NativeComponentCoefficients,
    component_index: u16,
    code_block_size: CodeBlockSize,
    max_bitplanes_for_subband: F,
) -> NativeEncodedTier1Layout
where
    F: FnMut(u8, BandOrientation) -> u8,
{
    encode_component_coefficients_for_tile_with_max_bitplanes(
        coefficients,
        0,
        component_index,
        code_block_size,
        CodeBlockStyle::default(),
        max_bitplanes_for_subband,
    )
}

pub(crate) fn encode_component_coefficients_for_tile_with_max_bitplanes<F>(
    coefficients: &NativeComponentCoefficients,
    tile_index: u16,
    component_index: u16,
    code_block_size: CodeBlockSize,
    style: CodeBlockStyle,
    mut max_bitplanes_for_subband: F,
) -> NativeEncodedTier1Layout
where
    F: FnMut(u8, BandOrientation) -> u8,
{
    let _p = profile_enter("t1::encode_component_coefficients_with_max_bitplanes");

    let tc = TileComponentRect {
        tile_index,
        component: component_index,
        x0: coefficients.x0 as u32,
        y0: coefficients.y0 as u32,
        x1: (coefficients.x0 + coefficients.width) as u32,
        y1: (coefficients.y0 + coefficients.height) as u32,
    };
    // `subband_rects` applies the reference-grid phase, then translate its
    // rectangles into the packed tile-local coefficient plane used by Tier-1.
    let mut subbands = subband_rects(&tc, coefficients.levels);
    for subband in &mut subbands {
        subband.x0 -= tc.x0;
        subband.x1 -= tc.x0;
        subband.y0 -= tc.y0;
        subband.y1 -= tc.y0;
    }
    let mut next_block_id = 0usize;

    let bands = subbands
        .into_iter()
        .map(|subband| {
            let max_bitplanes = max_bitplanes_for_subband(subband.resolution, subband.band);
            let blocks = codeblock_rects_for_subband(&subband, code_block_size, &mut next_block_id);

            #[cfg(feature = "counters")]
            counters::TOTAL_BLOCKS
                .fetch_add(blocks.len() as u64, std::sync::atomic::Ordering::Relaxed);

            let encoded_blocks = blocks
                .par_iter()
                .map_init(Vec::<i32>::new, |worker_scratch, block| {
                    pack_codeblock_coefficients_into(coefficients, block, worker_scratch);
                    let scratch = std::mem::take(worker_scratch);
                    let analyzed = analyze_codeblock_coefficients(
                        block.tile_index,
                        block.resolution,
                        block.band,
                        max_bitplanes,
                        block.x0 as usize,
                        block.y0 as usize,
                        block.x1 as usize,
                        block.y1 as usize,
                        scratch,
                    );
                    let encoded = encode_codeblock_styled(&analyzed, style);
                    *worker_scratch = analyzed.coefficients;
                    worker_scratch.clear();
                    encoded
                })
                .collect();

            NativeEncodedTier1Band {
                resolution: subband.resolution,
                band: subband.band,
                blocks: encoded_blocks,
            }
        })
        .collect();

    NativeEncodedTier1Layout { bands }
}

#[cfg(test)]
pub(crate) fn encode_component_coefficients_with(
    coefficients: &NativeComponentCoefficients,
    component_index: u16,
    code_block_size: CodeBlockSize,
    precision: u32,
    guard_bits: u8,
) -> NativeEncodedTier1Layout {
    encode_component_coefficients_with_max_bitplanes(
        coefficients,
        component_index,
        code_block_size,
        |_, band| band_max_bitplanes(precision, guard_bits, band),
    )
}

fn pack_codeblock_coefficients_into(
    coefficients: &NativeComponentCoefficients,
    block: &CodeBlockRect,
    scratch: &mut Vec<i32>,
) {
    let x0 = block.x0 as usize;
    let y0 = block.y0 as usize;
    let x1 = block.x1 as usize;
    let y1 = block.y1 as usize;
    scratch.clear();
    scratch.reserve((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        let row = y * coefficients.width;
        scratch.extend_from_slice(&coefficients.data[row + x0..row + x1]);
    }
}

#[cfg(test)]
fn analyze_codeblock(
    resolution: u8,
    band: BandOrientation,
    mb: u8,
    block: &NativeCodeBlock,
) -> NativeTier1CodeBlock {
    analyze_codeblock_coefficients(
        0,
        resolution,
        band,
        mb,
        block.x0,
        block.y0,
        block.x1,
        block.y1,
        block.coefficients.clone(),
    )
}

#[allow(clippy::too_many_arguments)]
fn analyze_codeblock_coefficients(
    tile_index: u16,
    resolution: u8,
    band: BandOrientation,
    mb: u8,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    coefficients: Vec<i32>,
) -> NativeTier1CodeBlock {
    let width = x1 - x0;
    let height = y1 - y0;

    // Fuse max_magnitude and nonzero_count into a single primitive scan.
    let (max_magnitude, nonzero_count) =
        (PRIMITIVES.analyze.i32_max_magnitude_and_nnz)(&coefficients);

    let magnitude_bitplanes = if max_magnitude == 0 {
        0
    } else {
        (u32::BITS - max_magnitude.leading_zeros()) as u8
    };
    let zero_bitplanes = mb.saturating_sub(magnitude_bitplanes);
    let coding_passes = plan_coding_passes(&coefficients, magnitude_bitplanes);
    let nonzero_fraction = if coefficients.is_empty() {
        0.0f32
    } else {
        nonzero_count as f32 / coefficients.len() as f32
    };

    NativeTier1CodeBlock {
        tile_index,
        resolution,
        band,
        x0,
        y0,
        x1,
        y1,
        width,
        height,
        coefficient_count: coefficients.len(),
        max_magnitude,
        magnitude_bitplanes,
        zero_bitplanes,
        nonzero_fraction,
        coding_passes,
        coefficients,
    }
}

// ---------------------------------------------------------------------------
// Encoding phase (entry point)
// ---------------------------------------------------------------------------

/// Encodes with the default code-block style. Only exercised by tests; the
/// production encode path always goes through
/// [`encode_component_coefficients_for_tile_with_max_bitplanes`].
#[cfg(test)]
pub(crate) fn encode_placeholder_tier1(layout: &NativeTier1Layout) -> NativeEncodedTier1Layout {
    let _p = profile_enter("t1::encode_placeholder_tier1");

    // Count bands first for statistics
    #[cfg(feature = "counters")]
    {
        let band_count: usize = layout.bands.iter().map(|b| b.blocks.len()).sum();
        counters::TOTAL_BLOCKS.fetch_add(band_count as u64, std::sync::atomic::Ordering::Relaxed);
    }

    let mut bands = Vec::with_capacity(layout.bands.len());
    let mut jobs = Vec::new();

    for (band_index, band) in layout.bands.iter().enumerate() {
        bands.push(NativeEncodedTier1Band {
            resolution: band.resolution,
            band: band.band,
            blocks: Vec::with_capacity(band.blocks.len()),
        });
        jobs.extend(band.blocks.iter().map(|block| (band_index, block)));
    }

    let encoded: Vec<_> = jobs
        .par_iter()
        .map(|&(band_index, block)| {
            (
                band_index,
                encode_codeblock_styled(block, CodeBlockStyle::default()),
            )
        })
        .collect();

    for (band_index, block) in encoded {
        bands[band_index].blocks.push(block);
    }

    NativeEncodedTier1Layout { bands }
}

#[cfg(test)]
fn encode_band_styled(band: &NativeTier1Band, style: CodeBlockStyle) -> NativeEncodedTier1Band {
    let blocks = band
        .blocks
        .par_iter()
        .map(|block| encode_codeblock_styled(block, style))
        .collect();

    NativeEncodedTier1Band {
        resolution: band.resolution,
        band: band.band,
        blocks,
    }
}

/// Encode one code-block under `style` (the Annex D switches).
///
/// The all-off style takes [`encode_codeblock_single_term`], the tuned
/// single-segment path. Any switch takes the general loop here, which decides
/// per pass whether it is MQ or raw coded and whether the codeword
/// terminates after it (Table D.9), resets contexts, and appends
/// segmentation symbols. Context states persist across segments; only the
/// coder registers and output restart at a termination.
pub(crate) fn encode_codeblock_styled(
    block: &NativeTier1CodeBlock,
    style: CodeBlockStyle,
) -> NativeEncodedTier1CodeBlock {
    let sample_count = block.width.saturating_mul(block.height);
    crate::encode::counters::record_codeblock_worker(
        sample_count
            .saturating_mul(std::mem::size_of::<i32>() + std::mem::size_of::<u16>())
            .saturating_add(sample_count / 2 + 64)
            .saturating_add(
                block.coding_passes.len() * std::mem::size_of::<NativeEncodedTier1Pass>(),
            ),
    );
    if block.magnitude_bitplanes == 0 {
        #[cfg(feature = "counters")]
        counters::EMPTY_BLOCKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return NativeEncodedTier1CodeBlock {
            tile_index: block.tile_index,
            resolution: block.resolution,
            band: block.band,
            x0: block.x0,
            y0: block.y0,
            x1: block.x1,
            y1: block.y1,
            magnitude_bitplanes: 0,
            zero_bitplanes: block.zero_bitplanes,
            block_class: BlockClass::Flat,
            payload: Vec::new(),
            passes: Vec::new(),
        };
    }

    if style == CodeBlockStyle::default() {
        return encode_codeblock_single_term(block);
    }

    let causal = style.vertical_causal;
    let top = block.magnitude_bitplanes - 1;
    let mut schedule = Vec::with_capacity(1 + 3 * usize::from(top));
    schedule.push((NativeTier1PassKind::Cleanup, top));
    for bitplane in (0..top).rev() {
        schedule.push((NativeTier1PassKind::SignificancePropagation, bitplane));
        schedule.push((NativeTier1PassKind::MagnitudeRefinement, bitplane));
        schedule.push((NativeTier1PassKind::Cleanup, bitplane));
    }

    let estimated_capacity = (block.width * block.height) / 2 + 64;
    let mut coder = MqCoder::with_capacity(estimated_capacity);
    let mut flags = FlagGrid::new(block.width, block.height);
    let mut passes: Vec<NativeEncodedTier1Pass> = Vec::with_capacity(schedule.len());
    let mut payload = Vec::new();
    // Passes of the codeword segment still open: (pass slot, bytes so far).
    let mut open_segment: Vec<(usize, usize)> = Vec::new();
    let mut significant_so_far = 0usize;

    for (pass_index, &(kind, bitplane)) in schedule.iter().enumerate() {
        let index = pass_index as u32;
        let raw = style.pass_is_raw(index);
        let terminated = pass_index + 1 == schedule.len() || style.pass_is_terminated(index);
        let mode = if raw {
            NativeTier1PassCodingMode::Raw
        } else {
            NativeTier1PassCodingMode::Mq
        };
        if raw && open_segment.is_empty() {
            coder.bypass_init();
        }

        let significant_before = significant_so_far;
        let (newly_significant, mse_numerator) = match kind {
            NativeTier1PassKind::SignificancePropagation => {
                sigpass_encode(&mut coder, block, bitplane, &mut flags, mode, causal)
            }
            NativeTier1PassKind::MagnitudeRefinement => (
                0,
                refpass_encode_mut(&mut coder, block, bitplane, &mut flags, mode, causal),
            ),
            NativeTier1PassKind::Cleanup => {
                let result = cleanup_encode(&mut coder, block, bitplane, &mut flags, causal);
                if style.segmentation_symbols {
                    coder.segmark_encode();
                }
                clear_visited_all(&mut flags, block.width, block.height);
                result
            }
        };
        significant_so_far += newly_significant;
        if style.reset_contexts {
            coder.reset_contexts();
        }

        #[cfg(feature = "counters")]
        match kind {
            NativeTier1PassKind::Cleanup => {
                counters::CLEANUP_PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            NativeTier1PassKind::SignificancePropagation => {
                counters::SP_PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            NativeTier1PassKind::MagnitudeRefinement => {
                counters::MR_PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        passes.push(NativeEncodedTier1Pass {
            kind,
            bitplane,
            pass_index: index as u16,
            coding_mode: mode,
            termination: if style.predictable_termination {
                NativeTier1PassTermination::ErTerm
            } else {
                NativeTier1PassTermination::TermAll
            },
            segmark: style.segmentation_symbols && kind == NativeTier1PassKind::Cleanup,
            is_terminated: terminated,
            newly_significant,
            significant_before,
            length: 0,
            cumulative_length: 0,
            distortion_hint: distortion_hint(bitplane, newly_significant, block.max_magnitude),
            mse_numerator,
        });
        // At a pass boundary inside an open segment the coder still holds
        // the last symbols in its registers (and the byte at the write
        // position may take a carry), so the bytes a decoder needs to
        // reproduce this pass end up to three bytes past the count; the
        // segment's last pass takes the exact remainder (`close_segment`).
        let pending = if terminated {
            0
        } else {
            match mode {
                NativeTier1PassCodingMode::Mq => MQ_PASS_PENDING_BYTES,
                NativeTier1PassCodingMode::Raw => RAW_PASS_PENDING_BYTES,
            }
        };
        open_segment.push((passes.len() - 1, coder.numbytes() + pending));

        if terminated {
            let bytes = match mode {
                NativeTier1PassCodingMode::Mq => coder.terminate(style.predictable_termination),
                NativeTier1PassCodingMode::Raw => {
                    coder.raw_term_flush_and_restart(style.predictable_termination)
                }
            };
            close_segment(&mut passes, &mut open_segment, bytes.len(), payload.len());
            payload.extend_from_slice(&bytes);
        }
    }
    debug_assert!(open_segment.is_empty());
    #[cfg(feature = "counters")]
    counters::TOTAL_PASS_BYTES
        .fetch_add(payload.len() as u64, std::sync::atomic::Ordering::Relaxed);

    NativeEncodedTier1CodeBlock {
        tile_index: block.tile_index,
        resolution: block.resolution,
        band: block.band,
        x0: block.x0,
        y0: block.y0,
        x1: block.x1,
        y1: block.y1,
        magnitude_bitplanes: block.magnitude_bitplanes,
        zero_bitplanes: block.zero_bitplanes,
        block_class: classify_from_nonzero_fraction(block.nonzero_fraction),
        payload,
        passes,
    }
}

/// Bytes an MQ coder may still owe for the symbols of a pass at a boundary
/// without termination: the byte at the write position plus the two bytes
/// of the code register window (OpenJPEG uses the same correction).
const MQ_PASS_PENDING_BYTES: usize = 3;
/// The raw (bypass) coder holds at most one partial byte, plus a stuffed
/// byte after 0xff.
const RAW_PASS_PENDING_BYTES: usize = 2;

/// Give the passes of a just-terminated codeword segment their byte lengths:
/// the coder's running byte count at each pass, with the final pass absorbing
/// the termination bytes, so the lengths sum to the segment exactly and
/// `cumulative_length` keeps counting across the whole block payload.
fn close_segment(
    passes: &mut [NativeEncodedTier1Pass],
    open_segment: &mut Vec<(usize, usize)>,
    segment_bytes: usize,
    payload_before: usize,
) {
    let last = open_segment.len().saturating_sub(1);
    let mut assigned = 0usize;
    let mut previous = 0usize;
    for (position, &(slot, snapshot)) in open_segment.iter().enumerate() {
        let length = if position == last {
            segment_bytes - assigned
        } else {
            let capped = snapshot.min(segment_bytes);
            let length = capped.saturating_sub(previous);
            previous = capped;
            length
        };
        assigned += length;
        passes[slot].length = length;
        passes[slot].cumulative_length = payload_before + assigned;
    }
    open_segment.clear();
}

/// Default JPEG 2000 Part-1 codeblock coding: a single MQ-terminated segment
/// per codeblock, with per-pass length snapshots recorded mid-stream (no
/// per-pass restart). Matches OpenJPEG's default (non-TERMALL) behavior.
fn encode_codeblock_single_term(block: &NativeTier1CodeBlock) -> NativeEncodedTier1CodeBlock {
    // Pre-allocate MQ output buffer based on codeblock size
    let estimated_capacity = (block.width * block.height) / 2 + 64;
    let mut coder = MqCoder::with_capacity(estimated_capacity);
    let mut flags = FlagGrid::new(block.width, block.height);
    // (kind, bitplane, newly_significant, snapshot_after, significant_before, mse_numerator)
    let mut metas: Vec<(NativeTier1PassKind, u8, usize, usize, usize, f64)> =
        Vec::with_capacity(block.coding_passes.len());
    let mut significant_so_far = 0usize;

    let top = block.magnitude_bitplanes - 1;

    // Most-significant bitplane: cleanup only, no SP/MR.
    let (ns, cl_mse) = cleanup_encode(&mut coder, block, top, &mut flags, false);
    metas.push((
        NativeTier1PassKind::Cleanup,
        top,
        ns,
        coder.numbytes(),
        0,
        cl_mse,
    ));
    significant_so_far += ns;
    clear_visited_all(&mut flags, block.width, block.height);

    for bitplane in (0..top).rev() {
        let significant_before_sp = significant_so_far;

        let (sp_ns, sp_mse) = sigpass_encode(
            &mut coder,
            block,
            bitplane,
            &mut flags,
            NativeTier1PassCodingMode::Mq,
            false,
        );
        metas.push((
            NativeTier1PassKind::SignificancePropagation,
            bitplane,
            sp_ns,
            coder.numbytes(),
            significant_before_sp,
            sp_mse,
        ));
        significant_so_far += sp_ns;

        let mr_mse = refpass_encode_mut(
            &mut coder,
            block,
            bitplane,
            &mut flags,
            NativeTier1PassCodingMode::Mq,
            false,
        );
        metas.push((
            NativeTier1PassKind::MagnitudeRefinement,
            bitplane,
            0,
            coder.numbytes(),
            significant_before_sp,
            mr_mse,
        ));

        let (cl_ns, cl_mse) = cleanup_encode(&mut coder, block, bitplane, &mut flags, false);
        metas.push((
            NativeTier1PassKind::Cleanup,
            bitplane,
            cl_ns,
            coder.numbytes(),
            significant_so_far,
            cl_mse,
        ));
        significant_so_far += cl_ns;

        clear_visited_all(&mut flags, block.width, block.height);
    }

    // Single termination at end of codeblock.
    let terminated = coder.flush_and_restart();
    let total = terminated.len();

    let mut passes: Vec<NativeEncodedTier1Pass> = Vec::with_capacity(metas.len());
    let mut cumulative = 0usize;
    let mut prev_snap = 0usize;
    let mut sum_lengths = 0usize;
    let last_idx = metas.len() - 1;

    for (i, (kind, bitplane, ns, snap, sb, mse)) in metas.iter().copied().enumerate() {
        let mut length = snap.saturating_sub(prev_snap);
        prev_snap = snap;
        if i == last_idx {
            // Absorb termination overhead / trimming discrepancy.
            length = total.saturating_sub(sum_lengths);
        }
        sum_lengths += length;
        cumulative += length;

        passes.push(NativeEncodedTier1Pass {
            kind,
            bitplane,
            pass_index: i as u16,
            coding_mode: NativeTier1PassCodingMode::Mq,
            termination: NativeTier1PassTermination::TermAll,
            segmark: false,
            is_terminated: i == last_idx,
            newly_significant: ns,
            significant_before: sb,
            length,
            cumulative_length: cumulative,
            distortion_hint: distortion_hint(bitplane, ns, block.max_magnitude),
            mse_numerator: mse,
        });

        #[cfg(feature = "counters")]
        {
            #[allow(unreachable_patterns)]
            match kind {
                NativeTier1PassKind::Cleanup => {
                    counters::CLEANUP_PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                NativeTier1PassKind::SignificancePropagation => {
                    counters::SP_PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                NativeTier1PassKind::MagnitudeRefinement => {
                    counters::MR_PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                _ => {}
            }
            counters::TOTAL_PASS_BYTES
                .fetch_add(length as u64, std::sync::atomic::Ordering::Relaxed);
        }
    }

    NativeEncodedTier1CodeBlock {
        tile_index: block.tile_index,
        resolution: block.resolution,
        band: block.band,
        x0: block.x0,
        y0: block.y0,
        x1: block.x1,
        y1: block.y1,
        magnitude_bitplanes: block.magnitude_bitplanes,
        zero_bitplanes: block.zero_bitplanes,
        block_class: classify_from_nonzero_fraction(block.nonzero_fraction),
        payload: terminated,
        passes,
    }
}

// ---------------------------------------------------------------------------
// Significance Propagation pass
//
// Traversal: column-major within 4-row stripes (x outer, ci=0..4 inner).
// Processes samples that are NOT yet significant AND have at least one
// significant neighbour.  Marks processed samples as visited (PI).
// ---------------------------------------------------------------------------

fn sigpass_encode(
    coder: &mut MqCoder,
    block: &NativeTier1CodeBlock,
    bitplane: u8,
    flags: &mut FlagGrid,
    mode: NativeTier1PassCodingMode,
    causal: bool,
) -> (usize, f64) {
    let w = block.width;
    let h = block.height;
    let one = 1u32 << bitplane;
    let mut newly_significant = 0usize;
    let mut mse_num = 0.0f64;

    let full_stripes = h / 4;
    let rem = h % 4;

    for k in (0..full_stripes * 4).step_by(4) {
        for x in 0..w {
            for ci in 0..4usize {
                let y = k + ci;
                sigpass_step(
                    coder,
                    block,
                    x,
                    y,
                    w,
                    one,
                    flags,
                    &mut newly_significant,
                    &mut mse_num,
                    mode,
                    causal,
                );
            }
        }
    }
    if rem > 0 {
        let k = full_stripes * 4;
        for x in 0..w {
            for ci in 0..rem {
                let y = k + ci;
                sigpass_step(
                    coder,
                    block,
                    x,
                    y,
                    w,
                    one,
                    flags,
                    &mut newly_significant,
                    &mut mse_num,
                    mode,
                    causal,
                );
            }
        }
    }

    (newly_significant, mse_num)
}

#[inline]
fn sigpass_step(
    coder: &mut MqCoder,
    block: &NativeTier1CodeBlock,
    x: usize,
    y: usize,
    w: usize,
    one: u32,
    flags: &mut FlagGrid,
    newly_significant: &mut usize,
    mse_num: &mut f64,
    mode: NativeTier1PassCodingMode,
    causal: bool,
) {
    if flags.is_significant(x, y) {
        return;
    }
    let neighbour_mask = neighbours(flags, x, y, causal);
    if neighbour_mask == 0 {
        return;
    }
    // Sample is not significant but has a significant neighbour — encode ZC.
    let ctx = zero_coding_context(block.band, neighbour_mask);
    // SAFETY: x < w and y < height, so y*w+x is always in bounds
    let coeff = unsafe { *block.coefficients.get_unchecked(y * w + x) };
    let mag = coeff.unsigned_abs();
    let plane_bit = ((mag & one) != 0) as u8;
    encode_symbol(coder, mode, ctx, plane_bit);
    flags.mark_visited(x, y);
    if plane_bit == 1 {
        let sign_bit = (coeff < 0) as u8;
        encode_sign(coder, mode, flags, x, y, causal, sign_bit);
        flags.mark_significant(x, y, sign_bit);
        *newly_significant += 1;
        *mse_num += sig_mse_delta(mag, one);
    }
}

// ---------------------------------------------------------------------------
// Magnitude Refinement pass
//
// Processes samples that were ALREADY significant before this bitplane (i.e.,
// significant AND NOT visited by the current SP pass).
// ---------------------------------------------------------------------------

fn refpass_encode_mut(
    coder: &mut MqCoder,
    block: &NativeTier1CodeBlock,
    bitplane: u8,
    flags: &mut FlagGrid,
    mode: NativeTier1PassCodingMode,
    causal: bool,
) -> f64 {
    let w = block.width;
    let h = block.height;
    let one = 1u32 << bitplane;
    let mut mse_num = 0.0f64;

    let full_stripes = h / 4;
    let rem = h % 4;

    let mut refine = |coder: &mut MqCoder, flags: &mut FlagGrid, x: usize, y: usize| {
        if !flags.is_significant(x, y) || flags.is_visited(x, y) {
            return;
        }
        let has_sig_neighbor = neighbours(flags, x, y, causal) != 0;
        let ctx = magnitude_context(has_sig_neighbor, flags.has_refinement_history(x, y));
        // SAFETY: x < w and y < height, so y*w+x is always in bounds
        let mag = unsafe { block.coefficients.get_unchecked(y * w + x).unsigned_abs() };
        let plane_bit = ((mag & one) != 0) as u8;
        encode_symbol(coder, mode, ctx, plane_bit);
        flags.mark_refined(x, y);
        mse_num += mr_mse_delta(mag, one);
    };

    for k in (0..full_stripes * 4).step_by(4) {
        for x in 0..w {
            for ci in 0..4usize {
                refine(coder, flags, x, k + ci);
            }
        }
    }
    if rem > 0 {
        let k = full_stripes * 4;
        for x in 0..w {
            for ci in 0..rem {
                refine(coder, flags, x, k + ci);
            }
        }
    }

    mse_num
}

// ---------------------------------------------------------------------------
// Cleanup pass
//
// Traversal: column-major within 4-row stripes.
// Uses AGG run-length coding when an entire column-stripe is "clean" (no
// significant neighbours, not significant, not visited) — equivalent to the
// `*f == 0` fast path in OpenJPEG.
// Clears visited (PI) flags for every sample it visits.
// ---------------------------------------------------------------------------

fn cleanup_encode(
    coder: &mut MqCoder,
    block: &NativeTier1CodeBlock,
    bitplane: u8,
    flags: &mut FlagGrid,
    causal: bool,
) -> (usize, f64) {
    let w = block.width;
    let h = block.height;
    let one = 1u32 << bitplane;
    let mut newly_significant = 0usize;
    let mut mse_num = 0.0f64;

    let full_stripes = h / 4;
    let rem = h % 4;

    for k in (0..full_stripes * 4).step_by(4) {
        for x in 0..w {
            cleanup_stripe(
                coder,
                block,
                x,
                k,
                4,
                one,
                flags,
                &mut newly_significant,
                &mut mse_num,
                causal,
            );
        }
    }
    if rem > 0 {
        let k = full_stripes * 4;
        for x in 0..w {
            // Partial stripes never use AGG (spec §C.3.2); use regular per-sample coding.
            cleanup_stripe_partial(
                coder,
                block,
                x,
                k,
                rem,
                one,
                flags,
                &mut newly_significant,
                &mut mse_num,
                causal,
            );
        }
    }

    (newly_significant, mse_num)
}

/// Full 4-row stripe — may use AGG coding when the stripe is clean.
fn cleanup_stripe(
    coder: &mut MqCoder,
    block: &NativeTier1CodeBlock,
    x: usize,
    k: usize,
    lim: usize, // always 4 here
    one: u32,
    flags: &mut FlagGrid,
    newly_significant: &mut usize,
    mse_num: &mut f64,
    causal: bool,
) {
    debug_assert_eq!(lim, 4);
    let w = block.width;

    if flags.stripe_is_clean_with_causal(x, k, lim, causal) {
        // AGG path: encode whether any of the 4 samples becomes significant.
        let runlen = (0..lim).find(|&ci| {
            let y = k + ci;
            // SAFETY: x < w and y < height, so y*w+x is always in bounds
            let mag = unsafe { block.coefficients.get_unchecked(y * w + x).unsigned_abs() };
            (mag & one) != 0
        });

        if let Some(rl) = runlen {
            // At least one sample is significant: encode run-length position.
            coder.encode_with_ctx(T1_CTXNO_AGG, 1);
            coder.encode_with_ctx(T1_CTXNO_UNI, ((rl >> 1) & 1) as u8);
            coder.encode_with_ctx(T1_CTXNO_UNI, (rl & 1) as u8);

            // The sample at `rl` is known significant — encode its sign (partial step).
            {
                let y = k + rl;
                // SAFETY: x < w and y < height, so y*w+x is always in bounds
                let coeff = unsafe { *block.coefficients.get_unchecked(y * w + x) };
                let mag = coeff.unsigned_abs();
                let sign_bit = (coeff < 0) as u8;
                encode_sign(
                    coder,
                    NativeTier1PassCodingMode::Mq,
                    flags,
                    x,
                    y,
                    causal,
                    sign_bit,
                );
                flags.mark_significant(x, y, sign_bit);
                *newly_significant += 1;
                flags.clear_visited(x, y);
                *mse_num += sig_mse_delta(mag, one);
            }

            // Regular ZC coding for samples after the run-length position.
            for ci in (rl + 1)..lim {
                let y = k + ci;
                cleanup_sample_regular(
                    coder,
                    block,
                    x,
                    y,
                    one,
                    flags,
                    newly_significant,
                    mse_num,
                    w,
                    causal,
                );
            }
        } else {
            // All 4 samples are insignificant at this bitplane: encode runlen=4.
            coder.encode_with_ctx(T1_CTXNO_AGG, 0);
            // Clear visited flags (should already be false in a clean stripe, but
            // clearing unconditionally keeps the invariant correct).
            for ci in 0..lim {
                flags.clear_visited(x, k + ci);
            }
        }
    } else {
        // Regular per-sample coding.
        for ci in 0..lim {
            let y = k + ci;
            cleanup_sample_regular(
                coder,
                block,
                x,
                y,
                one,
                flags,
                newly_significant,
                mse_num,
                w,
                causal,
            );
        }
    }
}

/// Partial stripe at the bottom of the codeblock — always uses regular coding.
fn cleanup_stripe_partial(
    coder: &mut MqCoder,
    block: &NativeTier1CodeBlock,
    x: usize,
    k: usize,
    lim: usize,
    one: u32,
    flags: &mut FlagGrid,
    newly_significant: &mut usize,
    mse_num: &mut f64,
    causal: bool,
) {
    let w = block.width;
    for ci in 0..lim {
        let y = k + ci;
        cleanup_sample_regular(
            coder,
            block,
            x,
            y,
            one,
            flags,
            newly_significant,
            mse_num,
            w,
            causal,
        );
    }
}

/// Regular (non-AGG) cleanup step for a single sample.
/// Skips samples that are already significant OR visited (PI) by the current SP.
/// Always clears the visited flag.
#[inline]
fn cleanup_sample_regular(
    coder: &mut MqCoder,
    block: &NativeTier1CodeBlock,
    x: usize,
    y: usize,
    one: u32,
    flags: &mut FlagGrid,
    newly_significant: &mut usize,
    mse_num: &mut f64,
    w: usize,
    causal: bool,
) {
    let visited = flags.is_visited(x, y);
    let significant = flags.is_significant(x, y);

    if significant || visited {
        // Skip — already handled by MR or SP — but always clear PI.
        flags.clear_visited(x, y);
        return;
    }

    let neighbour_mask = neighbours(flags, x, y, causal);
    let ctx = if neighbour_mask != 0 {
        zero_coding_context(block.band, neighbour_mask)
    } else {
        T1_CTXNO_ZC
    };

    // SAFETY: x < w and y < height, so y*w+x is always in bounds
    let coeff = unsafe { *block.coefficients.get_unchecked(y * w + x) };
    let mag = coeff.unsigned_abs();
    let plane_bit = ((mag & one) != 0) as u8;
    coder.encode_with_ctx(ctx, plane_bit);
    flags.clear_visited(x, y);

    if plane_bit == 1 {
        let sign_bit = (coeff < 0) as u8;
        encode_sign(
            coder,
            NativeTier1PassCodingMode::Mq,
            flags,
            x,
            y,
            causal,
            sign_bit,
        );
        flags.mark_significant(x, y, sign_bit);
        *newly_significant += 1;
        *mse_num += sig_mse_delta(mag, one);
    }
}

/// Significant-neighbour mask for context formation; under vertically causal
/// mode (D.7) the bottom row of a stripe does not see the stripe below.
#[inline(always)]
fn neighbours(flags: &FlagGrid, x: usize, y: usize, causal: bool) -> u32 {
    let mask = flags.neighbour_mask(x, y);
    if causal && y % 4 == 3 {
        mask & !(T1_SIGMA_S | T1_SIGMA_SW | T1_SIGMA_SE)
    } else {
        mask
    }
}

/// Code the sign of a newly significant sample: MQ-coded with the sign
/// context and prediction of Table D.3 (south neighbour ignored at a causal
/// stripe boundary), or the raw sign bit itself in a bypass pass (D.6).
#[inline(always)]
fn encode_sign(
    coder: &mut MqCoder,
    mode: NativeTier1PassCodingMode,
    flags: &FlagGrid,
    x: usize,
    y: usize,
    causal: bool,
    sign_bit: u8,
) {
    match mode {
        NativeTier1PassCodingMode::Raw => coder.bypass_encode(sign_bit),
        NativeTier1PassCodingMode::Mq => {
            let (sign_lut_index, _) = if causal && y % 4 == 3 {
                flags.cardinal_sign_context_without_south(x, y)
            } else {
                flags.cardinal_sign_context(x, y)
            };
            let sign_ctx = sign_context(sign_lut_index);
            let prediction = sign_prediction_bit(sign_lut_index);
            coder.encode_with_ctx(sign_ctx, sign_bit ^ prediction);
        }
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn clear_visited_all(flags: &mut FlagGrid, _width: usize, _height: usize) {
    flags.clear_visited_all();
}

/// Annex-J ΔMSE (quantized units) for a coefficient turning significant at the
/// plane with weight `one = 2^b`, assuming the decoder reconstructs truncated
/// coefficients at the midpoint of their uncertainty interval (see decode/t1.rs)
/// and that quantization truncates (residual uniform in [0,1)):
///   err_before = mag + 0.5 (reconstructed as 0)
///   err_after  = lo + 0.5 - one/2, lo = mag & (one-1)
#[inline]
fn sig_mse_delta(mag: u32, one: u32) -> f64 {
    let half = (one >> 1) as f64;
    let lo = (mag & (one - 1)) as f64;
    (mag as f64 + 0.5).powi(2) - (lo + 0.5 - half).powi(2)
}

/// Annex-J ΔMSE (quantized units) for a magnitude-refinement bit at plane
/// `one = 2^b` under midpoint reconstruction:
///   err_before = lo_hi + 0.5 - one   (entering offset is 2^b)
///   err_after  = lo_lo + 0.5 - one/2 (offset shrinks to 2^(b-1))
#[inline]
fn mr_mse_delta(mag: u32, one: u32) -> f64 {
    let half = (one >> 1) as f64;
    let lo_lo = (mag & (one - 1)) as f64;
    let lo_hi = (mag & ((one << 1) - 1)) as f64;
    (lo_hi + 0.5 - one as f64).powi(2) - (lo_lo + 0.5 - half).powi(2)
}

#[inline]
fn distortion_hint(bitplane: u8, newly_significant: usize, max_magnitude: u32) -> u32 {
    if newly_significant == 0 {
        return 0;
    }
    let plane_weight = 1u32 << bitplane.min(30);
    plane_weight
        .saturating_mul(newly_significant as u32 + 1)
        .saturating_mul((max_magnitude > 0) as u32 + 1)
}

#[inline]
fn encode_symbol(coder: &mut MqCoder, mode: NativeTier1PassCodingMode, ctx: u8, bit: u8) {
    match mode {
        NativeTier1PassCodingMode::Mq => coder.encode_with_ctx(ctx, bit),
        NativeTier1PassCodingMode::Raw => coder.bypass_encode(bit),
    }
}

fn plan_coding_passes(coefficients: &[i32], magnitude_bitplanes: u8) -> Vec<NativeTier1Pass> {
    if magnitude_bitplanes == 0 {
        return Vec::new();
    }

    let mut passes = Vec::with_capacity(1 + magnitude_bitplanes.saturating_sub(1) as usize * 3);
    let mut significant = vec![false; coefficients.len()];
    let mut significant_count = 0usize;
    let mut pass_index = 0u16;
    let top = magnitude_bitplanes - 1;

    // Optimized: direct loop instead of iterator chain
    let mut top_newly_significant = 0;
    for i in 0..coefficients.len() {
        if !significant[i] {
            let magnitude = coefficients[i].unsigned_abs();
            if ((magnitude >> top) & 1) != 0 {
                significant[i] = true;
                top_newly_significant += 1;
            }
        }
    }

    let significant_before = significant_count;
    significant_count += top_newly_significant;
    let significant_after = significant_count;
    passes.push(NativeTier1Pass {
        kind: NativeTier1PassKind::Cleanup,
        bitplane: top,
        pass_index,
        coding_mode: NativeTier1PassCodingMode::Mq,
        termination: NativeTier1PassTermination::TermAll,
        segmark: false,
        significant_before,
        newly_significant: top_newly_significant,
        significant_after,
    });
    pass_index += 1;

    for bitplane in (0..top).rev() {
        // Optimized: direct loop instead of iterator chain
        let mut newly_significant = 0;
        for i in 0..coefficients.len() {
            if !significant[i] {
                let magnitude = coefficients[i].unsigned_abs();
                if ((magnitude >> bitplane) & 1) != 0 {
                    significant[i] = true;
                    newly_significant += 1;
                }
            }
        }

        let significant_before = significant_count;
        significant_count += newly_significant;
        let significant_after = significant_count;
        passes.push(NativeTier1Pass {
            kind: NativeTier1PassKind::SignificancePropagation,
            bitplane,
            pass_index,
            coding_mode: NativeTier1PassCodingMode::Mq,
            termination: NativeTier1PassTermination::TermAll,
            segmark: false,
            significant_before,
            newly_significant,
            significant_after,
        });
        pass_index += 1;

        passes.push(NativeTier1Pass {
            kind: NativeTier1PassKind::MagnitudeRefinement,
            bitplane,
            pass_index,
            coding_mode: NativeTier1PassCodingMode::Mq,
            termination: NativeTier1PassTermination::TermAll,
            segmark: false,
            significant_before: significant_after,
            newly_significant: 0,
            significant_after,
        });
        pass_index += 1;

        passes.push(NativeTier1Pass {
            kind: NativeTier1PassKind::Cleanup,
            bitplane,
            pass_index,
            coding_mode: NativeTier1PassCodingMode::Mq,
            termination: NativeTier1PassTermination::TermAll,
            segmark: false,
            significant_before: significant_after,
            newly_significant: 0,
            significant_after,
        });
        pass_index += 1;
    }

    passes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        NativeTier1Layout, NativeTier1PassCodingMode, NativeTier1PassKind,
        NativeTier1PassTermination, analyze_component_layout, band_max_bitplanes,
        encode_band_styled, encode_component_coefficients_for_tile_with_max_bitplanes,
        encode_component_coefficients_with, encode_placeholder_tier1,
    };
    use crate::encode::backend::native::NativeComponentCoefficients;
    use crate::encode::backend::native::layout::build_component_layout;
    use crate::j2k::decode_markers::CodeBlockStyle;
    use crate::plan::{BandOrientation, CodeBlockSize};

    #[test]
    fn tier1_analysis_computes_bitplane_metadata() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            levels: 2,
            data: vec![-38, 36, 0, 16, 144, 0, 0, 16, 0, 0, 0, 0, 64, 64, 0, 0],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");

        let analyzed = analyze_component_layout(&layout);
        let ll = analyzed
            .bands
            .iter()
            .find(|band| band.resolution == 0 && band.band == BandOrientation::Ll)
            .expect("ll band");
        assert_eq!(ll.blocks.len(), 1);
        assert_eq!(ll.blocks[0].max_magnitude, 38);
        assert_eq!(ll.blocks[0].magnitude_bitplanes, 6);
        // Mb(LL) = guard_bits - 1 + expn = 2 - 1 + 8 = 9; zero_bitplanes = 9 - 6 = 3.
        assert_eq!(ll.blocks[0].zero_bitplanes, 3);
        assert_eq!(ll.blocks[0].coding_passes.len(), 16);
        assert_eq!(
            ll.blocks[0].coding_passes[0].kind,
            NativeTier1PassKind::Cleanup
        );
        assert_eq!(
            ll.blocks[0].coding_passes[0].coding_mode,
            NativeTier1PassCodingMode::Mq
        );
        assert_eq!(
            ll.blocks[0].coding_passes[0].termination,
            NativeTier1PassTermination::TermAll
        );
        assert!(!ll.blocks[0].coding_passes[0].segmark);
        assert_eq!(ll.blocks[0].coding_passes[0].bitplane, 5);
        assert_eq!(ll.blocks[0].coding_passes[0].newly_significant, 1);

        let hh2 = analyzed
            .bands
            .iter()
            .find(|band| band.resolution == 2 && band.band == BandOrientation::Hh)
            .expect("resolution 2 hh band");
        assert_eq!(hh2.blocks[0].max_magnitude, 0);
        assert_eq!(hh2.blocks[0].magnitude_bitplanes, 0);
        // Empty HH band: Mb(HH) = guard_bits - 1 + expn = 2 - 1 + 10 = 11.
        assert_eq!(hh2.blocks[0].zero_bitplanes, 11);
        assert!(hh2.blocks[0].coding_passes.is_empty());
    }

    #[test]
    fn direct_coefficient_plane_encoding_matches_legacy_layout_path() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 6,
            height: 6,
            levels: 1,
            data: (0..36).map(|v| v - 18).collect(),
        };
        let code_block_size = CodeBlockSize {
            width: 2,
            height: 2,
        };

        let legacy_layout =
            build_component_layout(&coefficients, code_block_size).expect("build layout");
        let legacy_analyzed = analyze_component_layout(&legacy_layout);
        let legacy_encoded = encode_placeholder_tier1(&legacy_analyzed);

        let direct_encoded =
            encode_component_coefficients_with(&coefficients, 0, code_block_size, 8, 2);

        assert_eq!(direct_encoded, legacy_encoded);
    }

    #[test]
    fn direct_coefficient_plane_encoding_preserves_tile_index() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 6,
            height: 6,
            levels: 1,
            data: (0..36).map(|v| v - 18).collect(),
        };
        let code_block_size = CodeBlockSize {
            width: 2,
            height: 2,
        };

        let encoded = encode_component_coefficients_for_tile_with_max_bitplanes(
            &coefficients,
            7,
            2,
            code_block_size,
            CodeBlockStyle::default(),
            |_, band| band_max_bitplanes(8, 2, band),
        );

        assert!(
            encoded
                .bands
                .iter()
                .flat_map(|band| &band.blocks)
                .all(|block| block.tile_index == 7)
        );
    }

    #[test]
    fn tier1_analysis_preserves_band_and_block_counts() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 6,
            height: 6,
            levels: 1,
            data: (0..36).collect(),
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 2,
                height: 2,
            },
        )
        .expect("build layout");

        let analyzed = analyze_component_layout(&layout);
        assert_eq!(
            analyzed,
            NativeTier1Layout {
                bands: analyzed.bands.clone(),
            }
        );
        let hh = analyzed
            .bands
            .iter()
            .find(|band| band.band == BandOrientation::Hh)
            .expect("hh band");
        assert_eq!(hh.blocks.len(), 4);
        assert_eq!(hh.blocks[0].coefficient_count, 4);
        assert_eq!(hh.blocks[0].max_magnitude, 28);
        assert_eq!(hh.blocks[0].magnitude_bitplanes, 5);
        assert_eq!(hh.blocks[0].coding_passes.len(), 13);
        assert_eq!(
            hh.blocks[0]
                .coding_passes
                .iter()
                .filter(|pass| pass.kind == NativeTier1PassKind::SignificancePropagation)
                .count(),
            4
        );
    }

    #[test]
    fn placeholder_tier1_encoding_produces_cumulative_pass_lengths() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            levels: 2,
            data: vec![-38, 36, 0, 16, 144, 0, 0, 16, 0, 0, 0, 0, 64, 64, 0, 0],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");
        let analyzed = analyze_component_layout(&layout);
        let encoded = encode_placeholder_tier1(&analyzed);

        let ll = encoded
            .bands
            .iter()
            .find(|band| band.resolution == 0 && band.band == BandOrientation::Ll)
            .expect("ll band");
        assert_eq!(ll.blocks[0].passes.len(), 16);
        // In single-termination mode, all bytes live in the code-block payload.
        let total: usize = ll.blocks[0].passes.iter().map(|p| p.length).sum();
        assert!(total > 0);
        assert_eq!(ll.blocks[0].payload.len(), total);
        assert_eq!(
            ll.blocks[0].passes[0].coding_mode,
            NativeTier1PassCodingMode::Mq
        );
        assert_eq!(
            ll.blocks[0].passes[0].termination,
            NativeTier1PassTermination::TermAll
        );
        assert!(!ll.blocks[0].passes[0].segmark);
        assert_eq!(
            ll.blocks[0].passes[0].cumulative_length,
            ll.blocks[0].passes[0].length
        );
        assert_eq!(
            ll.blocks[0].passes[1].cumulative_length,
            ll.blocks[0].passes[0].length + ll.blocks[0].passes[1].length
        );
        assert!(!ll.blocks[0].payload.is_empty());

        let hh2 = encoded
            .bands
            .iter()
            .find(|band| band.resolution == 2 && band.band == BandOrientation::Hh)
            .expect("resolution 2 hh band");
        assert!(hh2.blocks[0].passes.is_empty());
    }

    #[test]
    fn tier1_decoder_roundtrips_native_encoded_codeblock() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            levels: 2,
            data: vec![-38, 36, 0, 16, 144, 0, 0, 16, 0, 0, 0, 0, 64, 64, 0, 0],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");
        let analyzed = analyze_component_layout(&layout);
        let encoded = encode_placeholder_tier1(&analyzed);

        let analyzed_ll = analyzed
            .bands
            .iter()
            .find(|band| band.resolution == 0 && band.band == BandOrientation::Ll)
            .expect("analyzed ll");
        let encoded_ll = encoded
            .bands
            .iter()
            .find(|band| band.resolution == 0 && band.band == BandOrientation::Ll)
            .expect("encoded ll");
        let source = &analyzed_ll.blocks[0];
        let encoded_block = &encoded_ll.blocks[0];
        let data = encoded_block.payload.as_slice();

        let decoded = crate::decode::t1::decode_codeblock(
            source.width,
            source.height,
            source.band,
            source.zero_bitplanes + source.magnitude_bitplanes,
            source.zero_bitplanes as u32,
            encoded_block.passes.len() as u32,
            false,
            data,
        )
        .expect("decode codeblock");

        assert_eq!(decoded.coefficients, source.coefficients);
    }

    #[test]
    fn encoding_deterministic_across_calls() {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 4,
            levels: 1,
            data: vec![7, -3, 0, 5, 2, -8, 1, 0, 4, 0, 6, -2, 0, 1, 0, 3],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");
        let analyzed = analyze_component_layout(&layout);
        let enc1 = encode_placeholder_tier1(&analyzed);
        let enc2 = encode_placeholder_tier1(&analyzed);

        for (b1, b2) in enc1.bands.iter().zip(enc2.bands.iter()) {
            for (p1, p2) in b1.blocks.iter().zip(b2.blocks.iter()) {
                assert_eq!(p1.passes, p2.passes, "encoding must be deterministic");
            }
        }
    }

    /// A 4x8 LL block (zero decomposition levels, so the whole plane is one
    /// band; two stripes, so vertically causal context formation has a
    /// boundary to act on) whose magnitudes reach eight bit-planes, so it has
    /// 22 coding passes and the bypass rule (raw from pass 10) applies.
    fn deep_ll_band() -> super::NativeTier1Band {
        let coefficients = NativeComponentCoefficients {
            x0: 0,
            y0: 0,
            width: 4,
            height: 8,
            levels: 0,
            data: vec![
                200, -3, 0, 150, 2, -8, 1, 0, 4, 0, 6, -2, 0, 1, 0, -255, 9, 0, -40, 3, 0, 77, 0,
                -1, 12, 0, 0, 5, -90, 2, 0, 0,
            ],
        };
        let layout = build_component_layout(
            &coefficients,
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("build layout");
        analyze_component_layout(&layout)
            .bands
            .into_iter()
            .find(|band| band.band == BandOrientation::Ll)
            .expect("ll band")
    }

    fn styled(style: CodeBlockStyle) -> super::NativeEncodedTier1CodeBlock {
        let band = encode_band_styled(&deep_ll_band(), style);
        assert_eq!(band.blocks.len(), 1);
        band.blocks.into_iter().next().expect("one block")
    }

    /// Every styled encode keeps per-pass lengths that sum to the payload
    /// and cumulative lengths that end at it; segments end exactly where
    /// Table D.9 says.
    fn assert_lengths_consistent(
        block: &super::NativeEncodedTier1CodeBlock,
        style: CodeBlockStyle,
    ) {
        let total: usize = block.passes.iter().map(|pass| pass.length).sum();
        assert_eq!(total, block.payload.len());
        let mut running = 0;
        for (index, pass) in block.passes.iter().enumerate() {
            running += pass.length;
            assert_eq!(pass.cumulative_length, running, "pass {index}");
            let expected_terminated =
                index + 1 == block.passes.len() || style.pass_is_terminated(index as u32);
            assert_eq!(
                pass.is_terminated, expected_terminated,
                "pass {index} termination"
            );
            assert_eq!(
                pass.coding_mode == NativeTier1PassCodingMode::Raw,
                style.pass_is_raw(index as u32),
                "pass {index} mode"
            );
        }
    }

    #[test]
    fn predictable_termination_changes_terminated_passes() {
        let termall = CodeBlockStyle {
            terminate_each_pass: true,
            ..CodeBlockStyle::default()
        };
        let erterm = CodeBlockStyle {
            predictable_termination: true,
            ..termall
        };
        let plain = styled(termall);
        let predictable = styled(erterm);
        assert_lengths_consistent(&plain, termall);
        assert_lengths_consistent(&predictable, erterm);
        assert_eq!(plain.passes.len(), 22);
        assert!(plain.passes.iter().all(|pass| pass.is_terminated));
        assert!(
            predictable
                .passes
                .iter()
                .all(|pass| pass.termination == NativeTier1PassTermination::ErTerm)
        );
        assert_ne!(
            plain.payload, predictable.payload,
            "ERTERM should change at least one terminated pass"
        );
    }

    #[test]
    fn segmentation_symbols_mark_cleanup_passes() {
        let plain = CodeBlockStyle {
            terminate_each_pass: true,
            ..CodeBlockStyle::default()
        };
        let segsym = CodeBlockStyle {
            segmentation_symbols: true,
            ..plain
        };
        let without = styled(plain);
        let with = styled(segsym);
        assert_lengths_consistent(&with, segsym);
        let cleanups: Vec<_> = with
            .passes
            .iter()
            .filter(|pass| pass.kind == NativeTier1PassKind::Cleanup)
            .collect();
        assert_eq!(cleanups.len(), 8);
        assert!(cleanups.iter().all(|pass| pass.segmark));
        assert!(with.passes.iter().filter(|pass| pass.segmark).count() == 8);
        assert_ne!(with.payload, without.payload);
    }

    #[test]
    fn bypass_codes_lower_significance_and_refinement_passes_raw() {
        let bypass = CodeBlockStyle {
            bypass: true,
            ..CodeBlockStyle::default()
        };
        let block = styled(bypass);
        assert_lengths_consistent(&block, bypass);
        let raw: Vec<_> = block
            .passes
            .iter()
            .filter(|pass| pass.coding_mode == NativeTier1PassCodingMode::Raw)
            .collect();
        // Passes 10..22 minus the four cleanups among them.
        assert_eq!(raw.len(), 8);
        assert!(raw.iter().all(|pass| pass.pass_index >= 10));
        assert!(
            raw.iter()
                .all(|pass| pass.kind != NativeTier1PassKind::Cleanup)
        );
        // Table D.9 segments: passes 0..=9, then (SP, MR) raw and CL alone.
        let terminated: Vec<u16> = block
            .passes
            .iter()
            .filter(|pass| pass.is_terminated)
            .map(|pass| pass.pass_index)
            .collect();
        assert_eq!(terminated, vec![9, 11, 12, 14, 15, 17, 18, 20, 21]);

        let mq = styled(CodeBlockStyle::default());
        assert_ne!(mq.payload, block.payload);
        // The MQ-coded prefix is the same codeword up to the first termination.
        assert!(block.passes[9].cumulative_length > 0);
    }

    #[test]
    fn reset_and_causal_switches_change_the_codeword_but_not_its_shape() {
        for style in [
            CodeBlockStyle {
                reset_contexts: true,
                ..CodeBlockStyle::default()
            },
            CodeBlockStyle {
                vertical_causal: true,
                ..CodeBlockStyle::default()
            },
        ] {
            let block = styled(style);
            assert_lengths_consistent(&block, style);
            assert_eq!(block.passes.len(), 22);
            assert!(
                block
                    .passes
                    .iter()
                    .filter(|pass| pass.is_terminated)
                    .count()
                    == 1
            );
            assert_ne!(
                block.payload,
                styled(CodeBlockStyle::default()).payload,
                "{style:?}"
            );
        }
    }
}
