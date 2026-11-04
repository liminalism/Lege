//! Tier-1 code-block decoding for the default Part 1 MQ path.
//!
//! This mirrors the encoder pass scan in `encode/backend/native/t1.rs`:
//! top bit-plane cleanup only, then SP -> MR -> cleanup for lower bit-planes.

use crate::error::{Jp2LamError, Result};
use crate::j2k::decode_markers::{CodeBlockStyle, CodestreamHeader, WaveletTransform};
use crate::mq::{MqDecoder, RawDecoder, T1_CTXNO_AGG, T1_CTXNO_MAG, T1_CTXNO_UNI};
use crate::plan::BandOrientation;
use crate::tier1::consts::{
    T1_LUT_SGN_E, T1_LUT_SGN_N, T1_LUT_SGN_S, T1_LUT_SGN_W, T1_SIGMA_E, T1_SIGMA_N, T1_SIGMA_NE,
    T1_SIGMA_NEIGHBOURS, T1_SIGMA_NW, T1_SIGMA_S, T1_SIGMA_W,
};
use crate::tier1::helpers::orientation_index;
use crate::tier1::luts::{LUT_CTXNO_SC, LUT_CTXNO_ZC, LUT_SPB};
use crate::tier1::stripe::{
    StripeFlags, T1_CHI_0_I, T1_CHI_1_I, T1_CHI_2_I, T1_CHI_5_I, T1_MU_0, T1_PI_0, T1_PI_ALL,
    T1_SIGMA_15, T1_SIGMA_16, T1_SIGMA_17, T1_SIGMA_THIS,
};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::stats::StatsSink;
use super::t2::{DecodedCodewordSegment, DecodedTilePackets};

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedCodeBlockCoefficients {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) coefficients: Vec<i32>,
}

/// A fully decoded tile-component coefficient plane, already in the exact
/// numeric domain the inverse DWT consumes.
///
/// Reversible 5/3 coding carries no quantization, so Tier-1 writes the raw
/// integer magnitudes the inverse 5/3 lifting steps expect. Irreversible 9/7
/// coding is dequantized as each code block is emitted, so Tier-1 writes the
/// final `f32` subband samples directly and no separate full-image `i32`
/// coefficient plane or dequantization sweep is materialized.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CoefficientPlane {
    Integer(Vec<i32>),
    Real(Vec<f32>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecodedTileCoefficients {
    pub(crate) component: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) plane: CoefficientPlane,
}

impl DecodedTileCoefficients {
    /// Take the reversible integer plane, erroring if this tile was decoded as
    /// an irreversible (dequantized `f32`) plane.
    pub(crate) fn into_integer(self) -> Result<Vec<i32>> {
        match self.plane {
            CoefficientPlane::Integer(values) => Ok(values),
            CoefficientPlane::Real(_) => Err(invalid(
                "reversible reconstruction requested for an irreversible coefficient plane",
            )),
        }
    }

    /// Take the irreversible, already-dequantized `f32` plane, erroring if this
    /// tile was decoded as a reversible integer plane.
    pub(crate) fn into_real(self) -> Result<Vec<f32>> {
        match self.plane {
            CoefficientPlane::Real(values) => Ok(values),
            CoefficientPlane::Integer(_) => Err(invalid(
                "irreversible reconstruction requested for a reversible coefficient plane",
            )),
        }
    }
}

pub(crate) struct Tier1Scratch {
    /// Decoded (signed) coefficients, row-major `width × height`.
    coefficients: Vec<i32>,
    flags: StripeFlags,
}

impl Tier1Scratch {
    pub(crate) fn new() -> Self {
        Self {
            coefficients: Vec::new(),
            flags: StripeFlags::new(),
        }
    }

    pub(crate) fn coefficients(&self) -> &[i32] {
        &self.coefficients
    }

    fn reset(&mut self, width: usize, height: usize) -> Result<()> {
        let coefficient_count = width
            .checked_mul(height)
            .ok_or_else(|| invalid("code-block coefficient count overflow"))?;
        self.coefficients.resize(coefficient_count, 0);
        self.coefficients.fill(0);
        self.flags.reset(width, height);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn decode_tile_components(
    header: &CodestreamHeader,
    packets: &DecodedTilePackets<'_>,
) -> Result<Vec<DecodedTileCoefficients>> {
    let mut stats = StatsSink::disabled();
    decode_tile_components_profiled(header, packets, &mut stats)
}

#[cfg(test)]
pub(crate) fn decode_tile_components_profiled(
    header: &CodestreamHeader,
    packets: &DecodedTilePackets<'_>,
    stats: &mut StatsSink<'_>,
) -> Result<Vec<DecodedTileCoefficients>> {
    let mut scratch = Tier1Scratch::new();
    decode_tile_components_with_scratch(header, packets, stats, &mut scratch, 1)
}

pub(crate) fn decode_tile_components_with_scratch(
    header: &CodestreamHeader,
    packets: &DecodedTilePackets<'_>,
    stats: &mut StatsSink<'_>,
    scratch: &mut Tier1Scratch,
    worker_count: usize,
) -> Result<Vec<DecodedTileCoefficients>> {
    let component_count = header.siz.components.len();
    // Each component's coefficient plane is sized by ITS tile-component rectangle
    // (Annex B.2): a subsampled chroma plane (`dx`/`dy` > 1) decodes at half
    // resolution, while a non-subsampled component's dims equal the tile's, so
    // the common path is byte-identical. Tier-2 already placed this component's
    // code blocks in the same component-domain coordinate system.
    let component_dims = (0..component_count)
        .map(|component| header.tile_component_dims(component))
        .collect::<Result<Vec<_>>>()?;
    // Irreversible 9/7 code blocks are dequantized as they are written, so the
    // tile plane is the final `f32` subband image. Reversible 5/3 has no
    // quantization and keeps integer magnitudes for the inverse 5/3 lifting.
    // The transform is per-component: a COC override (Annex A.6.2) can make one
    // component reversible while the rest are 9/7, so each plane's storage type
    // follows `transform_for(component)`, not a single tile-wide flag.
    let mut components = (0..component_count)
        .map(|component| {
            let (cw, ch) = component_dims[component];
            DecodedTileCoefficients {
                component,
                width: cw,
                height: ch,
                plane: match header.transform_for(component) {
                    WaveletTransform::Irreversible97 => {
                        CoefficientPlane::Real(vec![0.0f32; cw * ch])
                    }
                    WaveletTransform::Reversible53 => {
                        CoefficientPlane::Integer(vec![0i32; cw * ch])
                    }
                },
            }
        })
        .collect::<Vec<_>>();
    // Serial when profiling (per-pass attribution needs one timeline) or when
    // the active worker budget is one. Otherwise fan the code blocks across
    // exactly the request's task budget on the shared executor; each block
    // writes into its own disjoint plane rectangle.
    if stats.is_enabled() || worker_count <= 1 {
        for block in &packets.codeblocks {
            let params = block_params(header, block)?;
            decode_codeblock_segments_into(
                params.block_width,
                params.block_height,
                block.band,
                params.max_bitplanes,
                block.zero_bitplanes,
                block.passes,
                header.cod.code_block_style,
                &block.segments,
                scratch,
                stats,
            )?;
            let component = components
                .get_mut(block.component)
                .ok_or_else(|| invalid("decoded code-block references invalid component"))?;
            let component_width = component.width;
            let output_start = stats.start();
            write_block_into_plane(
                &mut component.plane,
                component_width,
                block.x0,
                block.y0,
                params.block_width,
                params.block_height,
                &scratch.coefficients,
                params.step,
            )?;
            stats.finish(output_start, |stats, elapsed| {
                stats.tier1_block_output_ns = stats.tier1_block_output_ns.saturating_add(elapsed);
            });
        }
    } else {
        decode_codeblocks_parallel(header, packets, &mut components, worker_count)?;
    }

    Ok(components)
}

struct BlockParams {
    block_width: usize,
    block_height: usize,
    max_bitplanes: u8,
    /// Scalar dequantization step for the irreversible 9/7 path; `None` for the
    /// reversible 5/3 path, which carries no quantization.
    step: Option<f32>,
}

/// Resolve a code block's decode geometry, magnitude bit-plane count, and (for
/// the irreversible path) its fused dequantization step from the codestream
/// headers. Shared by the serial and parallel Tier-1 loops.
fn block_params(
    header: &CodestreamHeader,
    block: &super::t2::DecodedCodeBlock<'_>,
) -> Result<BlockParams> {
    // Per-component quantization: a QCC override for this component, else the
    // default QCD (ISO/IEC 15444-1 Annex A.6.5).
    let comp_quant = header.quant_for(block.component);
    let quant = *comp_quant
        .steps
        .get(block.band_index % comp_quant.steps.len())
        .ok_or_else(|| invalid("missing quantization step for decoded subband"))?;
    let precision = header
        .siz
        .components
        .get(block.component)
        .ok_or_else(|| invalid("decoded code-block references invalid component"))?
        .precision;
    if matches!(
        header.transform_for(block.component),
        WaveletTransform::Reversible53
    ) {
        let maximum_exponent =
            crate::dwt::norms::reversible_exponent(u32::from(precision), block.band);
        if quant.exponent > maximum_exponent {
            return Err(invalid(format!(
                "reversible subband exponent {} exceeds the {}-bit component bound {maximum_exponent}",
                quant.exponent, precision
            )));
        }
    }
    let max_bitplanes = comp_quant.guard_bits.saturating_sub(1) + quant.exponent;
    let step = if matches!(
        header.transform_for(block.component),
        WaveletTransform::Irreversible97
    ) {
        Some(super::reconstruct::quant_step(
            u32::from(precision),
            block.band,
            quant,
        ))
    } else {
        None
    };
    Ok(BlockParams {
        block_width: (block.x1 - block.x0) as usize,
        block_height: (block.y1 - block.y0) as usize,
        max_bitplanes,
        step,
    })
}

/// Write one decoded code block's magnitudes into its tile-component plane,
/// applying fused dequantization for the irreversible (`Real`) plane.
#[allow(clippy::too_many_arguments)]
fn write_block_into_plane(
    plane: &mut CoefficientPlane,
    tile_width: usize,
    x0: u32,
    y0: u32,
    block_width: usize,
    block_height: usize,
    coefficients: &[i32],
    step: Option<f32>,
) -> Result<()> {
    match plane {
        CoefficientPlane::Integer(values) => copy_block_to_tile(
            tile_width,
            values,
            x0,
            y0,
            block_width,
            block_height,
            coefficients,
        ),
        CoefficientPlane::Real(values) => {
            let step =
                step.ok_or_else(|| invalid("irreversible plane write missing quantization step"))?;
            dequantize_block_to_tile(
                tile_width,
                values,
                x0,
                y0,
                block_width,
                block_height,
                coefficients,
                step,
            )
        }
    }
}

/// A `Send`/`Sync` view of every tile-component plane as raw base pointers, so
/// the parallel Tier-1 loop can write each code block into its own disjoint
/// rectangle without a per-block result allocation.
enum PlanePtr {
    Integer(*mut i32),
    Real(*mut f32),
}

struct BlockPlanes {
    planes: Vec<PlanePtr>,
    /// Per-component plane width and length, so a subsampled chroma plane uses
    /// its own (smaller) row stride and bounds rather than the tile's.
    dims: Vec<(usize, usize)>,
}

// SAFETY: `BlockPlanes` only ever hands out `&mut` to pairwise-disjoint plane
// rectangles (one code block each). The raw pointers alias the `components`
// planes, which outlive every parallel task, and no task reads another task's
// region, so sharing the base pointers across threads is sound.
unsafe impl Send for BlockPlanes {}
unsafe impl Sync for BlockPlanes {}

impl BlockPlanes {
    fn new(components: &mut [DecodedTileCoefficients]) -> Self {
        let dims = components
            .iter()
            .map(|component| {
                (
                    component.width,
                    component.width.saturating_mul(component.height),
                )
            })
            .collect();
        let planes = components
            .iter_mut()
            .map(|component| match &mut component.plane {
                CoefficientPlane::Integer(values) => PlanePtr::Integer(values.as_mut_ptr()),
                CoefficientPlane::Real(values) => PlanePtr::Real(values.as_mut_ptr()),
            })
            .collect();
        Self { planes, dims }
    }

    /// # Safety
    /// The caller must guarantee that the `(component, rectangle)` written here
    /// does not overlap any other rectangle written concurrently. Tier-1 code
    /// blocks tile disjoint subband rectangles of each component plane, so this
    /// invariant holds across the parallel loop.
    #[allow(clippy::too_many_arguments)]
    unsafe fn write_block(
        &self,
        component: usize,
        x0: usize,
        y0: usize,
        block_width: usize,
        block_height: usize,
        coefficients: &[i32],
        step: Option<f32>,
    ) -> Result<()> {
        let plane = self
            .planes
            .get(component)
            .ok_or_else(|| invalid("decoded code-block references invalid component"))?;
        let &(tile_width, tile_len) = self
            .dims
            .get(component)
            .ok_or_else(|| invalid("decoded code-block references invalid component"))?;
        for y in 0..block_height {
            let dst = (y0 + y)
                .checked_mul(tile_width)
                .and_then(|row| row.checked_add(x0))
                .ok_or_else(|| invalid("block copy offset overflow"))?;
            if dst
                .checked_add(block_width)
                .is_none_or(|end| end > tile_len)
            {
                return Err(invalid("decoded block extends past tile"));
            }
            let src_row = &coefficients[y * block_width..y * block_width + block_width];
            match *plane {
                PlanePtr::Integer(ptr) => {
                    // SAFETY: `dst..dst + block_width` is in-bounds (checked
                    // above) and disjoint from every other task's rectangle.
                    let dst_slice =
                        unsafe { std::slice::from_raw_parts_mut(ptr.add(dst), block_width) };
                    dst_slice.copy_from_slice(src_row);
                }
                PlanePtr::Real(ptr) => {
                    let step = step.ok_or_else(|| {
                        invalid("irreversible plane write missing quantization step")
                    })?;
                    // SAFETY: as above.
                    let dst_slice =
                        unsafe { std::slice::from_raw_parts_mut(ptr.add(dst), block_width) };
                    for (out, &q) in dst_slice.iter_mut().zip(src_row) {
                        *out = crate::simd::scalar::dequantize_i32_to_f32(q, step);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Decode every code block across the active worker budget, writing each into
/// its disjoint plane rectangle with a per-worker reusable [`Tier1Scratch`].
fn decode_codeblocks_parallel(
    header: &CodestreamHeader,
    packets: &DecodedTilePackets<'_>,
    components: &mut [DecodedTileCoefficients],
    worker_count: usize,
) -> Result<()> {
    let planes = BlockPlanes::new(components);
    let style = header.cod.code_block_style;
    let blocks = &packets.codeblocks;

    // Weighted largest-processing-time-first schedule. Code-block decode cost
    // scales with the coefficient count times the number of coding passes, so
    // decode the most expensive blocks first: their long MQ scans overlap the
    // many cheap blocks instead of one big block stranding on a single worker
    // at the tail. Every block writes a disjoint plane rectangle, so *any*
    // completion order reproduces the serial output byte-for-byte â€” the
    // invariant `serial_and_budgeted_tier1_are_byte_identical` guards.
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    order.sort_unstable_by_key(|&i| {
        let block = &blocks[i];
        let area = u64::from(block.x1.saturating_sub(block.x0))
            * u64::from(block.y1.saturating_sub(block.y0));
        core::cmp::Reverse(area.saturating_mul(u64::from(block.passes)))
    });

    // A shared cursor turns the sorted list into a greedy work queue: whenever a
    // worker finishes a block it claims the next-largest remaining one. The
    // `failed` flag lets peers stop pulling new work once any block errors.
    let cursor = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);

    // Contribute at most `worker_count` long-lived tasks to the shared pool.
    // Each task allocates one [`Tier1Scratch`] for the whole tile and reuses it
    // across every block it claims â€” no per-block scratch reallocation.
    let task_count = worker_count.min(blocks.len()).max(1);
    let results = (0..task_count)
        .into_par_iter()
        .map(|_| -> Result<()> {
            let mut scratch = Tier1Scratch::new();
            let mut sink = StatsSink::disabled();
            loop {
                if failed.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let next = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(&index) = order.get(next) else {
                    return Ok(());
                };
                let block = &blocks[index];
                let params = match block_params(header, block) {
                    Ok(params) => params,
                    Err(error) => {
                        failed.store(true, Ordering::Relaxed);
                        return Err(error);
                    }
                };
                if let Err(error) = decode_codeblock_segments_into(
                    params.block_width,
                    params.block_height,
                    block.band,
                    params.max_bitplanes,
                    block.zero_bitplanes,
                    block.passes,
                    style,
                    &block.segments,
                    &mut scratch,
                    &mut sink,
                ) {
                    failed.store(true, Ordering::Relaxed);
                    return Err(error);
                }
                // SAFETY: code blocks tile pairwise-disjoint rectangles of each
                // component plane, so no two parallel writes overlap.
                let write = unsafe {
                    planes.write_block(
                        block.component,
                        block.x0 as usize,
                        block.y0 as usize,
                        params.block_width,
                        params.block_height,
                        &scratch.coefficients,
                        params.step,
                    )
                };
                if let Err(error) = write {
                    failed.store(true, Ordering::Relaxed);
                    return Err(error);
                }
            }
        })
        .collect::<Vec<_>>();

    for result in results {
        result?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn decode_tile_coefficients(
    header: &CodestreamHeader,
    packets: &DecodedTilePackets<'_>,
) -> Result<DecodedTileCoefficients> {
    let mut components = decode_tile_components(header, packets)?;
    if components.len() != 1 {
        return Err(invalid(
            "single-component decode requested for multi-component tile",
        ));
    }
    Ok(components.remove(0))
}

/// Consume the segmentation symbol that closes a cleanup pass: four symbols
/// coded with the UNIFORM context spelling `0b1010` (ISO/IEC 15444-1 D.5).
/// A different value means the codeword has desynchronized - unless this is
/// the last pass the stream carries for the block: a rate-truncated codeword
/// ends mid-pass, its final symbols are the decoder's 0xff fill, and the
/// marker after them is the first thing to go. That tail is the truncation
/// the encoder chose, not an error, so only an earlier mismatch is one.
pub(super) fn read_segmentation_symbol(decoder: &mut MqDecoder<'_>, last_pass: bool) -> Result<()> {
    let mut value = 0u8;
    for _ in 0..4 {
        value = (value << 1) | decoder.decode_with_ctx(T1_CTXNO_UNI);
    }
    if value != 0xA && !last_pass {
        return Err(invalid(format!(
            "segmentation symbol mismatch: expected 0xA, found {value:#x}"
        )));
    }
    Ok(())
}

/// Decode a payload prefix of `pass_count` coding passes into `scratch`.
///
/// Used by the perceptual candidate reconstructor so a stored Tier-1
/// truncation can be inverted without serializing a codestream.
pub(crate) fn decode_pass_prefix(
    width: usize,
    height: usize,
    band: BandOrientation,
    max_bitplanes: u8,
    zero_bitplanes: u32,
    pass_count: u32,
    data: &[u8],
    scratch: &mut Tier1Scratch,
) -> Result<()> {
    let segment = DecodedCodewordSegment {
        passes: pass_count,
        data: std::borrow::Cow::Borrowed(data),
    };
    let mut stats = StatsSink::disabled();
    decode_codeblock_segments_into(
        width,
        height,
        band,
        max_bitplanes,
        zero_bitplanes,
        pass_count,
        CodeBlockStyle::default(),
        std::slice::from_ref(&segment),
        scratch,
        &mut stats,
    )
}

/// Decode one code-block's coding passes.
///
/// `segmentation_symbols` mirrors the COD code-block style bit (ISO/IEC
/// 15444-1 D.5): when set, each cleanup pass is followed by the four-symbol
/// `0xA` marker coded with the UNIFORM context, which must be consumed to
/// keep the MQ decoder aligned.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_codeblock(
    width: usize,
    height: usize,
    band: BandOrientation,
    max_bitplanes: u8,
    zero_bitplanes: u32,
    pass_count: u32,
    segmentation_symbols: bool,
    data: &[u8],
) -> Result<DecodedCodeBlockCoefficients> {
    let segment = DecodedCodewordSegment {
        passes: pass_count,
        data: std::borrow::Cow::Borrowed(data),
    };
    decode_codeblock_segments(
        width,
        height,
        band,
        max_bitplanes,
        zero_bitplanes,
        pass_count,
        CodeBlockStyle {
            segmentation_symbols,
            ..CodeBlockStyle::default()
        },
        std::slice::from_ref(&segment),
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn decode_codeblock_segments(
    width: usize,
    height: usize,
    band: BandOrientation,
    max_bitplanes: u8,
    zero_bitplanes: u32,
    pass_count: u32,
    style: CodeBlockStyle,
    segments: &[DecodedCodewordSegment<'_>],
) -> Result<DecodedCodeBlockCoefficients> {
    let mut scratch = Tier1Scratch::new();
    let mut stats = StatsSink::disabled();
    decode_codeblock_segments_into(
        width,
        height,
        band,
        max_bitplanes,
        zero_bitplanes,
        pass_count,
        style,
        segments,
        &mut scratch,
        &mut stats,
    )?;
    Ok(DecodedCodeBlockCoefficients {
        width,
        height,
        coefficients: scratch.coefficients,
    })
}

#[allow(clippy::too_many_arguments)]
fn decode_codeblock_segments_into(
    width: usize,
    height: usize,
    band: BandOrientation,
    max_bitplanes: u8,
    zero_bitplanes: u32,
    pass_count: u32,
    style: CodeBlockStyle,
    segments: &[DecodedCodewordSegment<'_>],
    scratch: &mut Tier1Scratch,
    stats: &mut StatsSink<'_>,
) -> Result<()> {
    scratch.reset(width, height)?;
    if pass_count == 0 {
        return Ok(());
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
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(super) enum PassKind {
    Significance,
    Refinement,
    Cleanup,
}

pub(super) fn pass_kind_and_bitplane(pass_index: u32, top: u8) -> Result<(PassKind, u8)> {
    if pass_index == 0 {
        return Ok((PassKind::Cleanup, top));
    }
    let after_top = pass_index - 1;
    let plane_offset = after_top / 3 + 1;
    let bitplane = u32::from(top)
        .checked_sub(plane_offset)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| invalid("coding-pass index exceeds magnitude bit-planes"))?;
    let kind = match after_top % 3 {
        0 => PassKind::Significance,
        1 => PassKind::Refinement,
        _ => PassKind::Cleanup,
    };
    Ok((kind, bitplane))
}

/// The per-pass kernels below walk a code block stripe by stripe (four rows)
/// and column by column, holding the column's packed flag word in a register
/// and writing it back once. Sample `ci` of the column at `(x, 4 s + ci)` is
/// `data[(4 s + ci) * width + x]`.
///
/// Decoded values are stored signed: a newly significant sample at bit-plane
/// `b` becomes `Â±(2^b | 2^(b-1))` (the midpoint of its uncertainty interval)
/// and refinement moves it by `Â±2^(b-1)`, so the final magnitude is the same
/// as accumulating an unsigned magnitude and applying the sign at the end.
#[inline(always)]
fn zero_coding_context_packed(lut: &[u8; 512], word: u32, ci: u32) -> u8 {
    lut[((word >> (3 * ci)) & T1_SIGMA_NEIGHBOURS) as usize]
}

/// Sign-coding LUT index (Table D.3 contribution bits) for sample `ci` from
/// its column word and the west and east column words.
#[inline(always)]
fn sign_lut_index_packed(word: u32, west: u32, east: u32, ci: u32) -> usize {
    let shift = 3 * ci;
    let mut lu = (word >> shift) & (T1_SIGMA_N | T1_SIGMA_W | T1_SIGMA_E | T1_SIGMA_S);
    lu |= (west >> (T1_CHI_1_I + shift)) & T1_LUT_SGN_W;
    lu |= (east >> (T1_CHI_1_I - 2 + shift)) & T1_LUT_SGN_E;
    lu |= if ci == 0 {
        (word >> (T1_CHI_0_I - 4)) & T1_LUT_SGN_N
    } else {
        (word >> (T1_CHI_1_I - 4 + 3 * (ci - 1))) & T1_LUT_SGN_N
    };
    lu |= (word >> (T1_CHI_2_I - 6 + shift)) & T1_LUT_SGN_S;
    (lu & 0xff) as usize
}

/// The flag words and samples one stripe's column walk touches, as slices
/// whose lengths the compiler can see: the three flag rows (the stripe
/// above, the stripe itself, the stripe below) are exactly `width + 2` words
/// each and the four sample rows exactly `width` each (empty past the
/// block's last row), so indexing column `x < width` and its neighbours
/// needs no bounds checks in the full-stripe kernels.
struct StripeRows<'a> {
    north: &'a mut [u32],
    mid: &'a mut [u32],
    south: &'a mut [u32],
    samples: [&'a mut [i32]; 4],
}

impl<'a> StripeRows<'a> {
    #[inline(always)]
    fn new(
        flags: &'a mut StripeFlags,
        data: &'a mut [i32],
        stripe: usize,
        width: usize,
        height: usize,
    ) -> Self {
        let stride = width + 2;
        debug_assert_eq!(stride, flags.stride());
        let words = flags.words_mut();
        let (_, rest) = words.split_at_mut(stripe * stride);
        let (north, rest) = rest.split_at_mut(stride);
        let (mid, rest) = rest.split_at_mut(stride);
        let south = &mut rest[..stride];
        let rows = (height - 4 * stripe).min(4);
        let samples = &mut data[4 * stripe * width..(4 * stripe + rows) * width];
        let samples = if rows == 4 {
            let (r0, rest) = samples.split_at_mut(width);
            let (r1, rest) = rest.split_at_mut(width);
            let (r2, r3) = rest.split_at_mut(width);
            [r0, r1, r2, r3]
        } else {
            let (r0, rest) = samples.split_at_mut(width.min(samples.len()));
            let (r1, rest) = rest.split_at_mut(width.min(rest.len()));
            let (r2, r3) = rest.split_at_mut(width.min(rest.len()));
            [r0, r1, r2, r3]
        };
        Self {
            north,
            mid,
            south,
            samples,
        }
    }

    /// The packed word of column `x`.
    #[inline(always)]
    fn word(&self, x: usize) -> u32 {
        self.mid[x + 1]
    }

    #[inline(always)]
    fn set_word(&mut self, x: usize, word: u32) {
        self.mid[x + 1] = word;
    }

    /// Sample `ci` of column `x`.
    #[inline(always)]
    fn sample(&mut self, ci: u32, x: usize) -> &mut i32 {
        &mut self.samples[ci as usize][x]
    }
}

/// Record that sample `ci` of column `x` became significant with sign `sign`
/// (1 = negative): its own word (kept in `word`) and the neighbouring
/// columns' words learn about it. In vertically causal mode the stripe above
/// never learns about row 0 (Annex D.7).
#[inline(always)]
fn mark_significant_packed(
    s: &mut StripeRows<'_>,
    x: usize,
    word: &mut u32,
    ci: u32,
    sign: u32,
    vertical_causal: bool,
) {
    let shift = 3 * ci;
    s.mid[x] |= T1_SIGMA_E << shift;
    *word |= ((sign << T1_CHI_1_I) | T1_SIGMA_THIS) << shift;
    s.mid[x + 2] |= T1_SIGMA_W << shift;
    if ci == 0 && !vertical_causal {
        s.north[x + 1] |= (sign << T1_CHI_5_I) | T1_SIGMA_16;
        s.north[x] |= T1_SIGMA_17;
        s.north[x + 2] |= T1_SIGMA_15;
    }
    if ci == 3 {
        s.south[x + 1] |= (sign << T1_CHI_0_I) | T1_SIGMA_N;
        s.south[x] |= T1_SIGMA_NE;
        s.south[x + 2] |= T1_SIGMA_NW;
    }
}

/// Decode the sign of a sample that just became significant and store its
/// midpoint value.
#[inline(always)]
fn decode_new_significant_packed(
    decoder: &mut MqDecoder<'_>,
    s: &mut StripeRows<'_>,
    x: usize,
    word: &mut u32,
    ci: u32,
    one_plus_half: i32,
    vertical_causal: bool,
) {
    let lu = sign_lut_index_packed(*word, s.mid[x], s.mid[x + 2], ci);
    let sign = u32::from(decoder.decode_with_ctx(LUT_CTXNO_SC[lu]) ^ LUT_SPB[lu]);
    *s.sample(ci, x) = if sign != 0 {
        -one_plus_half
    } else {
        one_plus_half
    };
    mark_significant_packed(s, x, word, ci, sign, vertical_causal);
}

/// Significance propagation pass (D.3.1) over the whole block.
#[allow(clippy::too_many_arguments)]
fn sigpass_decode(
    decoder: &mut MqDecoder<'_>,
    band: BandOrientation,
    bitplane: u8,
    flags: &mut StripeFlags,
    data: &mut [i32],
    width: usize,
    height: usize,
    vertical_causal: bool,
) {
    let one = 1i32 << bitplane;
    let one_plus_half = one | (one >> 1);
    let lut = &LUT_CTXNO_ZC[orientation_index(band)];
    let stripes = height.div_ceil(4);
    #[inline(always)]
    fn column(
        decoder: &mut MqDecoder<'_>,
        lut: &[u8; 512],
        s: &mut StripeRows<'_>,
        x: usize,
        rows: u32,
        one_plus_half: i32,
        vertical_causal: bool,
    ) {
        let mut word = s.word(x);
        if word == 0 {
            return;
        }
        for ci in 0..rows {
            let shift = 3 * ci;
            if word & ((T1_SIGMA_THIS | T1_PI_0) << shift) == 0
                && word & (T1_SIGMA_NEIGHBOURS << shift) != 0
            {
                let ctx = zero_coding_context_packed(lut, word, ci);
                if decoder.decode_with_ctx(ctx) != 0 {
                    decode_new_significant_packed(
                        decoder,
                        s,
                        x,
                        &mut word,
                        ci,
                        one_plus_half,
                        vertical_causal,
                    );
                }
                word |= T1_PI_0 << shift;
            }
        }
        s.set_word(x, word);
    }
    for stripe in 0..stripes {
        let rows = (height - 4 * stripe).min(4) as u32;
        let mut s = StripeRows::new(flags, data, stripe, width, height);
        if rows == 4 {
            for x in 0..width {
                column(decoder, lut, &mut s, x, 4, one_plus_half, vertical_causal);
            }
        } else {
            for x in 0..width {
                column(
                    decoder,
                    lut,
                    &mut s,
                    x,
                    rows,
                    one_plus_half,
                    vertical_causal,
                );
            }
        }
    }
}

/// Raw significance pass (D.6): the same traversal and membership test as
/// [`sigpass_decode`], with the significance bit and then the sign read as
/// plain bits, no context and no sign prediction.
fn sigpass_decode_raw(
    raw: &mut RawDecoder<'_>,
    bitplane: u8,
    flags: &mut StripeFlags,
    data: &mut [i32],
    width: usize,
    height: usize,
    vertical_causal: bool,
) {
    let one = 1i32 << bitplane;
    let one_plus_half = one | (one >> 1);
    let stripes = height.div_ceil(4);
    for stripe in 0..stripes {
        let rows = (height - 4 * stripe).min(4) as u32;
        let mut s = StripeRows::new(flags, data, stripe, width, height);
        for x in 0..width {
            let mut word = s.word(x);
            if word != 0 {
                for ci in 0..rows {
                    let shift = 3 * ci;
                    if word & ((T1_SIGMA_THIS | T1_PI_0) << shift) == 0
                        && word & (T1_SIGMA_NEIGHBOURS << shift) != 0
                    {
                        if raw.decode_bit() != 0 {
                            let sign = u32::from(raw.decode_bit());
                            *s.sample(ci, x) = if sign != 0 {
                                -one_plus_half
                            } else {
                                one_plus_half
                            };
                            mark_significant_packed(
                                &mut s,
                                x,
                                &mut word,
                                ci,
                                sign,
                                vertical_causal,
                            );
                        }
                        word |= T1_PI_0 << shift;
                    }
                }
                s.set_word(x, word);
            }
        }
    }
}

/// Midpoint update for a refined sample. Entering MR at plane b the magnitude
/// carries a 2^b offset (midpoint of the previous uncertainty interval
/// [0, 2^(b+1))). Learning bit b halves the interval: offset becomes 2^(b-1).
///   bit=1: known += 2^b            → mag += 2^(b-1)
///   bit=0: known unchanged         → mag -= 2^b - 2^(b-1)
/// At b=0 this reduces to bit=1: exact, bit=0: mag -= 1 (exact).
#[inline(always)]
fn refine_sample(value: &mut i32, bit: u8, one: i32) {
    let half = one >> 1;
    let delta = if bit != 0 { half } else { half - one };
    *value += if *value < 0 { -delta } else { delta };
}

/// Magnitude refinement pass (D.3.3) over the whole block.
fn refpass_decode(
    decoder: &mut MqDecoder<'_>,
    bitplane: u8,
    flags: &mut StripeFlags,
    data: &mut [i32],
    width: usize,
    height: usize,
) {
    let one = 1i32 << bitplane;
    let stripes = height.div_ceil(4);
    #[inline(always)]
    fn column(decoder: &mut MqDecoder<'_>, s: &mut StripeRows<'_>, x: usize, rows: u32, one: i32) {
        let mut word = s.word(x);
        if word == 0 {
            return;
        }
        for ci in 0..rows {
            let shift = 3 * ci;
            if word & ((T1_SIGMA_THIS | T1_PI_0) << shift) == T1_SIGMA_THIS << shift {
                let ctx = magnitude_context_packed(word, ci);
                let bit = decoder.decode_with_ctx(ctx);
                refine_sample(s.sample(ci, x), bit, one);
                word |= T1_MU_0 << shift;
            }
        }
        s.set_word(x, word);
    }
    for stripe in 0..stripes {
        let rows = (height - 4 * stripe).min(4) as u32;
        let mut s = StripeRows::new(flags, data, stripe, width, height);
        if rows == 4 {
            for x in 0..width {
                column(decoder, &mut s, x, 4, one);
            }
        } else {
            for x in 0..width {
                column(decoder, &mut s, x, rows, one);
            }
        }
    }
}

/// Raw magnitude refinement pass (D.6): [`refpass_decode`] with the
/// refinement bit read as a plain bit.
fn refpass_decode_raw(
    raw: &mut RawDecoder<'_>,
    bitplane: u8,
    flags: &mut StripeFlags,
    data: &mut [i32],
    width: usize,
    height: usize,
) {
    let one = 1i32 << bitplane;
    let stripes = height.div_ceil(4);
    for stripe in 0..stripes {
        let rows = (height - 4 * stripe).min(4) as u32;
        let mut s = StripeRows::new(flags, data, stripe, width, height);
        for x in 0..width {
            let mut word = s.word(x);
            if word != 0 {
                for ci in 0..rows {
                    let shift = 3 * ci;
                    if word & ((T1_SIGMA_THIS | T1_PI_0) << shift) == T1_SIGMA_THIS << shift {
                        let bit = raw.decode_bit();
                        refine_sample(s.sample(ci, x), bit, one);
                        word |= T1_MU_0 << shift;
                    }
                }
                s.set_word(x, word);
            }
        }
    }
}

#[inline(always)]
fn magnitude_context_packed(word: u32, ci: u32) -> u8 {
    let shift = 3 * ci;
    if word & (T1_MU_0 << shift) != 0 {
        T1_CTXNO_MAG + 2
    } else if word & (T1_SIGMA_NEIGHBOURS << shift) != 0 {
        T1_CTXNO_MAG + 1
    } else {
        T1_CTXNO_MAG
    }
}

/// Cleanup pass (D.3.4) over the whole block, with run-length coding of
/// columns whose word is zero in a full stripe.
#[allow(clippy::too_many_arguments)]
fn cleanup_decode(
    decoder: &mut MqDecoder<'_>,
    band: BandOrientation,
    bitplane: u8,
    flags: &mut StripeFlags,
    data: &mut [i32],
    width: usize,
    height: usize,
    vertical_causal: bool,
) {
    let one = 1i32 << bitplane;
    let one_plus_half = one | (one >> 1);
    let lut = &LUT_CTXNO_ZC[orientation_index(band)];
    let stripes = height.div_ceil(4);
    /// Code samples `first .. rows` of one column; with `run_length` the
    /// sample `first` is already known significant (the run-length symbols
    /// said so) and only its sign is read.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn column(
        decoder: &mut MqDecoder<'_>,
        lut: &[u8; 512],
        s: &mut StripeRows<'_>,
        x: usize,
        word: &mut u32,
        first: u32,
        rows: u32,
        run_length: bool,
        one_plus_half: i32,
        vertical_causal: bool,
    ) {
        let mut first_is_significant = run_length;
        for ci in first..rows {
            let shift = 3 * ci;
            let coded = if first_is_significant {
                first_is_significant = false;
                true
            } else if *word & ((T1_SIGMA_THIS | T1_PI_0) << shift) == 0 {
                let ctx = zero_coding_context_packed(lut, *word, ci);
                decoder.decode_with_ctx(ctx) != 0
            } else {
                false
            };
            if coded {
                decode_new_significant_packed(
                    decoder,
                    s,
                    x,
                    word,
                    ci,
                    one_plus_half,
                    vertical_causal,
                );
            }
        }
    }
    for stripe in 0..stripes {
        let rows = (height - 4 * stripe).min(4) as u32;
        let mut s = StripeRows::new(flags, data, stripe, width, height);
        for x in 0..width {
            let mut word = s.word(x);
            if word == 0 && rows == 4 {
                if decoder.decode_with_ctx(T1_CTXNO_AGG) == 0 {
                    continue;
                }
                let hi = u32::from(decoder.decode_with_ctx(T1_CTXNO_UNI));
                let lo = u32::from(decoder.decode_with_ctx(T1_CTXNO_UNI));
                let first = (hi << 1) | lo;
                column(
                    decoder,
                    lut,
                    &mut s,
                    x,
                    &mut word,
                    first,
                    4,
                    true,
                    one_plus_half,
                    vertical_causal,
                );
            } else if rows == 4 {
                column(
                    decoder,
                    lut,
                    &mut s,
                    x,
                    &mut word,
                    0,
                    4,
                    false,
                    one_plus_half,
                    vertical_causal,
                );
            } else {
                column(
                    decoder,
                    lut,
                    &mut s,
                    x,
                    &mut word,
                    0,
                    rows,
                    false,
                    one_plus_half,
                    vertical_causal,
                );
            }
            s.set_word(x, word & !T1_PI_ALL);
        }
    }
}

fn copy_block_to_tile(
    tile_width: usize,
    tile: &mut [i32],
    block_x0: u32,
    block_y0: u32,
    block_width: usize,
    block_height: usize,
    coefficients: &[i32],
) -> Result<()> {
    let block_x0 = usize::try_from(block_x0).map_err(|_| invalid("block x0 exceeds usize"))?;
    let block_y0 = usize::try_from(block_y0).map_err(|_| invalid("block y0 exceeds usize"))?;
    for y in 0..block_height {
        let dst = (block_y0 + y)
            .checked_mul(tile_width)
            .and_then(|row| row.checked_add(block_x0))
            .ok_or_else(|| invalid("block copy offset overflow"))?;
        let src = y * block_width;
        let dst_end = dst + block_width;
        let src_end = src + block_width;
        let dst_slice = tile
            .get_mut(dst..dst_end)
            .ok_or_else(|| invalid("decoded block extends past tile"))?;
        dst_slice.copy_from_slice(&coefficients[src..src_end]);
    }
    Ok(())
}

/// Write one code block's decoded magnitudes into the tile's `f32` subband
/// plane, applying scalar dequantization in the same pass (fused Tier-1 output
/// + dequantization for the irreversible 9/7 path).
#[allow(clippy::too_many_arguments)]
fn dequantize_block_to_tile(
    tile_width: usize,
    tile: &mut [f32],
    block_x0: u32,
    block_y0: u32,
    block_width: usize,
    block_height: usize,
    coefficients: &[i32],
    step: f32,
) -> Result<()> {
    let block_x0 = usize::try_from(block_x0).map_err(|_| invalid("block x0 exceeds usize"))?;
    let block_y0 = usize::try_from(block_y0).map_err(|_| invalid("block y0 exceeds usize"))?;
    for y in 0..block_height {
        let dst = (block_y0 + y)
            .checked_mul(tile_width)
            .and_then(|row| row.checked_add(block_x0))
            .ok_or_else(|| invalid("block copy offset overflow"))?;
        let src = y * block_width;
        let dst_slice = tile
            .get_mut(dst..dst + block_width)
            .ok_or_else(|| invalid("decoded block extends past tile"))?;
        let src_slice = &coefficients[src..src + block_width];
        for (out, &q) in dst_slice.iter_mut().zip(src_slice) {
            *out = crate::simd::scalar::dequantize_i32_to_f32(q, step);
        }
    }
    Ok(())
}

pub(super) fn invalid(message: impl Into<String>) -> Jp2LamError {
    Jp2LamError::DecodeFailed(message.into())
}

#[cfg(test)]
mod tests {
    use super::{decode_codeblock, decode_codeblock_segments};
    use crate::decode::t2::DecodedCodewordSegment;
    use crate::j2k::decode_markers::CodeBlockStyle;
    use crate::mq::{MqCoder, T1_CTXNO_MAG, T1_CTXNO_ZC};
    use crate::plan::BandOrientation;
    use crate::tier1::helpers::{sign_context, sign_prediction_bit};
    use std::borrow::Cow;

    #[test]
    fn cleanup_only_single_sample_decodes_positive_one() {
        let mut coder = MqCoder::new();
        coder.encode_with_ctx(T1_CTXNO_ZC, 1);
        let sign_ctx = sign_context(0);
        let prediction = sign_prediction_bit(0);
        coder.encode_with_ctx(sign_ctx, prediction);
        let bytes = coder.finish();

        let decoded =
            decode_codeblock(1, 1, BandOrientation::Ll, 1, 0, 1, false, &bytes).expect("decode");
        assert_eq!(decoded.coefficients, vec![1]);
    }

    #[test]
    fn cleanup_and_refinement_single_sample_decodes_negative_three() {
        let mut coder = MqCoder::new();
        coder.encode_with_ctx(T1_CTXNO_ZC, 1);
        let sign_ctx = sign_context(0);
        let prediction = sign_prediction_bit(0);
        coder.encode_with_ctx(sign_ctx, 1 ^ prediction);
        coder.encode_with_ctx(T1_CTXNO_MAG, 1);
        let bytes = coder.finish();

        let decoded =
            decode_codeblock(1, 1, BandOrientation::Ll, 2, 0, 3, false, &bytes).expect("decode");
        assert_eq!(decoded.coefficients, vec![-3]);
    }

    #[test]
    fn d4_d5_termall_reset_and_segmark_decode_across_pass_segments() {
        let sign_ctx = sign_context(0);
        let prediction = sign_prediction_bit(0);

        let mut cleanup = MqCoder::new();
        cleanup.encode_with_ctx(T1_CTXNO_ZC, 1);
        cleanup.encode_with_ctx(sign_ctx, prediction);
        cleanup.segmark_encode();
        let cleanup = cleanup.finish();

        // The significance pass has no symbols for an already significant
        // one-sample block. A zero-byte terminated segment is valid; Annex
        // D.4.1 supplies synthetic 0xFF bytes if the decoder needs symbols.
        let significance = Vec::new();

        let mut refinement = MqCoder::new();
        refinement.encode_with_ctx(T1_CTXNO_MAG, 1);
        let refinement = refinement.finish();

        let segments = [
            DecodedCodewordSegment {
                passes: 1,
                data: Cow::Borrowed(&cleanup),
            },
            DecodedCodewordSegment {
                passes: 1,
                data: Cow::Borrowed(&significance),
            },
            DecodedCodewordSegment {
                passes: 1,
                data: Cow::Borrowed(&refinement),
            },
        ];
        let decoded = decode_codeblock_segments(
            1,
            1,
            BandOrientation::Ll,
            2,
            0,
            3,
            CodeBlockStyle {
                reset_contexts: true,
                terminate_each_pass: true,
                predictable_termination: true,
                segmentation_symbols: true,
                ..CodeBlockStyle::default()
            },
            &segments,
        )
        .expect("terminated pass segments");
        assert_eq!(decoded.coefficients, vec![3]);
    }

    #[test]
    fn d41_empty_segment_uses_synthesized_end_bytes() {
        let decoded = decode_codeblock(1, 1, BandOrientation::Ll, 1, 0, 1, false, &[])
            .expect("MQ decoder synthesizes end bytes");
        assert_eq!(decoded.coefficients, vec![0]);
    }

    #[test]
    fn impossible_pass_count_fails_fast() {
        let err = decode_codeblock(1, 1, BandOrientation::Ll, 1, 0, 2, false, &[0])
            .expect_err("too many passes for one bit-plane should fail")
            .to_string();

        assert!(err.contains("exceeds maximum"), "{err}");
    }

    #[test]
    fn magnitude_bitplanes_that_do_not_fit_i32_fail_fast() {
        let err = decode_codeblock(1, 1, BandOrientation::Ll, 32, 0, 1, false, &[0])
            .expect_err("32 magnitude bits cannot be represented by i32")
            .to_string();

        assert!(err.contains("i32 decoder limit"), "{err}");
    }
}

/// The packed stripe-column decoder must reproduce the previous per-sample
/// decoder bit for bit, for every code-block style switch and for truncated
/// pass counts. Blocks are produced by the in-tree encoder.
#[cfg(test)]
mod stripe_equivalence_tests {
    use super::decode_codeblock_segments;
    use crate::decode::t1_reference::decode_codeblock_segments_reference;
    use crate::decode::t2::DecodedCodewordSegment;
    use crate::encode::backend::native::NativeComponentCoefficients;
    use crate::encode::backend::native::layout::build_component_layout;
    use crate::encode::backend::native::t1::{
        NativeEncodedTier1CodeBlock, NativeTier1CodeBlock, analyze_component_layout,
        encode_codeblock_styled,
    };
    use crate::j2k::decode_markers::CodeBlockStyle;
    use crate::plan::CodeBlockSize;
    use std::borrow::Cow;

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
    }

    /// Coefficient fields with different statistics: sparse (mostly run-length
    /// coded), dense small, dense with a few huge values (many bit-planes).
    fn field(rng: &mut Lcg, len: usize, flavour: u32) -> Vec<i32> {
        (0..len)
            .map(|_| {
                let r = rng.next();
                let sign = if r & 1 == 0 { 1 } else { -1 };
                match flavour {
                    0 => {
                        if r % 11 == 0 {
                            sign * ((r >> 8) % 40) as i32
                        } else {
                            0
                        }
                    }
                    1 => sign * ((r >> 8) % 9) as i32,
                    2 => {
                        if r % 37 == 0 {
                            sign * (1000 + (r >> 8) % 3000) as i32
                        } else {
                            sign * ((r >> 8) % 5) as i32
                        }
                    }
                    _ => {
                        if r % 3 == 0 {
                            sign * ((r >> 8) % 300) as i32
                        } else {
                            0
                        }
                    }
                }
            })
            .collect()
    }

    fn encoded_blocks(
        width: usize,
        height: usize,
        levels: u8,
        coefficients: Vec<i32>,
        style: CodeBlockStyle,
    ) -> Vec<(NativeTier1CodeBlock, NativeEncodedTier1CodeBlock)> {
        let layout = build_component_layout(
            &NativeComponentCoefficients {
                x0: 0,
                y0: 0,
                width,
                height,
                levels,
                data: coefficients,
            },
            CodeBlockSize {
                width: 64,
                height: 64,
            },
        )
        .expect("layout");
        analyze_component_layout(&layout)
            .bands
            .into_iter()
            .flat_map(|band| band.blocks)
            .map(|block| {
                let encoded = encode_codeblock_styled(&block, style);
                (block, encoded)
            })
            .collect()
    }

    /// Split a styled block's payload into its terminated codeword segments,
    /// keeping only the first `pass_count` passes (a whole number of segments
    /// or a truncated last one, as Tier-2 would deliver them).
    fn segments(
        encoded: &NativeEncodedTier1CodeBlock,
        pass_count: usize,
    ) -> Vec<DecodedCodewordSegment<'static>> {
        let mut out = Vec::new();
        let mut start = 0usize;
        let mut passes = 0u32;
        for (index, pass) in encoded.passes.iter().enumerate().take(pass_count) {
            passes += 1;
            if pass.is_terminated || index + 1 == pass_count {
                out.push(DecodedCodewordSegment {
                    passes,
                    data: Cow::Owned(encoded.payload[start..pass.cumulative_length].to_vec()),
                });
                start = pass.cumulative_length;
                passes = 0;
            }
        }
        out
    }

    fn check(
        style: CodeBlockStyle,
        width: usize,
        height: usize,
        levels: u8,
        flavour: u32,
        seed: u64,
    ) {
        let mut rng = Lcg(seed);
        let coefficients = field(&mut rng, width * height, flavour);
        for (block, encoded) in encoded_blocks(width, height, levels, coefficients, style) {
            if encoded.passes.is_empty() {
                continue;
            }
            let max_bitplanes = block.zero_bitplanes + block.magnitude_bitplanes;
            let total = encoded.passes.len();
            // Every prefix length up to a few, then the whole block.
            let mut counts: Vec<usize> = (1..=total.min(4)).collect();
            counts.push(total);
            if total > 6 {
                counts.push(total - 1);
                counts.push(total / 2);
            }
            for pass_count in counts {
                let segments = segments(&encoded, pass_count);
                let fast = decode_codeblock_segments(
                    block.width,
                    block.height,
                    block.band,
                    max_bitplanes,
                    u32::from(block.zero_bitplanes),
                    pass_count as u32,
                    style,
                    &segments,
                )
                .map(|decoded| decoded.coefficients)
                .map_err(|error| error.to_string());
                let reference = decode_codeblock_segments_reference(
                    block.width,
                    block.height,
                    block.band,
                    max_bitplanes,
                    u32::from(block.zero_bitplanes),
                    pass_count as u32,
                    style,
                    &segments,
                )
                .map_err(|error| error.to_string());
                // A truncated prefix that ends between two cleanup passes can
                // leave a segmentation symbol short of bytes; both decoders
                // must then report the same error.
                assert_eq!(
                    fast, reference,
                    "{style:?} {}x{} {:?} flavour {flavour} passes {pass_count}/{total}",
                    block.width, block.height, block.band
                );
                if pass_count == total {
                    assert_eq!(
                        fast.as_deref().ok(),
                        Some(block.coefficients.as_slice()),
                        "{style:?} {}x{} {:?} flavour {flavour}: full decode is lossless",
                        block.width,
                        block.height,
                        block.band
                    );
                }
            }
        }
    }

    fn styles() -> Vec<CodeBlockStyle> {
        let d = CodeBlockStyle::default();
        vec![
            d,
            CodeBlockStyle {
                vertical_causal: true,
                ..d
            },
            CodeBlockStyle {
                reset_contexts: true,
                ..d
            },
            CodeBlockStyle {
                terminate_each_pass: true,
                ..d
            },
            CodeBlockStyle { bypass: true, ..d },
            CodeBlockStyle {
                segmentation_symbols: true,
                ..d
            },
            CodeBlockStyle {
                bypass: true,
                vertical_causal: true,
                reset_contexts: true,
                terminate_each_pass: true,
                predictable_termination: true,
                segmentation_symbols: true,
            },
            CodeBlockStyle {
                bypass: true,
                vertical_causal: true,
                segmentation_symbols: true,
                ..d
            },
        ]
    }

    #[test]
    fn packed_decoder_matches_reference_on_square_blocks() {
        for (i, style) in styles().into_iter().enumerate() {
            for flavour in 0..4 {
                check(
                    style,
                    64,
                    64,
                    0,
                    flavour,
                    1 + i as u64 * 10 + flavour as u64,
                );
                check(
                    style,
                    128,
                    128,
                    1,
                    flavour,
                    100 + i as u64 * 10 + flavour as u64,
                );
            }
        }
    }

    /// With segmentation symbols every cleanup pass ends in a marker the
    /// decoder verifies, so every pass prefix a layer can end on must decode
    /// without a marker error: the per-pass lengths have to cover the bytes
    /// the MQ coder still holds in its registers at the pass boundary.
    #[test]
    fn segmentation_symbol_prefixes_decode_at_every_pass() {
        let d = CodeBlockStyle::default();
        let styles = [
            CodeBlockStyle {
                segmentation_symbols: true,
                ..d
            },
            CodeBlockStyle {
                segmentation_symbols: true,
                reset_contexts: true,
                ..d
            },
            CodeBlockStyle {
                segmentation_symbols: true,
                vertical_causal: true,
                ..d
            },
            CodeBlockStyle {
                segmentation_symbols: true,
                bypass: true,
                ..d
            },
            CodeBlockStyle {
                segmentation_symbols: true,
                bypass: true,
                vertical_causal: true,
                reset_contexts: true,
                ..d
            },
        ];
        let mut checked = 0usize;
        for (i, style) in styles.into_iter().enumerate() {
            for &(w, h, levels) in &[
                (64usize, 64usize, 0u8),
                (128, 128, 1),
                (61, 59, 1),
                (5, 7, 0),
            ] {
                for flavour in 0..4u32 {
                    let mut rng = Lcg(5000 + i as u64 * 100 + (w * h) as u64 + flavour as u64);
                    let coefficients = field(&mut rng, w * h, flavour);
                    for (block, encoded) in encoded_blocks(w, h, levels, coefficients, style) {
                        let total = encoded.passes.len();
                        let max_bitplanes = block.zero_bitplanes + block.magnitude_bitplanes;
                        for pass_count in 1..=total {
                            let segments = segments(&encoded, pass_count);
                            let decoded = decode_codeblock_segments(
                                block.width,
                                block.height,
                                block.band,
                                max_bitplanes,
                                u32::from(block.zero_bitplanes),
                                pass_count as u32,
                                style,
                                &segments,
                            )
                            .unwrap_or_else(|error| {
                                panic!(
                                    "{style:?} {}x{} {:?} flavour {flavour} passes {pass_count}/{total}: {error}",
                                    block.width, block.height, block.band
                                )
                            });
                            if pass_count == total {
                                assert_eq!(decoded.coefficients, block.coefficients);
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 1000, "{checked} prefixes checked");
    }

    #[test]
    fn packed_decoder_matches_reference_on_odd_blocks() {
        for (i, style) in styles().into_iter().enumerate() {
            for &(w, h, levels) in &[
                (1usize, 1usize, 0u8),
                (5, 7, 0),
                (17, 33, 0),
                (64, 6, 0),
                (3, 64, 0),
                (9, 10, 1),
                (61, 59, 1),
                (2, 5, 0),
                (64, 63, 0),
                (63, 1, 0),
                (1, 63, 0),
            ] {
                for flavour in [1u32, 3] {
                    check(
                        style,
                        w,
                        h,
                        levels,
                        flavour,
                        1000 + i as u64 * 100 + (w * h) as u64 + flavour as u64,
                    );
                }
            }
        }
    }
}
