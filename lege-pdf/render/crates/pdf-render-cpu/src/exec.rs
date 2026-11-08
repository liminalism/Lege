//! The region executor: walk a [`CpuPreparedPage`]'s compact commands in
//! painter order and paint them into a [`Surface`] via span dispatch.
//!
//! Per command: apply the clip (rectangular envelope by arithmetic, non-rect
//! clips by a lazily-built `Alpha8` mask), generate analytic coverage, then
//! for each contiguous span pick a kernel **once** — opaque full spans
//! overwrite, partial-alpha full spans use the constant-alpha blend, and
//! anti-aliased/masked runs use the per-pixel mask blend (advice §4, §5, §8).
//! No allocation, `Arc` indexing, or transform work happens here.

#[cfg(feature = "profiling")]
use std::time::Instant;

use crate::kernels::{
    BlendFn, KernelSet, NonSepBlendFn, blend_span, blend_span_nonsep, composite_px, mul_div_255,
    nonseparable_blend, separable_blend,
};

/// The blend path chosen once per command (never per pixel).
#[derive(Clone, Copy)]
enum BlendChoice {
    Normal,
    Separable(BlendFn),
    NonSeparable(NonSepBlendFn),
}

fn choose_blend(mode: pdf_page_ir::BlendMode) -> BlendChoice {
    if let Some(f) = separable_blend(mode) {
        BlendChoice::Separable(f)
    } else if let Some(f) = nonseparable_blend(mode) {
        BlendChoice::NonSeparable(f)
    } else {
        BlendChoice::Normal
    }
}
use crate::mask::{ClipGeom, ClipMask, build_clip_mask_cancellable};
use pdf_page_ir::DeviceRect;

use crate::prepared::{CpuPreparedPage, DrawClass, PreparedCommand, PreparedGlyphRun, PreparedOp};
use crate::raster::{CovRun, RasterKernel};
use crate::stats::RenderStats;
use crate::surface::{Surface, unique_arc_mut, zeroed_arc};
use rayon::prelude::*;

/// Per-worker scratch: the coverage kernel and the kernel-set table. Reusable
/// geometry buffers live on the prepared page and the kernel; nothing here
/// allocates per command after warm-up (advice §13).
#[derive(Debug)]
pub struct CpuWorkerContext {
    raster: RasterKernel,
    kernels: KernelSet,
    pub(crate) fonts: crate::prepared::FontProgramCache,
    /// The previous render's page buffer, kept so the next render of the same
    /// geometry repaints resident memory instead of faulting in a fresh
    /// mapping. At most one buffer, always the most recent page's size, and
    /// only ever handed back out when its consumer has dropped it — so this
    /// retains no more than the one page surface the worker was already sized
    /// for, and nothing at all until a page has been rendered.
    pub(crate) surface_buffer: Option<std::sync::Arc<[u8]>>,
}

impl Default for CpuWorkerContext {
    fn default() -> Self {
        Self {
            raster: RasterKernel::default(),
            kernels: KernelSet::select(),
            fonts: crate::prepared::FontProgramCache::default(),
            surface_buffer: None,
        }
    }
}

impl CpuWorkerContext {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Execute a page into `surface`, rendering transparency groups into bounded
/// offscreen surfaces that composite back (advice §9).
pub fn execute(
    page: &CpuPreparedPage,
    surface: &mut Surface,
    ctx: &mut CpuWorkerContext,
    stats: &mut RenderStats,
) {
    #[cfg(feature = "profiling")]
    let start = Instant::now();
    let _in_flight = PageInFlight::enter();
    let CpuWorkerContext {
        raster, kernels, ..
    } = ctx;
    // Region-local clip masks, built once per page and indexed by dense
    // ClipId (advice §8 — no hash map in the draw loop).
    let mut masks: Vec<Option<ClipMask>> = (0..page.clips.len()).map(|_| None).collect();
    // Clip-dense pages (basin maps, music notation) spend nearly all their time
    // rasterizing hundreds of large independent path-clip masks. Each mask is a
    // pure function of the page and is byte-identical regardless of build order,
    // so pre-build every consumed mask across the rayon pool before the serial
    // draw loop. The draw loop then finds them cached. Peak memory is unchanged
    // (every consumed mask is cached by page end anyway); this only front-loads
    // and parallelizes the build.
    prebuild_masks(page, &mut masks);
    // Active soft masks (top = current); `None` entry = /SMask /None.
    let mut soft: Vec<Option<ClipMask>> = Vec::new();
    run_ops(
        page,
        0,
        page.ops.len(),
        surface,
        raster,
        kernels,
        &mut masks,
        &mut soft,
        stats,
    );
    #[cfg(feature = "profiling")]
    {
        stats.raster += start.elapsed();
    }
}

/// Derive every top-level page soft mask once for the experimental image-page
/// preparation seam. Mask content still runs through the normative CPU
/// executor, including nested masks, groups, clips, `/BC`, and `/TR`; callers
/// can then attach the resulting device-space coverage plane to GPU image
/// draws without teaching the GPU backend how to paint arbitrary mask content.
///
/// The returned vector is indexed like `page.ops`; only `PushSoftMask` slots
/// are populated. `None` for the whole result means cooperative cancellation.
pub(crate) fn prepare_soft_masks(page: &CpuPreparedPage) -> Option<Vec<Option<ClipMask>>> {
    let mut prepared: Vec<Option<ClipMask>> = std::iter::repeat_with(|| None)
        .take(page.ops.len())
        .collect();
    let mut raster = RasterKernel::default();
    let kernels = KernelSet::select();
    let mut masks: Vec<Option<ClipMask>> = (0..page.clips.len()).map(|_| None).collect();
    let mut stats = RenderStats::default();
    let mut i = 0;

    while i < page.ops.len() {
        if let Some(cancel) = &page.decode_limits.should_cancel
            && cancel()
        {
            return None;
        }
        match &page.ops[i] {
            PreparedOp::PushSoftMask {
                kind,
                transfer,
                content_end,
                bounds,
            } => {
                let content_end = *content_end as usize;
                prepared[i] = Some(render_soft_mask(
                    page,
                    i + 1,
                    content_end,
                    *kind,
                    transfer.as_deref(),
                    *bounds,
                    &mut raster,
                    &kernels,
                    &mut masks,
                    &mut stats,
                ));
                if stats.cancelled {
                    return None;
                }
                i = content_end;
            }
            _ => i += 1,
        }
    }

    Some(prepared)
}

/// Rasterize one patterned `/ImageMask` brush without its page-level alpha or
/// blend. The resulting bounded plane is straight RGB plus independent alpha,
/// ready for the experimental GPU image compositor to apply painter-order
/// state. `None` means cancellation or a degraded nested pattern-cell draw;
/// callers then route the complete request through the normative CPU backend
/// so diagnostics cannot disappear at the GPU seam.
pub(crate) fn prepare_patterned_stencil(
    page: &CpuPreparedPage,
    tiling: &crate::prepared::PreparedTiling,
) -> Option<crate::PreparedPatternedStencil> {
    tiling.stencil.as_ref()?;
    let mut normalized = tiling.clone();
    normalized.alpha = 255;
    normalized.blend = pdf_page_ir::BlendMode::Normal;

    let mut surface = Surface::offscreen(normalized.bounds);
    let mut raster = RasterKernel::default();
    let kernels = KernelSet::select();
    let mut masks: Vec<Option<ClipMask>> = (0..page.clips.len()).map(|_| None).collect();
    let mut stats = RenderStats::default();
    render_tiling(
        page,
        &normalized,
        &mut surface,
        &mut raster,
        &kernels,
        &mut masks,
        &mut stats,
    );
    if stats.cancelled || stats.degraded_draws > 0 {
        return None;
    }

    let (_, rgba) = surface.into_output(pdf_render_api::OutputFormat::Rgba8PremultipliedSrgb);
    let pixels = normalized.bounds.width as usize * normalized.bounds.height as usize;
    let mut samples = zeroed_arc(pixels * 3);
    let mut opacity = zeroed_arc(pixels);
    let samples_data = unique_arc_mut(&mut samples);
    let opacity_data = unique_arc_mut(&mut opacity);
    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        let alpha = u16::from(pixel[3]);
        opacity_data[index] = pixel[3];
        let sample = &mut samples_data[index * 3..index * 3 + 3];
        if alpha == 0 {
            sample.fill(0);
        } else {
            for (dst, &channel) in sample.iter_mut().zip(&pixel[..3]) {
                *dst = ((u16::from(channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Some(crate::PreparedPatternedStencil {
        bounds: normalized.bounds,
        samples,
        opacity,
    })
}

/// Pre-build every clip mask the ops will consume, across the rayon pool, so the
/// serial draw loop finds them cached. Each mask is a pure function of `page`
/// and byte-identical regardless of build order, so this is safe to parallelize
/// and never changes output. Engaged only past a threshold where the fan-out
/// pays for itself; below it the draw loop builds the few masks lazily.
fn prebuild_masks(page: &CpuPreparedPage, masks: &mut [Option<ClipMask>]) {
    // Cooperative cancellation: skip the (bounded) pre-pass if already cancelled.
    if let Some(cancel) = &page.decode_limits.should_cancel
        && cancel()
    {
        return;
    }
    // The distinct mask-source clips actually drawn under (a mask pushed but
    // never consumed is never built — matching the lazy loop's behavior).
    let mut wanted = vec![false; page.clips.len()];
    for op in &page.ops {
        let clip = match op {
            PreparedOp::Draw(c) if c.clip_has_mask => c.clip,
            PreparedOp::GlyphRun(g) if g.clip_has_mask => g.clip,
            PreparedOp::Image(i) if i.clip_has_mask => i.clip,
            PreparedOp::TiledFill(t) if t.clip_has_mask => t.clip,
            _ => None,
        };
        if let Some(cid) = clip
            && let Some(src) = page.clips[cid as usize].mask_source
        {
            wanted[src as usize] = true;
        }
    }
    let ids: Vec<u32> = (0..page.clips.len() as u32)
        .filter(|&i| wanted[i as usize])
        .collect();
    // Below this count, rayon fan-out costs more than it saves.
    const PARALLEL_MASK_THRESHOLD: usize = 16;
    if ids.len() < PARALLEL_MASK_THRESHOLD {
        return;
    }
    // One reused scratch kernel per worker thread (grows on first fill).
    let geom = ClipGeom::of(page);
    let should_cancel = page.decode_limits.should_cancel.as_deref();
    let built: Vec<(u32, ClipMask)> = ids
        .par_iter()
        .map_init(RasterKernel::default, |rk, &cid| {
            build_clip_mask_cancellable(rk, geom, cid, should_cancel).map(|mask| (cid, mask))
        })
        .filter_map(|built| built)
        .collect();
    for (cid, m) in built {
        masks[cid as usize] = Some(m);
    }
}

/// Execute ops `[start, end)` into `surface`. `BeginGroup` recurses into a
/// fresh offscreen surface and composites back; `PushSoftMask` renders its
/// content offscreen, derives a per-pixel mask, and pushes it.
#[allow(clippy::too_many_arguments)]
fn run_ops(
    page: &CpuPreparedPage,
    start: usize,
    end: usize,
    surface: &mut Surface,
    raster: &mut RasterKernel,
    kernels: &KernelSet,
    masks: &mut Vec<Option<ClipMask>>,
    soft: &mut Vec<Option<ClipMask>>,
    stats: &mut RenderStats,
) {
    let mut i = start;
    while i < end {
        // Cooperative mid-render cancellation (roadmap §4.4): checked at op
        // boundaries every 16 commands, so a cancelled viewer request stops
        // within a bounded slice of work instead of at page completion.
        if i.trailing_zeros() >= 4
            && let Some(cancel) = &page.decode_limits.should_cancel
            && cancel()
        {
            stats.cancelled = true;
            return;
        }
        match &page.ops[i] {
            PreparedOp::Draw(cmd) => {
                let active_soft = soft.last().and_then(Option::as_ref);
                paint_command(
                    page,
                    cmd,
                    surface,
                    raster,
                    kernels,
                    masks,
                    active_soft,
                    stats,
                );
                i += 1;
            }
            PreparedOp::GlyphRun(gr) => {
                let active_soft = soft.last().and_then(Option::as_ref);
                paint_glyph_run(
                    page,
                    gr,
                    surface,
                    raster,
                    kernels,
                    masks,
                    active_soft,
                    stats,
                );
                i += 1;
            }
            PreparedOp::TiledFill(t) => {
                render_tiling(page, t, surface, raster, kernels, masks, stats);
                i += 1;
            }
            PreparedOp::Image(img) => {
                let active_soft = soft.last().and_then(Option::as_ref);
                paint_image(page, img, surface, raster, masks, active_soft, stats);
                i += 1;
            }
            PreparedOp::BeginGroup {
                group,
                end: group_end,
            } => {
                let ge = *group_end as usize;
                if group.bounds.width == 0 || group.bounds.height == 0 {
                    i = ge + 1;
                    continue;
                }
                // Group backdrop (ISO 32000-1 §11.4.7): an *isolated* group
                // starts transparent; a *non-isolated* group's elements
                // composite against the backdrop behind it. We seed the
                // backdrop only for a non-isolated group whose own composite
                // blend is Normal (the common wrapper case) — there the
                // seeded offscreen is composited back by a plain replace/lerp,
                // which is exact for an opaque backdrop and cannot double-count
                // it. A non-isolated group with a *non-Normal* composite blend
                // (e.g. a Multiply group) is still rendered on a transparent
                // offscreen (its content isolated) and composited with that
                // blend against the parent — which, when the parent is the page
                // or an already-seeded non-isolated wrapper, now holds the real
                // backdrop, so `Multiply` finally hits the tone underneath
                // instead of transparency (the Young-Turks gold, the Medieval
                // page tone). This is Stage 1: full non-isolated compositing
                // with backdrop removal for a non-Normal group whose *content*
                // carries its own non-Normal blends remains approximated.
                let blend = choose_blend(group.blend);
                // The soft mask in force at the invocation modulates the
                // *group's* composite, not its contents (§11.6.6 — lowering
                // pushes a None reset inside the group for that reason). Its
                // stack slot is stable across the group's ops, which are
                // balanced, so remember the depth rather than borrowing across
                // the `run_ops` mutable borrow.
                let soft_slot = soft.len().checked_sub(1);
                let seed = !group.isolated && matches!(blend, BlendChoice::Normal);
                let mut off = if seed {
                    Surface::offscreen_seeded(surface, group.bounds)
                } else {
                    Surface::offscreen(group.bounds)
                };
                if group.knockout {
                    run_knockout_group(
                        page,
                        i + 1,
                        ge,
                        &mut off,
                        raster,
                        kernels,
                        masks,
                        soft,
                        stats,
                    );
                } else {
                    run_ops(
                        page,
                        i + 1,
                        ge,
                        &mut off,
                        raster,
                        kernels,
                        masks,
                        soft,
                        stats,
                    );
                }
                let group_soft = soft_slot
                    .and_then(|ix| soft.get(ix))
                    .and_then(Option::as_ref);
                if seed {
                    // Seeded offscreen already holds backdrop∘content; fold it
                    // back at group opacity without re-compositing the backdrop.
                    composite_group_seeded(surface, &off, group.opacity, group_soft);
                } else {
                    composite_group(surface, &off, group.opacity, blend, group_soft);
                }
                stats.transparency_groups += 1;
                i = ge + 1;
            }
            PreparedOp::EndGroup => i += 1,
            PreparedOp::PushSoftMask {
                kind,
                transfer,
                content_end,
                bounds,
            } => {
                let ce = *content_end as usize;
                let mask = render_soft_mask(
                    page,
                    i + 1,
                    ce,
                    *kind,
                    transfer.as_deref(),
                    *bounds,
                    raster,
                    kernels,
                    masks,
                    stats,
                );
                soft.push(Some(mask));
                stats.soft_masks += 1;
                i = ce;
            }
            PreparedOp::PushSoftMaskNone => {
                soft.push(None);
                i += 1;
            }
            PreparedOp::PopSoftMask => {
                soft.pop();
                i += 1;
            }
        }
        if stats.cancelled {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_soft_mask(
    page: &CpuPreparedPage,
    content_start: usize,
    content_end: usize,
    kind: pdf_page_ir::MaskKind,
    transfer: Option<&[u8; 256]>,
    bounds: DeviceRect,
    raster: &mut RasterKernel,
    kernels: &KernelSet,
    masks: &mut Vec<Option<ClipMask>>,
    stats: &mut RenderStats,
) -> ClipMask {
    if bounds.width == 0 || bounds.height == 0 {
        // An empty real mask is not `/SMask /None`: its outside value still
        // gates every subsequent pixel. Keeping an explicit zero-sized mask
        // also fixes the formerly inverted Alpha/plain-Luminosity empty-mask
        // case, where `None` accidentally disabled masking altogether.
        return ClipMask {
            bounds,
            data: Vec::new(),
            outside: apply_transfer(soft_mask_outside(kind), transfer),
            all_opaque: false,
        };
    }

    let mut off = Surface::offscreen(bounds);
    // A /BC luminosity mask composites its content against the backdrop color,
    // not transparent black (§11.6.5.2).
    if let pdf_page_ir::MaskKind::LuminosityBc { backdrop } = kind {
        for ly in 0..off.height {
            let row = off.row_mut(off.origin_y + ly);
            for px in row.chunks_exact_mut(4) {
                px[0] = backdrop[0];
                px[1] = backdrop[1];
                px[2] = backdrop[2];
                px[3] = 255;
            }
        }
    }
    // The mask content renders isolated from any outer soft mask.
    let mut inner: Vec<Option<ClipMask>> = Vec::new();
    run_ops(
        page,
        content_start,
        content_end,
        &mut off,
        raster,
        kernels,
        masks,
        &mut inner,
        stats,
    );
    derive_soft_mask(&off, kind, transfer)
}

fn build_render_clip_mask(
    page: &CpuPreparedPage,
    raster: &mut RasterKernel,
    cid: u32,
    stats: &mut RenderStats,
) -> Option<ClipMask> {
    let mask = build_clip_mask_cancellable(
        raster,
        ClipGeom::of(page),
        cid,
        page.decode_limits.should_cancel.as_deref(),
    );
    if mask.is_none() {
        stats.cancelled = true;
    }
    mask
}

/// Execute a *knockout* group's ops `[start, end)` into `off` (ISO 32000-1
/// §11.6.6): every painting element composites against the group's **initial**
/// backdrop rather than the accumulated result, later elements replacing
/// earlier ones where they overlap.
///
/// Implementation: `initial` snapshots the offscreen; each top-level element
/// (a draw, or a whole nested group/tiled fill) renders onto a fresh copy of
/// `initial`, and every pixel the element changed is copied into the
/// accumulator (`off`). Diffing against `initial` detects "touched": an
/// element that writes exactly the backdrop value leaves the accumulator
/// unchanged, which is value-identical. Soft-mask push/pop ops are graphics
/// *state*, not elements — they execute once, in order, and stay active for
/// the following elements.
#[allow(clippy::too_many_arguments)]
fn run_knockout_group(
    page: &CpuPreparedPage,
    start: usize,
    end: usize,
    off: &mut Surface,
    raster: &mut RasterKernel,
    kernels: &KernelSet,
    masks: &mut Vec<Option<ClipMask>>,
    soft: &mut Vec<Option<ClipMask>>,
    stats: &mut RenderStats,
) {
    let initial = off.clone();
    let mut scratch = initial.clone();
    let mut scratch_is_initial = true;
    let mut i = start;
    while i < end {
        // One element's op extent, and whether it paints (vs pure state).
        let (next, paints) = match &page.ops[i] {
            PreparedOp::Draw(_)
            | PreparedOp::GlyphRun(_)
            | PreparedOp::TiledFill(_)
            | PreparedOp::Image(_) => (i + 1, true),
            PreparedOp::BeginGroup { end: ge, .. } => (*ge as usize + 1, true),
            PreparedOp::PushSoftMask { content_end, .. } => (*content_end as usize, false),
            PreparedOp::PushSoftMaskNone | PreparedOp::PopSoftMask | PreparedOp::EndGroup => {
                (i + 1, false)
            }
        };
        if paints {
            if !scratch_is_initial {
                scratch.copy_pixels_from(&initial);
            }
            run_ops(
                page,
                i,
                next,
                &mut scratch,
                raster,
                kernels,
                masks,
                soft,
                stats,
            );
            scratch_is_initial = false;
            // Fold changed pixels into the accumulator.
            let (gw, gh) = (off.width, off.height);
            for ly in 0..gh {
                let srow = scratch.local_row(ly);
                let irow = initial.local_row(ly);
                let orow = off.row_mut(initial.origin_y + ly);
                for lx in 0..gw {
                    let r = lx * 4..lx * 4 + 4;
                    if srow[r.clone()] != irow[r.clone()] {
                        orow[r.clone()].copy_from_slice(&srow[r]);
                    }
                }
            }
        } else {
            // State op: execute against the accumulator (it paints nothing).
            run_ops(page, i, next, off, raster, kernels, masks, soft, stats);
        }
        i = next;
    }
}

/// Blit an image: sample every device pixel in its bounds back through the
/// inverse transform, apply the clip + soft mask, and composite — source-over
/// on the fast paths (Normal blend), the general per-pixel compositor for a
/// non-Normal image blend.
#[allow(clippy::too_many_arguments)]
fn paint_image(
    page: &CpuPreparedPage,
    img: &crate::image::PreparedImage,
    surface: &mut Surface,
    raster: &mut RasterKernel,
    masks: &mut [Option<ClipMask>],
    soft: Option<&ClipMask>,
    stats: &mut RenderStats,
) {
    #[cfg(feature = "profiling")]
    let image_start = Instant::now();
    stats.commands += 1;
    // A minified hard stencil is coverage-averaged, as a one-bit soft mask.
    let coverage_masked;
    let img = match img.minified_stencil_as_coverage() {
        Some(promoted) => {
            coverage_masked = promoted;
            &coverage_masked
        }
        None => img,
    };
    let cmask_cid = if img.clip_has_mask {
        // Lowering guarantees a masked draw carries its clip id; a missing id
        // means the IR invariant broke upstream. Skip the draw (never panic a
        // page) and record the degradation.
        let Some(cid) = img.clip else {
            page.diagnostics
                .note_degraded("image draw skipped: masked clip without a clip id".into());
            return;
        };
        // Share the nearest path-clip ancestor's mask across rect descendants.
        let cid = page.clips[cid as usize].mask_source.unwrap_or(cid);
        if masks[cid as usize].is_none() {
            let Some(mask) = build_render_clip_mask(page, raster, cid, stats) else {
                return;
            };
            masks[cid as usize] = Some(mask);
        }
        Some(cid as usize)
    } else {
        None
    };
    let raw_cmask = cmask_cid.and_then(|cid| masks[cid].as_ref());
    // Path clips that rasterize to full coverage over their envelope are
    // semantically rectangular for this draw. Avoid a mask lookup and
    // multiply for every destination pixel once that is proven.
    let opaque_clip_elided = raw_cmask.is_some_and(|cm| cm.all_opaque);
    let cmask = raw_cmask.filter(|_| !opaque_clip_elided);

    let b = img.bounds;
    let (x0, y0) = (b.x as usize, b.y as usize);
    let x1 = (b.x + b.width as i32) as usize;
    let y1 = (b.y + b.height as i32) as usize;
    // Pixels this image actually marks. `covered_pixels` feeds
    // `is_silent_blank()`; an image-only page that DID paint must not be
    // mislabeled a silent blank just because no `SolidPath` ran.
    let mut covered: u64 = 0;
    #[cfg(feature = "profiling")]
    let mut sample_attempts: u64 = 0;
    #[cfg(feature = "profiling")]
    let mut sample_taps: u64 = 0;
    #[cfg(feature = "profiling")]
    let mut fast_cmyk_area_used = false;
    // Non-Normal image blend takes the generic per-pixel path with the
    // general compositor; every fast path below is source-over.
    let blend = choose_blend(img.blend);
    let blend_is_normal = matches!(blend, BlendChoice::Normal);
    // Eight-bit gray sources (book scans, MRC pages) are one byte per texel
    // where every fast path below reads three. Promote the axis-aligned
    // opaque ones to RGB8 once so they qualify; sampling the promoted image
    // is byte-identical (see `PreparedImage::gray8_promoted_to_rgb8`). A
    // magnified draw under a mask has no fast path either way, so it is not
    // expanded for nothing.
    let promoted_gray;
    let img = if blend_is_normal
        && cmask.is_none()
        && soft.is_none()
        && img.alpha == 255
        && img.inv.b == 0.0
        && img.inv.c == 0.0
        && matches!(img.color_space, pdf_page_ir::ImageColorSpace::Gray)
        && (img.footprint[0] > 1.0
            || img.footprint[1] > 1.0
            || (img.smask.is_none() && img.mask.is_none()))
    {
        match img.gray8_promoted_to_rgb8() {
            Some(rgb) => {
                promoted_gray = rgb;
                &promoted_gray
            }
            None => img,
        }
    } else {
        img
    };
    // Indexed (palette) sources are one byte (or less) per texel resolved
    // through a palette, which no fast path reads. Expand them to RGB8 once
    // through the palette; sampling the promoted image is byte-identical (see
    // `PreparedImage::indexed_promoted_to_rgb8`), and every path from here on —
    // the axis-aligned fast paths, the masked MRC path, and the generic
    // sampler's RGB8 byte-sum branch — is then open to it. The expansion is
    // one pass over the source; any sampling of the original costs at least a
    // palette resolution per tap, and the taps are never fewer than the texels
    // a draw touches, so it pays for itself on every geometry. A colour-key
    // mask keys on the raw palette indices and keeps the original.
    //
    // Lowering already performs this expansion behind the content-keyed image
    // cache (`prepared.rs::promote_through_cache`), so a lowered image arrives
    // here as RGB8 and only images built elsewhere expand per draw.
    let promoted_indexed;
    let img = if matches!(
        img.color_space,
        pdf_page_ir::ImageColorSpace::Indexed { .. }
    ) && !matches!(img.mask, Some(pdf_page_ir::ImageMask::ColorKey(_)))
    {
        #[cfg(feature = "profiling")]
        let promote_start = Instant::now();
        let promoted = img.indexed_promoted_to_rgb8();
        #[cfg(feature = "profiling")]
        stats
            .profile
            .add_duration("render.image.indexed_promotion", promote_start.elapsed());
        match promoted {
            Some(rgb) => {
                promoted_indexed = rgb;
                &promoted_indexed
            }
            None => img,
        }
    } else {
        img
    };
    let fast_rgb8_base = blend_is_normal
        && cmask.is_none()
        && soft.is_none()
        && img.alpha == 255
        && img.smask.is_none()
        && img.mask.is_none()
        && !img.is_stencil
        && img.bpc == 8
        && img.decode.is_none()
        && matches!(img.color_space, pdf_page_ir::ImageColorSpace::Rgb)
        && img.footprint[0] <= 1.0
        && img.footprint[1] <= 1.0
        && img.inv.b.abs() < 1e-12
        && img.inv.c.abs() < 1e-12
        && img.samples.len() >= img.width as usize * img.height as usize * 3;
    let fast_rgb8 = fast_rgb8_base && img.interpolation == pdf_page_ir::InterpolationMode::Nearest;
    // Magnified/1:1 continuous-tone JPEG/JPX is forced to Bilinear in
    // prepared.rs; without a dedicated path those draws fall into the generic
    // per-pixel bilinear loop (~tens of Mpix/s). Same axis-aligned eligibility
    // as nearest, with bilinear taps matching `PreparedImage::bilinear`.
    let fast_rgb8_bilinear =
        fast_rgb8_base && img.interpolation == pdf_page_ir::InterpolationMode::Bilinear;
    // The low-bit-depth twin of `fast_rgb8`: a 1-, 2- or 4-bit single-component
    // image — the bilevel JBIG2 or CCITT page of a scan, an MRC producer or
    // Lege's own output — drawn axis-aligned at or above its own resolution,
    // sampled through its prepared lookup table. Without it such a draw took
    // the generic per-pixel loop (~30 ns a pixel, 100 ms for a 1474×2400
    // page). Minified draws keep the summed-area `fast_binary` path below.
    let lut_nearest = if blend_is_normal
        && cmask.is_none()
        && soft.is_none()
        && img.alpha == 255
        && img.smask.is_none()
        && img.mask.is_none()
        && img.footprint[0] <= 1.0
        && img.footprint[1] <= 1.0
        && img.inv.b.abs() < 1e-12
        && img.inv.c.abs() < 1e-12
        && img.interpolation == pdf_page_ir::InterpolationMode::Nearest
    {
        img.lut_nearest_opaque()
    } else {
        None
    };
    // The area-minified twin of `fast_rgb8`: same eligibility, but the image is
    // minified on at least one axis (footprint > 1), so each destination pixel
    // box-averages its source footprint (what the generic path's
    // `area_average` does) instead of point-sampling. Off-diagonal inverse
    // terms must be *exactly* zero so per-column/row source boxes reproduce the
    // generic per-pixel `inv.apply` bit-for-bit. Interpolation is not checked:
    // minification ignores it (`shade` area-averages regardless of the
    // Nearest/Bilinear hint), so a minified Bilinear draw lands here too.
    let rgb8_area_shape = blend_is_normal
        && cmask.is_none()
        && soft.is_none()
        && img.alpha == 255
        && !img.is_stencil
        && img.bpc == 8
        && img.decode.is_none()
        && matches!(img.color_space, pdf_page_ir::ImageColorSpace::Rgb)
        && (img.footprint[0] > 1.0 || img.footprint[1] > 1.0)
        && img.inv.b == 0.0
        && img.inv.c == 0.0
        && img.samples.len() >= img.width as usize * img.height as usize * 3;
    let fast_rgb8_area = rgb8_area_shape && img.smask.is_none() && img.mask.is_none();
    // The MRC shape: the same opaque axis-aligned minified RGB8 source, but cut
    // out by a one-bit JBIG2 layer. Without this the draw falls into the
    // generic per-pixel loop and pays a fresh inverse map, two base tap boxes,
    // and a mask lookup for every destination pixel — all of which depend only
    // on the destination column or row once the placement is axis-aligned.
    let area_min_mask = if rgb8_area_shape {
        area_min_mask(img)
    } else {
        None
    };
    // The same, for an opaque axis-aligned minified CMYK image: the source is
    // converted to RGB8 once and then box-averaged identically. Eligibility
    // mirrors `fast_rgb8_area` but for the CMYK colour space; the sample-length
    // and size checks live in `cmyk_source_as_rgb8`.
    let fast_cmyk_area = blend_is_normal
        && cmask.is_none()
        && soft.is_none()
        && img.alpha == 255
        && img.smask.is_none()
        && img.mask.is_none()
        && !img.is_stencil
        && img.bpc == 8
        && img.decode.is_none()
        && matches!(img.color_space, pdf_page_ir::ImageColorSpace::Cmyk)
        && (img.footprint[0] > 1.0 || img.footprint[1] > 1.0)
        && img.inv.b == 0.0
        && img.inv.c == 0.0;
    let fast_binary = blend_is_normal && soft.is_none() && img.is_binary_box_filterable();
    // An `/ImageMask` stencil in solid colour is a one-bit image whose
    // painting bit is opaque and whose masking bit is transparent, so it takes
    // the bilevel box filter when minified and a transparent-skip nearest walk
    // otherwise, on the interior rectangle; the border band stays generic.
    // The placement must be axis-aligned with off-diagonal terms exactly zero
    // (that is what makes the per-axis tables and the interior test reproduce
    // the generic `inv.apply`), and a magnified Bilinear stencil keeps the
    // generic sampler, whose four-tap blend this path does not reproduce.
    let stencil_lut = if blend_is_normal && soft.is_none() {
        stencil_fast_lut(img)
    } else {
        None
    };

    if fast_rgb8 {
        let result = paint_axis_aligned_rgb8_nearest_opaque(img, surface, x0, y0, x1, y1);
        covered = result.0;
        #[cfg(feature = "profiling")]
        {
            sample_attempts = (x1 - x0) as u64 * (y1 - y0) as u64;
            sample_taps = result.1;
        }
    } else if fast_rgb8_bilinear {
        let result = paint_axis_aligned_rgb8_bilinear_opaque(img, surface, x0, y0, x1, y1);
        covered = result.0;
        #[cfg(feature = "profiling")]
        {
            sample_attempts = (x1 - x0) as u64 * (y1 - y0) as u64;
            sample_taps = result.1;
        }
    } else if let Some(lut) = lut_nearest {
        let result = paint_axis_aligned_lut_nearest_opaque(img, lut, surface, x0, y0, x1, y1);
        covered = result.0;
        #[cfg(feature = "profiling")]
        {
            sample_attempts = (x1 - x0) as u64 * (y1 - y0) as u64;
            sample_taps = result.1;
        }
    } else if fast_rgb8_area {
        let result = paint_axis_aligned_rgb8_area_min_opaque(img, surface, x0, y0, x1, y1);
        covered = result.0;
        #[cfg(feature = "profiling")]
        {
            sample_attempts = (x1 - x0) as u64 * (y1 - y0) as u64;
            sample_taps = result.1;
        }
    } else if let Some(mask) = area_min_mask {
        let result = paint_axis_aligned_rgb8_area_min_masked(img, mask, surface, x0, y0, x1, y1);
        covered = result.0;
        #[cfg(feature = "profiling")]
        {
            sample_attempts = (x1 - x0) as u64 * (y1 - y0) as u64;
            sample_taps = result.1;
        }
    } else if let Some(result) = fast_cmyk_area
        .then(|| paint_axis_aligned_cmyk_area_min_opaque(img, surface, x0, y0, x1, y1))
        .flatten()
    {
        covered = result.0;
        #[cfg(feature = "profiling")]
        {
            fast_cmyk_area_used = true;
            sample_attempts = (x1 - x0) as u64 * (y1 - y0) as u64;
            sample_taps = result.1;
        }
    } else if let Some(lut) = fast_binary.then(|| img.binary_box_lut()).flatten() {
        let result = paint_axis_aligned_binary_box(img, lut, surface, cmask, x0, y0, x1, y1);
        covered = result.0;
        #[cfg(feature = "profiling")]
        {
            sample_attempts = result.1;
            sample_taps = result.2;
        }
    } else if let Some(lut) = stencil_lut {
        let totals = paint_axis_aligned_stencil(img, lut, surface, cmask, blend, x0, y0, x1, y1);
        covered = totals.0;
        #[cfg(feature = "profiling")]
        {
            sample_attempts = totals.1;
            sample_taps = totals.2;
        }
        #[cfg(not(feature = "profiling"))]
        let _ = (totals.1, totals.2);
    } else {
        let totals = paint_image_generic(img, surface, cmask, soft, blend, x0, y0, x1, y1, None);
        covered += totals.0;
        #[cfg(feature = "profiling")]
        {
            sample_attempts += totals.1;
            sample_taps += totals.2;
        }
        #[cfg(not(feature = "profiling"))]
        let _ = (totals.1, totals.2);
    }
    stats.covered_pixels += covered;
    stats.ops_painted += 1;
    #[cfg(feature = "profiling")]
    {
        let profile = &mut stats.profile;
        profile.add_duration("render.image", image_start.elapsed());
        profile.increment("image.draws", 1);
        profile.increment(
            "image.destination_pixels",
            (x1 - x0) as u64 * (y1 - y0) as u64,
        );
        profile.increment("image.sample_attempts", sample_attempts);
        profile.increment("image.source_sample_taps", sample_taps);
        profile.increment("image.painted_pixels", covered);
        if fast_rgb8 {
            profile.increment("image.fast_rgb8_nearest_pixels", covered);
        }
        if fast_rgb8_bilinear {
            profile.increment("image.fast_rgb8_bilinear_pixels", covered);
        }
        if lut_nearest.is_some() {
            profile.increment("image.fast_lut_nearest_pixels", covered);
        }
        if fast_rgb8_area {
            profile.increment("image.fast_rgb8_area_min_pixels", covered);
            profile.add_duration("render.image.area_min", image_start.elapsed());
        } else if let Some(mask) = area_min_mask {
            profile.increment("image.fast_rgb8_area_min_masked_pixels", covered);
            profile.add_duration("render.image.area_min_masked", image_start.elapsed());
            profile.increment(
                match mask {
                    AreaMinMask::Soft(_) => "image.area_min_masked.soft1_draws",
                    AreaMinMask::Soft8(_) => "image.area_min_masked.soft8_draws",
                    AreaMinMask::Stencil(_) => "image.area_min_masked.stencil_draws",
                },
                1,
            );
        }
        if fast_cmyk_area_used {
            profile.increment("image.fast_cmyk_area_min_pixels", covered);
        }
        if fast_binary {
            profile.increment("image.fast_binary_box_pixels", covered);
        }
        if stencil_lut.is_some() {
            profile.increment("image.fast_stencil_pixels", covered);
        }
        if opaque_clip_elided {
            profile.increment("image.opaque_clip_elisions", 1);
        }
        if img.footprint[0] > 1.0 || img.footprint[1] > 1.0 {
            profile.increment("image.minified_draws", 1);
            profile.increment("image.area_average_pixels", sample_attempts);
        } else {
            profile.increment("image.magnified_or_1to1_draws", 1);
            match img.interpolation {
                pdf_page_ir::InterpolationMode::Nearest => {
                    profile.increment("image.nearest_pixels", sample_attempts);
                }
                pdf_page_ir::InterpolationMode::Bilinear => {
                    profile.increment("image.bilinear_pixels", sample_attempts);
                }
            }
        }
        match &img.color_space {
            pdf_page_ir::ImageColorSpace::Gray => profile.increment("image.gray_draws", 1),
            pdf_page_ir::ImageColorSpace::Rgb => profile.increment("image.rgb_draws", 1),
            pdf_page_ir::ImageColorSpace::Cmyk => profile.increment("image.cmyk_draws", 1),
            pdf_page_ir::ImageColorSpace::Indexed { .. } => {
                profile.increment("image.indexed_draws", 1)
            }
            pdf_page_ir::ImageColorSpace::TintLut { .. }
            | pdf_page_ir::ImageColorSpace::TintLut2 { .. } => {
                profile.increment("image.tint_lut_draws", 1)
            }
            pdf_page_ir::ImageColorSpace::Lab { .. } => profile.increment("image.lab_draws", 1),
            pdf_page_ir::ImageColorSpace::IccRgb { .. } => {
                profile.increment("image.icc_rgb_draws", 1)
            }
            pdf_page_ir::ImageColorSpace::IccCmyk { .. } => {
                profile.increment("image.icc_cmyk_draws", 1)
            }
        }
        if img.is_stencil {
            profile.increment("image.stencil_draws", 1);
        }
        if img.smask.is_some() || img.mask.is_some() {
            profile.increment("image.resource_masked_draws", 1);
        }
        if img.inv.b.abs() < 1e-12 && img.inv.c.abs() < 1e-12 {
            profile.increment("image.axis_aligned_draws", 1);
        } else {
            profile.increment("image.general_affine_draws", 1);
        }
        if img.clip_has_mask {
            profile.increment("image.clip_masked_draws", 1);
        }
        if soft.is_some() {
            profile.increment("image.soft_masked_draws", 1);
        }
    }
}

/// The generic per-pixel image body: the clip and soft-clip coverage, A10
/// image-edge anti-aliasing, the full `shade()` sampler and the general
/// compositor, for every destination pixel of `[x0, x1) × [y0, y1)`.
///
/// `hole`, as `[hx0, hx1, hy0, hy1]`, is a rectangle of whole destination
/// columns and rows a fast path has already painted; it is skipped. The fast
/// paths that pass one paint exactly the pixels whose `edge_coverage` is
/// `None` there, so the anti-aliased border band around it is still drawn by
/// this body, and the draw stays byte-identical to running it everywhere.
///
/// Returns `(covered, sample_attempts, sample_taps)`; the last two stay zero
/// unless the profiling counters are compiled in.
#[allow(clippy::too_many_arguments)]
fn paint_image_generic(
    img: &crate::image::PreparedImage,
    surface: &mut Surface,
    cmask: Option<&ClipMask>,
    soft: Option<&ClipMask>,
    blend: BlendChoice,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    hole: Option<[usize; 4]>,
) -> (u64, u64, u64) {
    // Mask geometry is fixed for the whole draw; resolve it once.
    let clip_win = cmask.map(ClipMask::clip_window);
    let soft_win = soft.map(ClipMask::soft_window);
    let ox = surface.origin_x;
    // One destination pixel. Every term is a pure function of the source, the
    // masks and this pixel's own device coordinate, and the only write is
    // into its own row. Adds to `(covered, sample_attempts, sample_taps)`.
    let paint_px = |x: usize, y: usize, row: &mut [u8], totals: &mut (u64, u64, u64)| {
        // Clip mask coverage.
        let mut cov: u16 = 255;
        if let Some(cw) = &clip_win {
            cov = cw.coverage(x, y);
        }
        if let Some(sw) = &soft_win {
            cov = mul_div_255(cov, sw.coverage(x, y));
        }
        if cov == 0 {
            return;
        }
        // A10 image-edge anti-aliasing: pixels straddling the image's
        // device-space quad edge paint at their fractional coverage
        // (`None` = provably interior, full weight — byte-identical
        // to the pre-A10 path there). Matches PDFium's softer
        // Type 3 / stencil edges (workstream H).
        let (dx, dy) = (x as f64 + 0.5, y as f64 + 0.5);
        let edge = img.edge_coverage(dx, dy);
        if edge == Some(0) {
            return;
        }
        #[cfg(feature = "profiling")]
        {
            totals.1 += 1;
        }
        let color = match edge {
            None => {
                #[cfg(feature = "profiling")]
                {
                    let (color, taps) = img.shade_profiled(dx, dy);
                    totals.2 += taps;
                    color
                }
                #[cfg(not(feature = "profiling"))]
                img.shade(dx, dy)
            }
            // Edge sliver: sample at the nearest in-quad point.
            Some(_) => img.shade_clamped(dx, dy),
        };
        let Some(color) = color else {
            return;
        };
        let cov = match edge {
            Some(ec) => mul_div_255(cov, ec),
            None => cov,
        };
        if cov == 0 {
            return;
        }
        let a = mul_div_255(mul_div_255(color[3] as u16, img.alpha as u16), cov);
        if a == 0 {
            return;
        }
        let px = &mut row[(x - ox) * 4..(x - ox) * 4 + 4];
        composite_px_blended(px, [color[0], color[1], color[2]], a as u8, blend);
        totals.0 += 1;
    };
    // One destination row: the rows are independent, so the per-pixel
    // arithmetic does not depend on how they are scheduled.
    let paint_row = |y: usize, row: &mut [u8]| -> (u64, u64, u64) {
        let mut totals = (0u64, 0u64, 0u64);
        match hole {
            Some([hx0, hx1, hy0, hy1]) if (hy0..hy1).contains(&y) => {
                for x in x0..hx0 {
                    paint_px(x, y, row, &mut totals);
                }
                for x in hx1..x1 {
                    paint_px(x, y, row, &mut totals);
                }
            }
            _ => {
                for x in x0..x1 {
                    paint_px(x, y, row, &mut totals);
                }
            }
        }
        totals
    };

    let (buf, first_abs_y, row_stride) = surface.rows_mut_abs(y0, y1);
    let parallel = image_row_parallel_enabled()
        && (x1 - x0).saturating_mul(y1 - y0) >= IMAGE_ROW_PAR_PIXEL_THRESHOLD;
    if parallel {
        buf.par_chunks_mut(row_stride)
            .enumerate()
            .map(|(i, row)| paint_row(first_abs_y + i, row))
            .reduce(|| (0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2))
    } else {
        buf.chunks_exact_mut(row_stride)
            .enumerate()
            .map(|(i, row)| paint_row(first_abs_y + i, row))
            .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2))
    }
}

/// The two-entry table under which `img`, an `/ImageMask` stencil, can take
/// [`paint_axis_aligned_stencil`], or `None` to keep the generic body. The
/// caller adds the draw-level terms (Normal blend, no soft clip).
fn stencil_fast_lut(img: &crate::image::PreparedImage) -> Option<([u8; 4], [u8; 4])> {
    let eligible = img.is_stencil
        && img.inv.b == 0.0
        && img.inv.c == 0.0
        && img.inv.a != 0.0
        && img.inv.d != 0.0
        && (img.footprint[0] > 1.0
            || img.footprint[1] > 1.0
            || img.interpolation == pdf_page_ir::InterpolationMode::Nearest);
    if eligible {
        img.stencil_bilevel_lut()
    } else {
        None
    }
}

/// The destination columns (or rows) `[lo, hi)` of an axis-aligned draw that
/// `PreparedImage::edge_coverage` classifies as interior along this axis:
/// the mapped pixel square, `t ± scale/2` with `t = scale·(i + 0.5) + offset`,
/// stays inside `[0, 1]`. With `inv.b == inv.c == 0` exactly, `edge_coverage`'s
/// bound and its `inv.apply` reduce to this per-axis expression bit for bit,
/// so a pixel is interior iff its column and its row both are. `t` is
/// monotone in `i`, so the set is one run; `None` when it is empty.
fn interior_span(scale: f64, offset: f64, lo: usize, hi: usize) -> Option<(usize, usize)> {
    let half = 0.5 * scale.abs();
    let inside = |i: usize| {
        let t = scale * (i as f64 + 0.5) + offset;
        t >= half && t <= 1.0 - half
    };
    let first = (lo..hi).find(|&i| inside(i))?;
    let end = (first..hi).find(|&i| !inside(i)).unwrap_or(hi);
    Some((first, end))
}

/// An axis-aligned `/ImageMask` stencil in solid colour, as a one-bit image
/// whose painting bit is `[stencil_rgb, 255]` and masking bit transparent
/// (`lut`, from `PreparedImage::stencil_bilevel_lut`). The interior rectangle,
/// where `edge_coverage` is `None`, takes the bilevel box filter when the draw
/// minifies, or a nearest walk that skips masked texels when it does not; the
/// anti-aliased border band around it keeps the generic per-pixel body. Both
/// fast paths compute the generic stencil sampler's integers term for term,
/// so the draw is byte-identical to the generic body. The caller has checked
/// Normal blend, no soft clip, `inv.b == inv.c == 0` and, for a draw that
/// does not minify, Nearest interpolation.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn paint_axis_aligned_stencil(
    img: &crate::image::PreparedImage,
    lut: ([u8; 4], [u8; 4]),
    surface: &mut Surface,
    cmask: Option<&ClipMask>,
    blend: BlendChoice,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64, u64) {
    let mut totals = (0u64, 0u64, 0u64);
    let span = interior_span(img.inv.a, img.inv.e, x0, x1)
        .zip(interior_span(img.inv.d, img.inv.f, y0, y1));
    let hole = span.map(|((hx0, hx1), (hy0, hy1))| {
        totals = if img.footprint[0] > 1.0 || img.footprint[1] > 1.0 {
            paint_axis_aligned_binary_box(img, lut, surface, cmask, hx0, hy0, hx1, hy1)
        } else {
            paint_axis_aligned_stencil_nearest(img, lut, surface, cmask, hx0, hy0, hx1, hy1)
        };
        [hx0, hx1, hy0, hy1]
    });
    let band = paint_image_generic(img, surface, cmask, None, blend, x0, y0, x1, y1, hole);
    (totals.0 + band.0, totals.1 + band.1, totals.2 + band.2)
}

/// The nearest-sampled interior of an axis-aligned stencil drawn at or above
/// its own resolution (see [`paint_axis_aligned_stencil`]). Every column and
/// row of `[x0, x1) × [y0, y1)` maps inside the image, so each picks its
/// source texel exactly as `shade()` does (`round`, clamped), reads its bit,
/// and skips it when it masks; a painting texel is `[stencil_rgb, 255]`
/// composited at the generic body's `alpha · clip` weight.
#[allow(clippy::too_many_arguments)]
fn paint_axis_aligned_stencil_nearest(
    img: &crate::image::PreparedImage,
    (zero, one): ([u8; 4], [u8; 4]),
    surface: &mut Surface,
    cmask: Option<&ClipMask>,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64, u64) {
    let columns: Vec<u32> = (x0..x1)
        .map(|x| {
            let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
            let fx = u * img.width as f64 - 0.5;
            (fx.round() as i64).clamp(0, img.width.saturating_sub(1) as i64) as u32
        })
        .collect();
    // The bit value that paints, and the paint colour (the table's opaque entry).
    let (paint_bit, paint) = if zero[3] != 0 { (0u8, zero) } else { (1u8, one) };
    let rgb = [paint[0], paint[1], paint[2]];
    let row_bytes = img.packed_row_bits() / 8;
    let samples: &[u8] = &img.samples;
    // `color[3]` is 255 for a painting texel, so the generic body's
    // `mul_div_255(255, alpha)` is `alpha`, kept in that form.
    let alpha = mul_div_255(255, img.alpha as u16);
    let clip_win = cmask.map(ClipMask::clip_window);
    let output_origin = surface.origin_x;
    let paint_row = |y: usize, row: &mut [u8]| -> (u64, u64, u64) {
        let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
        let fy = (1.0 - v) * img.height as f64 - 0.5;
        let source_row =
            (fy.round() as i64).clamp(0, img.height.saturating_sub(1) as i64) as usize;
        let source = &samples[source_row * row_bytes..][..row_bytes];
        let mut painted = 0u64;
        for (local_x, &col) in columns.iter().enumerate() {
            let bit = (source[col as usize >> 3] >> (7 - (col & 7))) & 1;
            if bit != paint_bit {
                continue;
            }
            let x = x0 + local_x;
            let cov = clip_win.as_ref().map_or(255, |cw| cw.coverage(x, y));
            let a = mul_div_255(alpha, cov);
            if a == 0 {
                continue;
            }
            let px = &mut row[(x - output_origin) * 4..][..4];
            if a == 255 {
                px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            } else {
                composite_px_blended(px, rgb, a as u8, BlendChoice::Normal);
            }
            painted += 1;
        }
        let attempts = columns.len() as u64;
        (painted, attempts, attempts)
    };
    let (output, first_output_y, output_stride) = surface.rows_mut_abs(y0, y1);
    let parallel = image_row_parallel_enabled()
        && (x1 - x0).saturating_mul(y1 - y0) >= IMAGE_ROW_PAR_PIXEL_THRESHOLD;
    if parallel {
        output
            .par_chunks_mut(output_stride)
            .enumerate()
            .map(|(i, row)| paint_row(first_output_y + i, row))
            .reduce(|| (0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2))
    } else {
        output
            .chunks_exact_mut(output_stride)
            .enumerate()
            .map(|(i, row)| paint_row(first_output_y + i, row))
            .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2))
    }
}

/// Fast path for an axis-aligned minified one-bit image. Device X and Y map
/// independently into source space, so prepare the inclusive source box for
/// each destination column/row once instead of repeating affine and footprint
/// math for every pixel.
#[allow(clippy::too_many_arguments)]
fn paint_axis_aligned_binary_box(
    img: &crate::image::PreparedImage,
    lut: ([u8; 4], [u8; 4]),
    surface: &mut Surface,
    cmask: Option<&ClipMask>,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64, u64) {
    let mut source_columns: Vec<Option<crate::image::AxisTaps>> = Vec::with_capacity(x1 - x0);
    for x in x0..x1 {
        let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
        if !(0.0..1.0).contains(&u) {
            source_columns.push(None);
            continue;
        }
        let fx = u * img.width as f64 - 0.5;
        source_columns.push(img.box_taps_x(fx));
    }

    let mut source_rows: Vec<Option<crate::image::AxisTaps>> = Vec::with_capacity(y1 - y0);
    for y in y0..y1 {
        let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
        if !(0.0..1.0).contains(&v) {
            source_rows.push(None);
            continue;
        }
        let fy = (1.0 - v) * img.height as f64 - 0.5;
        source_rows.push(img.box_taps_y(fy));
    }

    // Prefer the row-band filter: per destination row, weight each referenced
    // source column's bits down the row's taps once, then every destination
    // box's weighted ones count is two edge reads and one prefix difference.
    // The same integers as the per-pixel `binary_box_average`, in another
    // order, so the output is byte-for-byte unchanged.
    if let Some(window) = BilevelWindow::new(img, &source_columns, &source_rows) {
        return paint_binary_box_rows(
            img,
            lut,
            surface,
            cmask,
            &source_columns,
            &source_rows,
            &window,
            x0,
            y0,
        );
    }
    paint_binary_box_serial(
        img,
        lut,
        surface,
        cmask,
        &source_columns,
        &source_rows,
        x0,
        y0,
    )
}

/// The per-pixel minified bilevel loop: each destination box's weighted ones
/// count by packed popcount (`binary_box_average_with`, which reads the rows
/// through `get`, so a truncated buffer counts its missing bytes as zero).
/// The fallback when [`BilevelWindow::new`] declines the draw, and the
/// reference the row-band filter is tested against.
#[allow(clippy::too_many_arguments)]
fn paint_binary_box_serial(
    img: &crate::image::PreparedImage,
    lut: ([u8; 4], [u8; 4]),
    surface: &mut Surface,
    cmask: Option<&ClipMask>,
    source_columns: &[Option<crate::image::AxisTaps>],
    source_rows: &[Option<crate::image::AxisTaps>],
    x0: usize,
    y0: usize,
) -> (u64, u64, u64) {
    let output_origin = surface.origin_x;
    let mut painted = 0u64;
    let mut attempts = 0u64;
    let mut taps = 0u64;
    // Fixed for the whole draw; resolve it once rather than per pixel.
    let clip_win = cmask.map(ClipMask::clip_window);
    let (output, first_output_y, output_stride) =
        surface.rows_mut_abs(y0, y0 + source_rows.len());
    // Align the row table with the clipped output window.
    let row_offset = first_output_y.saturating_sub(y0);
    let row_slice = source_rows.get(row_offset..).unwrap_or(&[]);
    for (i, (row, row_taps)) in output
        .chunks_exact_mut(output_stride)
        .zip(row_slice)
        .enumerate()
    {
        let y = first_output_y + i;
        for (local_x, col_taps) in source_columns.iter().enumerate() {
            let x = x0 + local_x;
            let cov = clip_win.as_ref().map_or(255, |cw| cw.coverage(x, y));
            if cov == 0 {
                continue;
            }
            attempts += 1;
            let (Some(tx), Some(ty)) = (col_taps, row_taps) else {
                continue;
            };
            let Some((color, source_taps)) = img.binary_box_average_with(lut, tx, ty) else {
                continue;
            };
            taps += source_taps;
            let a = mul_div_255(mul_div_255(color[3] as u16, img.alpha as u16), cov);
            if a == 0 {
                continue;
            }
            let target = (x - output_origin) * 4;
            if a == 255 {
                row[target] = color[0];
                row[target + 1] = color[1];
                row[target + 2] = color[2];
                row[target + 3] = 255;
            } else {
                let ia = 255 - a;
                row[target] =
                    (mul_div_255(color[0] as u16, a) + mul_div_255(row[target] as u16, ia)) as u8;
                row[target + 1] = (mul_div_255(color[1] as u16, a)
                    + mul_div_255(row[target + 1] as u16, ia))
                    as u8;
                row[target + 2] = (mul_div_255(color[2] as u16, a)
                    + mul_div_255(row[target + 2] as u16, ia))
                    as u8;
                row[target + 3] = (a + mul_div_255(row[target + 3] as u16, ia)) as u8;
            }
            painted += 1;
        }
    }
    (painted, attempts, taps)
}

/// The referenced source window of a minified one-bit draw, for the row-band
/// weighted popcount ([`paint_binary_box_rows`]). It reads the packed rows in
/// whole bytes from `byte_lo`, so its column index `i` is source column
/// `byte_lo * 8 + i`; the few columns outside the taps' union only cost a
/// read.
struct BilevelWindow<'a> {
    samples: &'a [u8],
    row_bytes: usize,
    byte_lo: usize,
    nbytes: usize,
    /// Source rows the draw's taps span, for the parallel decision.
    rows: usize,
}

impl<'a> BilevelWindow<'a> {
    /// `None` (keep the per-pixel loop) when no destination column or row
    /// maps into the image, when the sample buffer is shorter than the rows
    /// the taps reach (a truncated JBIG2 or CCITT decode: the window indexes
    /// rows directly, the per-pixel loop reads them through `get`), or when a
    /// row's tap weights could overflow the `u32` column sums.
    fn new(
        img: &'a crate::image::PreparedImage,
        source_columns: &[Option<crate::image::AxisTaps>],
        source_rows: &[Option<crate::image::AxisTaps>],
    ) -> Option<Self> {
        let col_lo = source_columns.iter().flatten().map(|t| t.lo).min()?;
        let col_hi = source_columns.iter().flatten().map(|t| t.hi).max()?;
        let row_lo = source_rows.iter().flatten().map(|t| t.lo).min()?;
        let row_hi = source_rows.iter().flatten().map(|t| t.hi).max()?;
        let max_row_total = source_rows.iter().flatten().map(|t| t.total).max()?;
        if max_row_total > u32::MAX as u64 {
            return None;
        }
        let row_bytes = img.packed_row_bits() / 8;
        let byte_lo = col_lo as usize >> 3;
        let nbytes = (col_hi as usize >> 3) + 1 - byte_lo;
        let end = (row_hi as usize)
            .checked_mul(row_bytes)?
            .checked_add(byte_lo + nbytes)?;
        if img.samples.len() < end {
            return None;
        }
        Some(Self {
            samples: &img.samples,
            row_bytes,
            byte_lo,
            nbytes,
            rows: (row_hi - row_lo) as usize + 1,
        })
    }

    /// `colw[i] = Σ_y wy(y)·bit(i, y)` over the source rows of `rows`, each
    /// row weighted by its tap weight; then `prefix[i] = Σ_{j<i} colw[j]`.
    /// Returns whether any referenced bit is set (when none is, both stay
    /// zero and every box of the row counts zero ones).
    fn column_sums(
        &self,
        rows: &crate::image::AxisTaps,
        colw: &mut [u32],
        prefix: &mut [u64],
    ) -> bool {
        colw.fill(0);
        let mut any = 0u8;
        for y in rows.lo..=rows.hi {
            let wy = rows.weight_at(y) as u32;
            let start = y as usize * self.row_bytes + self.byte_lo;
            let src = &self.samples[start..start + self.nbytes];
            for (&byte, sums) in src.iter().zip(colw.chunks_exact_mut(8)) {
                if byte == 0 {
                    continue;
                }
                any |= byte;
                for (k, sum) in sums.iter_mut().enumerate() {
                    *sum += wy * ((byte >> (7 - k)) & 1) as u32;
                }
            }
        }
        if any == 0 {
            return false;
        }
        let mut acc = 0u64;
        prefix[0] = 0;
        for (p, &c) in prefix[1..].iter_mut().zip(colw.iter()) {
            acc += c as u64;
            *p = acc;
        }
        true
    }
}

/// `Σ_x wx(x)·colw[x]` over the column taps `cols`, with `colw` and its
/// exclusive prefix from [`BilevelWindow::column_sums`] (index 0 at source
/// column `base`): the interior columns weigh a full `AXIS_TAP_SCALE`, so
/// they are one prefix difference. Equals `Σ_y Σ_x wy·wx·bit`, the integer
/// `BilevelIntegral::weighted_ones` and `binary_box_average` compute.
#[inline]
fn row_band_weighted_ones(
    cols: &crate::image::AxisTaps,
    base: u32,
    colw: &[u32],
    prefix: &[u64],
) -> u64 {
    let lo = (cols.lo - base) as usize;
    let hi = (cols.hi - base) as usize;
    if lo == hi {
        return cols.w_lo * colw[lo] as u64;
    }
    cols.w_lo * colw[lo] as u64
        + cols.w_hi * colw[hi] as u64
        + crate::image::AXIS_TAP_SCALE * (prefix[hi] - prefix[lo + 1])
}

/// Composite a prepared axis-aligned minified one-bit image with the row-band
/// weighted popcount ([`BilevelWindow`]). The clip coverage, the two-entry
/// mix and the source-over arithmetic are the per-pixel loop's
/// ([`paint_binary_box_serial`]); only the weighted ones count is reordered,
/// and the mix is memoized across identical `(weight, ones)` runs, which
/// dominate bilevel scans' long white/black stretches.
///
/// Destination rows are independent: each band of them weights its own
/// source rows into its own scratch and writes only its own output rows, so
/// bands run across the rayon pool (when `PDF_RENDERER_IMAGE_ROW_PAR` and the
/// pages in flight allow) without changing a byte. A source row a band shares
/// with its neighbour is only read twice. The working set is two rows of the
/// referenced source width per band, where the summed-area table this
/// replaces held four bytes per referenced source texel (34 MB for a 300-dpi
/// page, and none at all above its 32M-entry cap, which a 600-dpi Letter page
/// exceeds).
#[allow(clippy::too_many_arguments)]
fn paint_binary_box_rows(
    img: &crate::image::PreparedImage,
    (zero, one): ([u8; 4], [u8; 4]),
    surface: &mut Surface,
    cmask: Option<&ClipMask>,
    source_columns: &[Option<crate::image::AxisTaps>],
    source_rows: &[Option<crate::image::AxisTaps>],
    window: &BilevelWindow<'_>,
    x0: usize,
    y0: usize,
) -> (u64, u64, u64) {
    let alpha = img.alpha as u16;
    let output_origin = surface.origin_x;
    // Fixed for the whole draw; resolve it once rather than per pixel.
    let clip_win = cmask.map(ClipMask::clip_window);
    let base = (window.byte_lo * 8) as u32;
    let span = window.nbytes * 8;

    let paint_band = |first_y: usize,
                      band: &mut [u8],
                      band_rows: &[Option<crate::image::AxisTaps>],
                      stride: usize|
     -> (u64, u64, u64) {
        let mut colw = vec![0u32; span];
        let mut prefix = vec![0u64; span + 1];
        let (mut painted, mut attempts, mut taps) = (0u64, 0u64, 0u64);
        for (i, (row, row_taps)) in band.chunks_exact_mut(stride).zip(band_rows).enumerate() {
            let y = first_y + i;
            // Whether any referenced bit of this row's taps is set; when none
            // is, every box counts zero ones and the sums are not built.
            let any = row_taps
                .as_ref()
                .is_some_and(|ty| window.column_sums(ty, &mut colw, &mut prefix));
            // All-zero / all-one boxes short-circuit without division; the
            // mixed boxes memoize on `(weight, weighted ones)`. Clip coverage
            // and destination blending stay per pixel.
            let mut memo_key = (u64::MAX, u64::MAX);
            let mut memo_color = [0u8; 4];
            for (local_x, col_taps) in source_columns.iter().enumerate() {
                let x = x0 + local_x;
                let cov = clip_win.as_ref().map_or(255, |cw| cw.coverage(x, y));
                if cov == 0 {
                    continue;
                }
                attempts += 1;
                let (Some(tx), Some(ty)) = (col_taps, row_taps) else {
                    continue;
                };
                let weight = tx.total * ty.total;
                if weight == 0 {
                    continue;
                }
                taps += (tx.hi - tx.lo + 1) as u64 * (ty.hi - ty.lo + 1) as u64;
                let ones_w = if any {
                    row_band_weighted_ones(tx, base, &colw, &prefix)
                } else {
                    0
                };
                let color = if ones_w == 0 {
                    zero
                } else if ones_w >= weight {
                    one
                } else if memo_key == (weight, ones_w) {
                    memo_color
                } else {
                    let c = crate::image::mix_bilevel(zero, one, ones_w, weight);
                    memo_key = (weight, ones_w);
                    memo_color = c;
                    c
                };
                let a = mul_div_255(mul_div_255(color[3] as u16, alpha), cov);
                if a == 0 {
                    continue;
                }
                let target = (x - output_origin) * 4;
                if a == 255 {
                    row[target] = color[0];
                    row[target + 1] = color[1];
                    row[target + 2] = color[2];
                    row[target + 3] = 255;
                } else {
                    let ia = 255 - a;
                    row[target] = (mul_div_255(color[0] as u16, a)
                        + mul_div_255(row[target] as u16, ia))
                        as u8;
                    row[target + 1] = (mul_div_255(color[1] as u16, a)
                        + mul_div_255(row[target + 1] as u16, ia))
                        as u8;
                    row[target + 2] = (mul_div_255(color[2] as u16, a)
                        + mul_div_255(row[target + 2] as u16, ia))
                        as u8;
                    row[target + 3] = (a + mul_div_255(row[target + 3] as u16, ia)) as u8;
                }
                painted += 1;
            }
        }
        (painted, attempts, taps)
    };

    let dest_w = source_columns.len();
    let dest_h = source_rows.len();
    let (output, first_output_y, output_stride) = surface.rows_mut_abs(y0, y0 + dest_h);
    // Align the row table with the clipped output window.
    let row_offset = first_output_y.saturating_sub(y0);
    let row_slice = source_rows.get(row_offset..).unwrap_or(&[]);

    // Fan out on either side of the work: many destination pixels, or a large
    // source read (a heavily minified page has few of the former).
    let parallel = image_row_parallel_enabled()
        && (dest_w.saturating_mul(dest_h) >= IMAGE_ROW_PAR_PIXEL_THRESHOLD
            || span.saturating_mul(window.rows) >= BILEVEL_ROWS_PAR_MIN_BITS);
    if parallel {
        const BAND: usize = 16;
        output
            .par_chunks_mut(output_stride * BAND)
            .zip(row_slice.par_chunks(BAND))
            .enumerate()
            .map(|(i, (band, rows))| {
                paint_band(first_output_y + i * BAND, band, rows, output_stride)
            })
            .reduce(|| (0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2))
    } else {
        paint_band(first_output_y, output, row_slice, output_stride)
    }
}

/// A summed-area table of set bits over the referenced source sub-rectangle of a
/// prepared one-bit image. `sat[(ay+1)*stride + (ax+1)]` holds the number of set
/// bits in the local sub-rectangle rows `0..=ay`, columns `0..=ax`; the first
/// row and column are zero, so any inclusive box `[ax0..=ax1] × [ay0..=ay1]`
/// counts its ones in four reads.
///
/// Test-only: the oracle the row-band filter ([`BilevelWindow`]) that
/// replaced it is checked against.
#[cfg(test)]
struct BilevelIntegral {
    sat: Vec<u32>,
    stride: usize,
    col_lo: u32,
    row_lo: u32,
}

#[cfg(test)]
impl BilevelIntegral {
    /// Fill `sat` with the same table the serial walk in [`Self::build`]
    /// produces, in three passes: every block of rows sums from zero in
    /// parallel; the block-boundary carries are chained once (a handful of
    /// rows); every block but the first then adds its carry back, in parallel.
    /// Each cell ends as the same sum of the same `u32` terms, so the table is
    /// bit-identical to the serial one.
    #[allow(clippy::too_many_arguments)]
    fn fill_parallel(
        sat: &mut [u32],
        samples: &[u8],
        stride: usize,
        sw: usize,
        sh: usize,
        row_bits: usize,
        col_lo: u32,
        row_lo: u32,
    ) {
        // Row 0 of the table is the all-zero sentinel and is never written.
        let body = &mut sat[stride..];
        let threads = rayon::current_num_threads().max(1);
        // Two blocks per thread keeps the tail balanced without making the
        // serial carry chain (one row of `stride` adds per block) matter.
        let block_rows = sh.div_ceil(threads * 2).max(1);
        let chunk = block_rows * stride;

        body.par_chunks_mut(chunk).enumerate().for_each(|(b, blk)| {
            let first_ly = b * block_rows;
            let rows = blk.len() / stride;
            for i in 0..rows {
                let source_row = row_lo as usize + first_ly + i;
                let base_bit = source_row * row_bits + col_lo as usize;
                let (before, rest) = blk.split_at_mut(i * stride);
                let cur = &mut rest[..stride];
                let mut rowsum = 0u32;
                if i == 0 {
                    for (lx, bitpos) in (base_bit..base_bit + sw).enumerate() {
                        let byte = samples[bitpos >> 3];
                        rowsum += ((byte >> (7 - (bitpos & 7))) & 1) as u32;
                        cur[lx + 1] = rowsum;
                    }
                } else {
                    let prev = &before[(i - 1) * stride..i * stride];
                    for (lx, bitpos) in (base_bit..base_bit + sw).enumerate() {
                        let byte = samples[bitpos >> 3];
                        rowsum += ((byte >> (7 - (bitpos & 7))) & 1) as u32;
                        cur[lx + 1] = prev[lx + 1] + rowsum;
                    }
                }
            }
        });

        let blocks = body.len().div_ceil(chunk);
        if blocks < 2 {
            return;
        }
        // `carries[b]` is the running column total of every row before block
        // `b`; block 0 needs none. Only whole blocks are ever a predecessor,
        // so the last row of block `b - 1` is always at `b * block_rows - 1`.
        let mut carries = vec![0u32; blocks * stride];
        for b in 1..blocks {
            let last = (b * block_rows - 1) * stride;
            let (done, pending) = carries.split_at_mut(b * stride);
            let prev_carry = &done[(b - 1) * stride..];
            for ((dst, carry), tail) in pending[..stride]
                .iter_mut()
                .zip(prev_carry)
                .zip(&body[last..last + stride])
            {
                *dst = *carry + *tail;
            }
        }

        body.par_chunks_mut(chunk)
            .enumerate()
            .skip(1)
            .for_each(|(b, blk)| {
                let carry = &carries[b * stride..(b + 1) * stride];
                for row in blk.chunks_exact_mut(stride) {
                    for (cell, add) in row.iter_mut().zip(carry) {
                        *cell += *add;
                    }
                }
            });
    }

    /// Cap on the integral-image allocation. Above this the referenced source
    /// region is large enough that per-pixel popcount (whose total work is
    /// bounded by the box footprint, not the source area) is preferred, and the
    /// table is not built. 128 MiB of `u32` covers a ~180-megapixel scan.
    const MAX_ENTRIES: usize = 32 * 1024 * 1024;

    fn build(
        img: &crate::image::PreparedImage,
        source_columns: &[Option<crate::image::AxisTaps>],
        source_rows: &[Option<crate::image::AxisTaps>],
    ) -> Option<Self> {
        let (mut col_lo, mut col_hi, mut any_col) = (u32::MAX, 0u32, false);
        for taps in source_columns.iter().flatten() {
            col_lo = col_lo.min(taps.lo);
            col_hi = col_hi.max(taps.hi);
            any_col = true;
        }
        let (mut row_lo, mut row_hi, mut any_row) = (u32::MAX, 0u32, false);
        for taps in source_rows.iter().flatten() {
            row_lo = row_lo.min(taps.lo);
            row_hi = row_hi.max(taps.hi);
            any_row = true;
        }
        if !any_col || !any_row {
            return None;
        }
        let sw = (col_hi - col_lo + 1) as usize;
        let sh = (row_hi - row_lo + 1) as usize;
        let stride = sw + 1;
        let entries = stride.checked_mul(sh + 1)?;
        if entries > Self::MAX_ENTRIES {
            return None;
        }

        let row_bits = img.packed_row_bits();
        let samples: &[u8] = &img.samples;
        // The fill indexes the packed rows directly. A truncated buffer (a
        // short JBIG2 or CCITT decode) would panic there; the per-pixel
        // fallback reads it through `get`, as the generic sampler does.
        let last_bit = (row_hi as usize * row_bits).checked_add(col_hi as usize)?;
        if samples.len() <= last_bit >> 3 {
            return None;
        }
        let mut sat = vec![0u32; entries];
        // The table is a running sum, so the rows form one dependency chain —
        // but a *block* of rows can be summed from zero independently and then
        // shifted by the running total that reaches it, and integer addition
        // is associative, so the finished table is bit-for-bit the same one
        // the serial walk below produces. That matters here because the table
        // is the draw's dominant cost: a full-page bilevel scan writes ~100 MB
        // of `u32`, most of it first-touch page faults, and those parallelize.
        if image_row_parallel_enabled() && entries >= BILEVEL_ROWS_PAR_MIN_BITS {
            Self::fill_parallel(&mut sat, samples, stride, sw, sh, row_bits, col_lo, row_lo);
        } else {
            for ly in 0..sh {
                let source_row = row_lo as usize + ly;
                let base_bit = source_row * row_bits + col_lo as usize;
                let (prev, cur) = sat.split_at_mut((ly + 1) * stride);
                let prev = &prev[ly * stride..ly * stride + stride];
                let cur = &mut cur[..stride];
                let mut rowsum = 0u32;
                for (lx, bitpos) in (base_bit..base_bit + sw).enumerate() {
                    let byte = samples[bitpos >> 3];
                    rowsum += ((byte >> (7 - (bitpos & 7))) & 1) as u32;
                    cur[lx + 1] = prev[lx + 1] + rowsum;
                }
            }
        }
        Some(Self {
            sat,
            stride,
            col_lo,
            row_lo,
        })
    }

    /// Set bits in the inclusive source box, in four table reads.
    #[inline]
    fn ones(&self, sx0: u32, sx1: u32, sy0: u32, sy1: u32) -> u32 {
        let ax0 = (sx0 - self.col_lo) as usize;
        let ax1 = (sx1 - self.col_lo + 1) as usize;
        let ay0 = (sy0 - self.row_lo) as usize;
        let ay1 = (sy1 - self.row_lo + 1) as usize;
        let s = self.stride;
        self.sat[ay1 * s + ax1] + self.sat[ay0 * s + ax0]
            - self.sat[ay0 * s + ax1]
            - self.sat[ay1 * s + ax0]
    }

    /// Fractionally-weighted set-bit count of the tap box, in `AXIS_TAP_SCALE²`
    /// units. Interior taps weigh a full `S²`; the edge rows/columns carry
    /// their overlap weight. Expansion of `Σ wy·wx·bit` with per-axis edge
    /// deficits `u = S − w`:
    ///
    /// `S²·box − S·Σ u_c·colstrip(c) − S·Σ u_r·rowstrip(r) + Σ u_c·u_r·corner`
    ///
    /// — every term a SAT box read, so the cost stays O(1) per pixel.
    fn weighted_ones(&self, cols: &crate::image::AxisTaps, rows: &crate::image::AxisTaps) -> u64 {
        const S: i64 = crate::image::AXIS_TAP_SCALE as i64;
        let box_ones = self.ones(cols.lo, cols.hi, rows.lo, rows.hi) as i64;
        let mut acc = S * S * box_ones;

        // Unique edge indices with their weight deficits (lo == hi collapses
        // to a single entry carrying the whole deficit).
        let col_edges: [(u32, i64); 2] = if cols.lo == cols.hi {
            [(cols.lo, S - cols.w_lo as i64), (cols.lo, 0)]
        } else {
            [
                (cols.lo, S - cols.w_lo as i64),
                (cols.hi, S - cols.w_hi as i64),
            ]
        };
        let row_edges: [(u32, i64); 2] = if rows.lo == rows.hi {
            [(rows.lo, S - rows.w_lo as i64), (rows.lo, 0)]
        } else {
            [
                (rows.lo, S - rows.w_lo as i64),
                (rows.hi, S - rows.w_hi as i64),
            ]
        };

        for &(c, u) in &col_edges {
            if u > 0 {
                acc -= S * u * self.ones(c, c, rows.lo, rows.hi) as i64;
            }
        }
        for &(r, u) in &row_edges {
            if u > 0 {
                acc -= S * u * self.ones(cols.lo, cols.hi, r, r) as i64;
            }
        }
        for &(c, uc) in &col_edges {
            if uc == 0 {
                continue;
            }
            for &(r, ur) in &row_edges {
                if ur == 0 {
                    continue;
                }
                acc += uc * ur * self.ones(c, c, r, r) as i64;
            }
        }
        acc.max(0) as u64
    }
}

/// Fast path for the common scan/viewer case: an opaque, unmasked, axis-aligned
/// RGB8 image sampled nearest-neighbor. Source X indices depend only on device
/// X, so compute them once per draw; then copy bytes directly into the
/// premultiplied RGBA surface (opaque straight RGB equals premultiplied RGB).
///
/// Runs of destination columns that read consecutive source texels — the
/// whole row at 1:1 — are widened 3→4 bytes by [`widen_rgb8_to_rgba8_opaque`]
/// instead of copied texel by texel.
fn paint_axis_aligned_rgb8_nearest_opaque(
    img: &crate::image::PreparedImage,
    surface: &mut Surface,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64) {
    // Present (in-image) destination columns form one interval: `u` is
    // monotone in `x`, so the set where it lies in `[0, 1)` is contiguous.
    let mut first_x = None;
    let mut columns: Vec<u32> = Vec::with_capacity(x1 - x0);
    for x in x0..x1 {
        let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
        if !(0.0..1.0).contains(&u) {
            if first_x.is_some() {
                break;
            }
            continue;
        }
        first_x.get_or_insert(x);
        let fx = u * img.width as f64 - 0.5;
        columns.push((fx.round() as i64).clamp(0, img.width.saturating_sub(1) as i64) as u32);
    }
    let Some(first_x) = first_x else {
        return (0, 0);
    };
    let runs = consecutive_runs(&columns);
    let n = columns.len();

    let source_stride = img.width as usize * 3;
    let samples: &[u8] = &img.samples;
    let output_origin = surface.origin_x;
    let kernel = image_kernel();
    let mut painted = 0u64;
    for y in y0..y1 {
        let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
        if !(0.0..1.0).contains(&v) {
            continue;
        }
        let fy = (1.0 - v) * img.height as f64 - 0.5;
        let source_row = (fy.round() as i64).clamp(0, img.height.saturating_sub(1) as i64) as usize;
        let source_base = source_row * source_stride;
        let row = surface.row_mut(y);
        let dst = &mut row[(first_x - output_origin) * 4..][..n * 4];
        for &(start, len) in &runs {
            let src_off = source_base + columns[start] as usize * 3;
            widen_rgb8_to_rgba8_opaque(kernel, samples, src_off, &mut dst[start * 4..][..len * 4]);
        }
        painted += n as u64;
    }
    (painted, painted)
}

/// Fast path for an axis-aligned magnified or 1:1 low-bit-depth image that
/// samples through a prepared lookup table (`PreparedImage::sample_lut`): the
/// one-bit JBIG2 or CCITT page a scanner, an MRC producer or Lege itself
/// writes, drawn at or above its own resolution. The generic per-pixel loop
/// paid an inverse map, an edge test and a `shade()` per destination pixel
/// for it (~30 ns each, 100 ms for a 1474×2400 page). Device X and Y map to a
/// source column and row independently once the placement is axis-aligned,
/// so the column → texel table is built once and each destination row reads
/// its packed source row through the lookup table. Nearest sampling,
/// byte-identical to `PreparedImage::shade` (`nearest` → `pixel` →
/// `lut[read_bits(..)]`); the caller has checked the table is complete and
/// opaque and the sample data covers every row (`lut_nearest_opaque`).
fn paint_axis_aligned_lut_nearest_opaque(
    img: &crate::image::PreparedImage,
    lut: &[[u8; 4]],
    surface: &mut Surface,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64) {
    // Same column mapping as `paint_axis_aligned_rgb8_nearest_opaque`.
    let mut first_x = None;
    let mut columns: Vec<u32> = Vec::with_capacity(x1 - x0);
    for x in x0..x1 {
        let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
        if !(0.0..1.0).contains(&u) {
            if first_x.is_some() {
                break;
            }
            continue;
        }
        first_x.get_or_insert(x);
        let fx = u * img.width as f64 - 0.5;
        columns.push((fx.round() as i64).clamp(0, img.width.saturating_sub(1) as i64) as u32);
    }
    let Some(first_x) = first_x else {
        return (0, 0);
    };
    let n = columns.len();

    let bpc = img.bpc as usize;
    let mask = ((1u16 << bpc) - 1) as u8;
    // Packed rows are byte-aligned (`row_bits` is a multiple of 8), and every
    // column index is below the width, so `bit >> 3` stays inside the row.
    let row_bytes = img.packed_row_bits() / 8;
    let samples: &[u8] = &img.samples;
    let output_origin = surface.origin_x;
    let mut painted = 0u64;
    for y in y0..y1 {
        let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
        if !(0.0..1.0).contains(&v) {
            continue;
        }
        let fy = (1.0 - v) * img.height as f64 - 0.5;
        let source_row = (fy.round() as i64).clamp(0, img.height.saturating_sub(1) as i64) as usize;
        let source = &samples[source_row * row_bytes..][..row_bytes];
        let row = surface.row_mut(y);
        let dst = &mut row[(first_x - output_origin) * 4..][..n * 4];
        for (&col, out) in columns.iter().zip(dst.chunks_exact_mut(4)) {
            let bit = col as usize * bpc;
            let value = (source[bit >> 3] >> (8 - bpc - (bit & 7))) & mask;
            out.copy_from_slice(&lut[value as usize]);
        }
        painted += n as u64;
    }
    (painted, painted)
}

/// Split `columns` into maximal runs `(start, len)` of consecutive source
/// texel indices (`columns[k + 1] == columns[k] + 1`).
fn consecutive_runs(columns: &[u32]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = 0usize;
    for k in 1..=columns.len() {
        if k == columns.len() || columns[k] != columns[k - 1].wrapping_add(1) {
            runs.push((start, k - start));
            start = k;
        }
    }
    runs
}

/// Which vector kernel the opaque RGB8 image painters run. Probed once per
/// draw, never per pixel; the scalar twins are the reference the vector
/// kernels are tested against bit for bit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImageKernel {
    Scalar,
    #[cfg(target_arch = "x86_64")]
    Avx2,
}

#[inline]
fn image_kernel() -> ImageKernel {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return ImageKernel::Avx2;
        }
    }
    ImageKernel::Scalar
}

/// Write `dst.len() / 4` opaque RGBA pixels from the packed RGB8 texels at
/// `src[src_off..]`: `[r, g, b]` → `[r, g, b, 255]`. The source is the whole
/// sample buffer (not just the run) so the vector kernel can read a few bytes
/// past the run's end wherever the buffer continues; it never reads past the
/// buffer itself.
#[inline]
#[allow(
    unsafe_code,
    reason = "dispatches the AVX2 widen kernel behind a runtime probe; see SAFETY comment"
)]
fn widen_rgb8_to_rgba8_opaque(kernel: ImageKernel, src: &[u8], src_off: usize, dst: &mut [u8]) {
    let n = dst.len() / 4;
    // Bounds-check the run once, here, so both kernels share one invariant.
    let run = &src[src_off..src_off + n * 3];
    match kernel {
        #[cfg(target_arch = "x86_64")]
        ImageKernel::Avx2 => {
            // SAFETY: AVX2 is present (`image_kernel` probed it); `run` is a
            // valid `3n` byte slice at `src_off` within `src`, and `dst` holds
            // at least `4n` bytes.
            unsafe { widen_rgb8_to_rgba8_avx2(src, src_off, n, dst) }
        }
        ImageKernel::Scalar => widen_rgb8_to_rgba8_scalar(run, dst),
    }
}

/// Scalar reference for [`widen_rgb8_to_rgba8_opaque`].
#[inline]
fn widen_rgb8_to_rgba8_scalar(run: &[u8], dst: &mut [u8]) {
    for (rgb, out) in run.chunks_exact(3).zip(dst.chunks_exact_mut(4)) {
        out[0] = rgb[0];
        out[1] = rgb[1];
        out[2] = rgb[2];
        out[3] = 255;
    }
}

/// AVX2 widen: eight texels per iteration (two 16-byte loads, one in-lane
/// byte shuffle, one alpha OR, one 32-byte store), scalar for the remainder
/// and wherever a 16-byte load would run past the end of `src`.
///
/// # Safety
/// The CPU must support AVX2. `src[src_off..src_off + 3n]` and `dst[..4n]`
/// must be in bounds (the caller slices both).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(unsafe_code)]
unsafe fn widen_rgb8_to_rgba8_avx2(src: &[u8], src_off: usize, n: usize, dst: &mut [u8]) {
    use core::arch::x86_64::*;
    debug_assert!(dst.len() >= n * 4 && src.len() >= src_off + n * 3);
    // Per 128-bit lane: texel k's bytes 3k..3k+2 land at 4k..4k+2; byte 4k+3
    // is zeroed (shuffle index -1) and then OR'd to 255.
    let shuffle = _mm256_setr_epi8(
        0, 1, 2, -1, 3, 4, 5, -1, 6, 7, 8, -1, 9, 10, 11, -1, //
        0, 1, 2, -1, 3, 4, 5, -1, 6, 7, 8, -1, 9, 10, 11, -1,
    );
    let alpha = _mm256_set1_epi32(0xFF00_0000u32 as i32);
    let mut i = 0usize;
    // The second lane's load starts 12 bytes in and reads 16, so the group
    // needs 28 readable source bytes.
    while i + 8 <= n && src_off + i * 3 + 28 <= src.len() {
        // SAFETY: both 16-byte loads lie within `src` by the loop condition;
        // the 32-byte store lies within `dst[..4n]` since `i + 8 <= n`.
        unsafe {
            let p = src.as_ptr().add(src_off + i * 3);
            let lo = _mm_loadu_si128(p.cast());
            let hi = _mm_loadu_si128(p.add(12).cast());
            let v = _mm256_set_m128i(hi, lo);
            let out = _mm256_or_si256(_mm256_shuffle_epi8(v, shuffle), alpha);
            _mm256_storeu_si256(dst.as_mut_ptr().add(i * 4).cast(), out);
        }
        i += 8;
    }
    if i < n {
        widen_rgb8_to_rgba8_scalar(
            &src[src_off + i * 3..src_off + n * 3],
            &mut dst[i * 4..n * 4],
        );
    }
}

/// The present destination columns of an axis-aligned bilinear draw, as
/// structure-of-arrays tables the row kernels stream through. Present columns
/// (those whose centre maps inside the image) form one interval starting at
/// device column `first_x`; absent columns paint nothing.
struct BilinearColumns {
    first_x: usize,
    /// Byte offset of the floor tap within a source row (`i0 * 3`).
    off0: Vec<i32>,
    /// Byte offset of the ceil tap within a source row (`i1 * 3`, clamped).
    off1: Vec<i32>,
    /// Fractional weight toward the ceil tap (matches `PreparedImage::bilinear`).
    t: Vec<f32>,
    one_minus_t: Vec<f32>,
}

impl BilinearColumns {
    fn len(&self) -> usize {
        self.off0.len()
    }
}

/// The present destination rows of an axis-aligned bilinear draw: the source
/// row pair and vertical weights of each, starting at device row `first_y`.
struct BilinearRows {
    first_y: usize,
    i0: Vec<u32>,
    i1: Vec<u32>,
    t: Vec<f32>,
    one_minus_t: Vec<f32>,
}

impl BilinearRows {
    fn len(&self) -> usize {
        self.i0.len()
    }
}

/// Prepare the column table over device columns `x0..x1`, or `None` when no
/// column centre maps inside the image.
fn bilinear_columns(
    img: &crate::image::PreparedImage,
    x0: usize,
    x1: usize,
) -> Option<BilinearColumns> {
    let w = img.width;
    let last = (w as i64 - 1).max(0);
    let mut cols = BilinearColumns {
        first_x: 0,
        off0: Vec::with_capacity(x1 - x0),
        off1: Vec::with_capacity(x1 - x0),
        t: Vec::with_capacity(x1 - x0),
        one_minus_t: Vec::with_capacity(x1 - x0),
    };
    let mut first = None;
    for x in x0..x1 {
        let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
        if !(0.0..1.0).contains(&u) {
            if first.is_some() {
                break;
            }
            continue;
        }
        first.get_or_insert(x);
        let fx = u * w as f64 - 0.5;
        let x_floor = fx.floor();
        let tx = (fx - x_floor) as f32;
        let ix0 = (x_floor as i64).clamp(0, last);
        let ix1 = (x_floor as i64 + 1).clamp(0, last);
        cols.off0.push((ix0 * 3) as i32);
        cols.off1.push((ix1 * 3) as i32);
        cols.t.push(tx);
        cols.one_minus_t.push(1.0 - tx);
    }
    cols.first_x = first?;
    Some(cols)
}

/// Prepare the row table over device rows `y0..y1` (see [`bilinear_columns`]).
fn bilinear_rows(img: &crate::image::PreparedImage, y0: usize, y1: usize) -> Option<BilinearRows> {
    let h = img.height;
    let last = (h as i64 - 1).max(0);
    let mut rows = BilinearRows {
        first_y: 0,
        i0: Vec::with_capacity(y1 - y0),
        i1: Vec::with_capacity(y1 - y0),
        t: Vec::with_capacity(y1 - y0),
        one_minus_t: Vec::with_capacity(y1 - y0),
    };
    let mut first = None;
    for y in y0..y1 {
        let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
        if !(0.0..1.0).contains(&v) {
            if first.is_some() {
                break;
            }
            continue;
        }
        first.get_or_insert(y);
        let fy = (1.0 - v) * h as f64 - 0.5;
        let y_floor = fy.floor();
        let ty = (fy - y_floor) as f32;
        rows.i0.push((y_floor as i64).clamp(0, last) as u32);
        rows.i1.push((y_floor as i64 + 1).clamp(0, last) as u32);
        rows.t.push(ty);
        rows.one_minus_t.push(1.0 - ty);
    }
    rows.first_y = first?;
    Some(rows)
}

/// Fast path for opaque, unmasked, axis-aligned RGB8 magnification / 1:1 with
/// bilinear sampling — the continuous-tone JPEG/JPX case.
///
/// Mirrors [`paint_axis_aligned_rgb8_nearest_opaque`]: source U depends only on
/// device X and V only on Y, so axis tables are prepared once. The filter is
/// separable and is run that way: each *source* row touched is lerped
/// horizontally once into a planar f32 row (`hlerp`), and each destination row
/// lerps its two cached source rows vertically (`vlerp`) and writes opaque
/// premultiplied RGBA. Under magnification a source row serves several
/// destination rows, and even at 1:1 it serves two, so the horizontal work is
/// shared instead of repeated per destination pixel. Every op is the same f32
/// op in the same order as `PreparedImage::bilinear` (horizontal, then
/// vertical, then truncate), so the result is byte-identical to the generic
/// per-pixel sampler.
///
/// When every fractional weight is exactly zero — the image placed at unit
/// scale on an integer translation — the lerps reduce to the floor texel and
/// the draw is a straight 3→4 byte widen of each source row.
///
/// Rows are independent and painted in bands across the rayon pool when the
/// destination is large enough that the parallel overhead is recovered. On
/// x86_64 with AVX2 the row kernels gather and lerp eight columns at a time;
/// the scalar twins are the tested reference.
fn paint_axis_aligned_rgb8_bilinear_opaque(
    img: &crate::image::PreparedImage,
    surface: &mut Surface,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64) {
    let (Some(columns), Some(rows)) = (bilinear_columns(img, x0, x1), bilinear_rows(img, y0, y1))
    else {
        return (0, 0);
    };
    let painted = paint_bilinear_rows(image_kernel(), img, surface, &columns, &rows);
    // Four source taps per painted destination pixel (edge clamps may alias).
    (painted, painted.saturating_mul(4))
}

/// Paint the prepared bilinear tables with `kernel`. Split from
/// [`paint_axis_aligned_rgb8_bilinear_opaque`] so the tests can run every
/// kernel over the same tables and compare the surfaces.
fn paint_bilinear_rows(
    kernel: ImageKernel,
    img: &crate::image::PreparedImage,
    surface: &mut Surface,
    columns: &BilinearColumns,
    rows: &BilinearRows,
) -> u64 {
    let source_stride = img.width as usize * 3;
    let samples: &[u8] = &img.samples;
    let n = columns.len();
    let dst_x = (columns.first_x - surface.origin_x) * 4;

    // The integer-aligned unit-scale draw: `t == 0` everywhere makes every
    // lerp return its floor tap exactly, so the row is a widened copy.
    let unit_blit = columns.t.iter().all(|&t| t == 0.0) && rows.t.iter().all(|&t| t == 0.0);

    let (buf, first_abs_y, stride) = surface.rows_mut_abs(rows.first_y, rows.first_y + rows.len());
    // Align the row table with the clipped buffer window.
    let row_offset = first_abs_y.saturating_sub(rows.first_y);
    let dest_rows = rows
        .len()
        .saturating_sub(row_offset)
        .min(buf.len() / stride.max(1));
    if dest_rows == 0 || n == 0 {
        return 0;
    }
    let buf = &mut buf[..dest_rows * stride];
    let parallel = image_row_parallel_enabled()
        && n.saturating_mul(dest_rows) >= IMAGE_ROW_PAR_PIXEL_THRESHOLD;

    if unit_blit {
        let texels: Vec<u32> = columns.off0.iter().map(|&o| (o / 3) as u32).collect();
        let runs = consecutive_runs(&texels);
        let paint_row = |i: usize, row: &mut [u8]| {
            let base = rows.i0[row_offset + i] as usize * source_stride;
            let dst = &mut row[dst_x..dst_x + n * 4];
            for &(start, len) in &runs {
                let src_off = base + columns.off0[start] as usize;
                widen_rgb8_to_rgba8_opaque(
                    kernel,
                    samples,
                    src_off,
                    &mut dst[start * 4..][..len * 4],
                );
            }
        };
        if parallel {
            buf.par_chunks_mut(stride)
                .enumerate()
                .for_each(|(i, row)| paint_row(i, row));
        } else {
            buf.chunks_exact_mut(stride)
                .enumerate()
                .for_each(|(i, row)| paint_row(i, row));
        }
        return (n * dest_rows) as u64;
    }

    // Each band of destination rows streams its own two-slot cache of
    // horizontally lerped source rows. Bands are the unit of fan-out; serially
    // the whole draw is one band so no source row is lerped twice.
    const BAND_ROWS: usize = 32;
    let band_rows = if parallel { BAND_ROWS } else { dest_rows };
    let paint_band = |band: usize, band_buf: &mut [u8]| {
        let mut cache = HLerpCache::new(n);
        for (i, row) in band_buf.chunks_exact_mut(stride).enumerate() {
            let r = row_offset + band * band_rows + i;
            let (iy0, iy1) = (rows.i0[r], rows.i1[r]);
            let s0 = cache.ensure(iy0, None, |plane| {
                hlerp_row(
                    kernel,
                    samples,
                    iy0 as usize * source_stride,
                    columns,
                    plane,
                )
            });
            let s1 = cache.ensure(iy1, Some(s0), |plane| {
                hlerp_row(
                    kernel,
                    samples,
                    iy1 as usize * source_stride,
                    columns,
                    plane,
                )
            });
            let (p0, p1) = (cache.plane(s0), cache.plane(s1));
            vlerp_row(
                kernel,
                p0,
                p1,
                n,
                rows.t[r],
                rows.one_minus_t[r],
                &mut row[dst_x..dst_x + n * 4],
            );
        }
    };
    if parallel {
        buf.par_chunks_mut(stride * band_rows)
            .enumerate()
            .for_each(|(band, band_buf)| paint_band(band, band_buf));
    } else {
        buf.chunks_mut(stride * band_rows)
            .enumerate()
            .for_each(|(band, band_buf)| paint_band(band, band_buf));
    }
    (n * dest_rows) as u64
}

/// Two horizontally lerped source rows, planar f32 (`[r; n][g; n][b; n]`),
/// keyed by source row. Destination rows walk the source monotonically with
/// step ≤ 1 under magnification, so two slots make every source row's
/// horizontal pass happen once per band.
struct HLerpCache {
    n: usize,
    keys: [Option<u32>; 2],
    planes: [Vec<f32>; 2],
    next_evict: usize,
}

impl HLerpCache {
    fn new(n: usize) -> Self {
        Self {
            n,
            keys: [None, None],
            planes: [vec![0.0; n * 3], vec![0.0; n * 3]],
            next_evict: 0,
        }
    }

    /// The slot holding source row `row`, filling one with `fill` if absent.
    /// `keep` is a slot the caller still needs and must not be evicted.
    fn ensure(&mut self, row: u32, keep: Option<usize>, fill: impl FnOnce(&mut [f32])) -> usize {
        if let Some(slot) = (0..2).find(|&s| self.keys[s] == Some(row)) {
            return slot;
        }
        let mut slot = self.next_evict;
        if keep == Some(slot) {
            slot = 1 - slot;
        }
        fill(&mut self.planes[slot][..self.n * 3]);
        self.keys[slot] = Some(row);
        self.next_evict = 1 - slot;
        slot
    }

    fn plane(&self, slot: usize) -> &[f32] {
        &self.planes[slot][..self.n * 3]
    }
}

/// Horizontal pass of one source row (at byte offset `base`) over the column
/// table, into `plane` (`[r; n][g; n][b; n]`): each lane is
/// `s0 * (1 - t) + s1 * t` in f32, the order `PreparedImage::bilinear` uses.
#[inline]
#[allow(
    unsafe_code,
    reason = "dispatches the AVX2 gather kernel behind a runtime probe; see SAFETY comment"
)]
fn hlerp_row(
    kernel: ImageKernel,
    samples: &[u8],
    base: usize,
    columns: &BilinearColumns,
    plane: &mut [f32],
) {
    match kernel {
        #[cfg(target_arch = "x86_64")]
        ImageKernel::Avx2 => {
            // SAFETY: AVX2 is present (`image_kernel` probed it). The kernel
            // bounds every gather against `samples.len()` itself and takes the
            // scalar path for columns a 4-byte gather would overrun.
            unsafe { hlerp_row_avx2(samples, base, columns, plane) }
        }
        ImageKernel::Scalar => hlerp_row_scalar(samples, base, columns, plane, 0),
    }
}

/// Scalar reference for [`hlerp_row`], from column `start` on.
#[inline]
fn hlerp_row_scalar(
    samples: &[u8],
    base: usize,
    columns: &BilinearColumns,
    plane: &mut [f32],
    start: usize,
) {
    let n = columns.len();
    let (r, rest) = plane.split_at_mut(n);
    let (g, b) = rest.split_at_mut(n);
    for k in start..n {
        let s0 = base + columns.off0[k] as usize;
        let s1 = base + columns.off1[k] as usize;
        let omt = columns.one_minus_t[k];
        let t = columns.t[k];
        r[k] = samples[s0] as f32 * omt + samples[s1] as f32 * t;
        g[k] = samples[s0 + 1] as f32 * omt + samples[s1 + 1] as f32 * t;
        b[k] = samples[s0 + 2] as f32 * omt + samples[s1 + 2] as f32 * t;
    }
}

/// AVX2 horizontal pass: eight columns per iteration, each tap fetched as one
/// 32-bit gather (`[r, g, b, next]`) and split into three f32 lanes. Same
/// mul/mul/add order as the scalar twin, no FMA, so the planes are identical.
///
/// # Safety
/// The CPU must support AVX2. `plane.len() >= 3 * columns.len()`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(unsafe_code)]
unsafe fn hlerp_row_avx2(
    samples: &[u8],
    base: usize,
    columns: &BilinearColumns,
    plane: &mut [f32],
) {
    use core::arch::x86_64::*;
    let n = columns.len();
    debug_assert!(plane.len() >= n * 3);
    // A gather reads 4 bytes at `base + off`; the row's ceil-tap offsets are
    // non-decreasing, so the columns it may serve are a prefix.
    let avail = samples.len().saturating_sub(base);
    let gather_cols = if avail >= 4 {
        columns.off1.partition_point(|&o| (o as usize) + 4 <= avail)
    } else {
        0
    };
    let byte_mask = _mm256_set1_epi32(0xFF);
    let mut k = 0usize;
    // SAFETY: every index gathered in this loop satisfies
    // `base + off + 4 <= samples.len()` by `gather_cols`; the plane stores are
    // within `plane[..3n]` since `k + 8 <= n`.
    unsafe {
        let row_ptr = samples.as_ptr().add(base).cast::<i32>();
        let (r_out, rest) = plane.split_at_mut(n);
        let (g_out, b_out) = rest.split_at_mut(n);
        // Eight consecutive texels (24 bytes) split into per-channel i32 lanes:
        // two 16-byte loads (the second 12 bytes in, so each 128-bit lane
        // holds four texels) and one in-lane byte shuffle per channel.
        let plane_mask = |c: i8| {
            _mm256_setr_epi8(
                c,
                -1,
                -1,
                -1,
                c + 3,
                -1,
                -1,
                -1,
                c + 6,
                -1,
                -1,
                -1,
                c + 9,
                -1,
                -1,
                -1, //
                c,
                -1,
                -1,
                -1,
                c + 3,
                -1,
                -1,
                -1,
                c + 6,
                -1,
                -1,
                -1,
                c + 9,
                -1,
                -1,
                -1,
            )
        };
        let (mask_r, mask_g, mask_b) = (plane_mask(0), plane_mask(1), plane_mask(2));
        let texel_ramp = _mm256_setr_epi32(0, 3, 6, 9, 12, 15, 18, 21);
        let load_run = |off: i32| {
            let p = samples.as_ptr().add(base + off as usize);
            let v = _mm256_set_m128i(_mm_loadu_si128(p.add(12).cast()), _mm_loadu_si128(p.cast()));
            (
                _mm256_shuffle_epi8(v, mask_r),
                _mm256_shuffle_epi8(v, mask_g),
                _mm256_shuffle_epi8(v, mask_b),
            )
        };
        while k + 8 <= gather_cols {
            let omt = _mm256_loadu_ps(columns.one_minus_t.as_ptr().add(k));
            let t = _mm256_loadu_ps(columns.t.as_ptr().add(k));
            let lerp = |a: __m256i, b: __m256i| {
                let fa = _mm256_cvtepi32_ps(a);
                let fb = _mm256_cvtepi32_ps(b);
                _mm256_add_ps(_mm256_mul_ps(fa, omt), _mm256_mul_ps(fb, t))
            };
            let (o0, o1) = (columns.off0[k], columns.off1[k]);
            let idx0 = _mm256_loadu_si256(columns.off0.as_ptr().add(k).cast());
            let idx1 = _mm256_loadu_si256(columns.off1.as_ptr().add(k).cast());
            // When the eight columns read eight consecutive texels — the 1:1
            // case — both taps are plain loads instead of gathers. The test is
            // per lane: at 1:1 the floor index can also step by zero or two
            // texels where floating-point noise straddles an integer, so a
            // matching span alone would not do. The second lane's load reads
            // 28 bytes from the run's start.
            let consecutive = |idx: __m256i, o: i32| {
                let expected = _mm256_add_epi32(_mm256_set1_epi32(o), texel_ramp);
                _mm256_movemask_epi8(_mm256_cmpeq_epi32(idx, expected)) == -1
            };
            let run = consecutive(idx0, o0)
                && consecutive(idx1, o1)
                && base + o1 as usize + 28 <= samples.len();
            let ((r0, g0, b0), (r1, g1, b1)) = if run {
                (load_run(o0), load_run(o1))
            } else {
                let v0 = _mm256_i32gather_epi32::<1>(row_ptr, idx0);
                let v1 = _mm256_i32gather_epi32::<1>(row_ptr, idx1);
                let split = |v: __m256i| {
                    (
                        _mm256_and_si256(v, byte_mask),
                        _mm256_and_si256(_mm256_srli_epi32::<8>(v), byte_mask),
                        _mm256_and_si256(_mm256_srli_epi32::<16>(v), byte_mask),
                    )
                };
                (split(v0), split(v1))
            };
            _mm256_storeu_ps(r_out.as_mut_ptr().add(k), lerp(r0, r1));
            _mm256_storeu_ps(g_out.as_mut_ptr().add(k), lerp(g0, g1));
            _mm256_storeu_ps(b_out.as_mut_ptr().add(k), lerp(b0, b1));
            k += 8;
        }
    }
    if k < n {
        hlerp_row_scalar(samples, base, columns, plane, k);
    }
}

/// Vertical pass of one destination row: `p0 * (1 - t) + p1 * t` per channel
/// in f32, truncated toward zero, written as opaque RGBA into `dst` (`4n`
/// bytes). The order `PreparedImage::bilinear` uses after its horizontal lerps.
#[inline]
#[allow(
    unsafe_code,
    reason = "dispatches the AVX2 pack kernel behind a runtime probe; see SAFETY comment"
)]
fn vlerp_row(
    kernel: ImageKernel,
    p0: &[f32],
    p1: &[f32],
    n: usize,
    t: f32,
    one_minus_t: f32,
    dst: &mut [u8],
) {
    assert!(p0.len() >= n * 3 && p1.len() >= n * 3 && dst.len() >= n * 4);
    match kernel {
        #[cfg(target_arch = "x86_64")]
        ImageKernel::Avx2 => {
            // SAFETY: AVX2 is present (`image_kernel` probed it); the slice
            // lengths asserted above are the kernel's only precondition.
            unsafe { vlerp_row_avx2(p0, p1, n, t, one_minus_t, dst) }
        }
        ImageKernel::Scalar => vlerp_row_scalar(p0, p1, n, t, one_minus_t, dst, 0),
    }
}

/// Scalar reference for [`vlerp_row`], from column `start` on.
#[inline]
fn vlerp_row_scalar(
    p0: &[f32],
    p1: &[f32],
    n: usize,
    t: f32,
    omt: f32,
    dst: &mut [u8],
    start: usize,
) {
    for k in start..n {
        let out = &mut dst[k * 4..k * 4 + 4];
        out[0] = (p0[k] * omt + p1[k] * t).clamp(0.0, 255.0) as u8;
        out[1] = (p0[n + k] * omt + p1[n + k] * t).clamp(0.0, 255.0) as u8;
        out[2] = (p0[2 * n + k] * omt + p1[2 * n + k] * t).clamp(0.0, 255.0) as u8;
        out[3] = 255;
    }
}

/// AVX2 vertical pass: eight columns per iteration — three planar lerps, a
/// truncating `cvtt` (matching `as u8` on `[0, 255]`), a clamp, and one packed
/// 32-byte RGBA store.
///
/// # Safety
/// The CPU must support AVX2. `p0.len() >= 3n`, `p1.len() >= 3n`,
/// `dst.len() >= 4n`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(unsafe_code)]
unsafe fn vlerp_row_avx2(p0: &[f32], p1: &[f32], n: usize, t: f32, omt: f32, dst: &mut [u8]) {
    use core::arch::x86_64::*;
    debug_assert!(p0.len() >= n * 3 && p1.len() >= n * 3 && dst.len() >= n * 4);
    let tv = _mm256_set1_ps(t);
    let omtv = _mm256_set1_ps(omt);
    let zero = _mm256_setzero_si256();
    let max = _mm256_set1_epi32(255);
    let alpha = _mm256_set1_epi32(0xFF00_0000u32 as i32);
    let mut k = 0usize;
    // SAFETY: every load reads `[k, k + 8)` of a plane channel with
    // `k + 8 <= n`, within the asserted lengths; the store writes
    // `dst[4k..4k + 32]`, within `4n`.
    unsafe {
        while k + 8 <= n {
            let channel = |c: usize| {
                let a = _mm256_loadu_ps(p0.as_ptr().add(c * n + k));
                let b = _mm256_loadu_ps(p1.as_ptr().add(c * n + k));
                let v = _mm256_add_ps(_mm256_mul_ps(a, omtv), _mm256_mul_ps(b, tv));
                let i = _mm256_cvttps_epi32(v);
                _mm256_min_epi32(_mm256_max_epi32(i, zero), max)
            };
            let r = channel(0);
            let g = _mm256_slli_epi32::<8>(channel(1));
            let b = _mm256_slli_epi32::<16>(channel(2));
            let px = _mm256_or_si256(_mm256_or_si256(r, g), _mm256_or_si256(b, alpha));
            _mm256_storeu_si256(dst.as_mut_ptr().add(k * 4).cast(), px);
            k += 8;
        }
    }
    if k < n {
        vlerp_row_scalar(p0, p1, n, t, omt, dst, k);
    }
}

/// Fast path for an opaque, unmasked, axis-aligned RGB8 image being
/// area-minified. The minification twin of `paint_axis_aligned_rgb8_nearest_opaque`:
/// each destination pixel averages the inclusive source-texel box that
/// `image.rs::area_average` computes, then — the image being fully opaque — the
/// averaged RGB is written straight to the surface (opaque source-over is a
/// copy), skipping the generic path's per-pixel float shade dispatch, `/Decode`,
/// mask tests, `average_rgba`, and blend.
///
/// Source X boxes are prepared once per destination column and Y boxes once per
/// row. The dispatch gate requires exactly-zero off-diagonal inverse terms, so
/// `u = inv.a·dx + inv.e` and `v = inv.d·dy + inv.f` equal the generic path's
/// `inv.apply` bit-for-bit (adding `inv.c·dy = 0.0` / `inv.b·dx = 0.0` is the
/// identity on a finite float). The box bounds, the uniform-weight sum, and the
/// integer-truncating divide all mirror `area_average` exactly, so the output is
/// byte-for-byte identical to the generic path. Returns `(painted, taps)` where
/// `taps` is the summed box area, matching `image.source_sample_taps`.
fn paint_axis_aligned_rgb8_area_min_opaque(
    img: &crate::image::PreparedImage,
    surface: &mut Surface,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64) {
    // RGB8 source samples are already the exact output component representation.
    area_min_box_average_opaque(img, surface, &img.samples, x0, y0, x1, y1)
}

/// Area-minify an opaque axis-aligned CMYK image. The source is converted to
/// packed RGB8 **once** (each pixel through the identical `cmyk_to_rgb`/`to_u8`
/// as the generic `pixel()`), then box-averaged like the RGB8 path. This is
/// byte-identical to the generic convert-each-tap-then-average, but the
/// conversion is hoisted out of the per-destination-pixel loop. `None` (→ the
/// caller keeps the generic path) if the source is not convertible or too large.
fn paint_axis_aligned_cmyk_area_min_opaque(
    img: &crate::image::PreparedImage,
    surface: &mut Surface,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> Option<(u64, u64)> {
    let rgb = img.cmyk_source_as_rgb8()?;
    Some(area_min_box_average_opaque(
        img, surface, &rgb, x0, y0, x1, y1,
    ))
}

/// The bilevel cut-out a minified RGB8 draw carries. MRC producers write one of
/// two encodings: a grayscale `/SMask` coverage layer whose alpha is
/// box-filtered with the base image, or a hard `/Mask` stencil that is
/// point-sampled all-or-nothing. Both are JBIG2 in practice.
#[derive(Clone, Copy)]
enum AreaMinMask<'a> {
    /// A one-bit `/SMask`, box-filtered by packed popcount.
    Soft(&'a pdf_page_ir::ImageSMask),
    /// An eight-bit `/SMask` (a cover's transparency layer), box-filtered in
    /// f64 exactly as the generic sampler does.
    Soft8(&'a pdf_page_ir::ImageSMask),
    Stencil(&'a pdf_page_ir::ImageSMask),
}

/// Classify `img`'s mask for the axis-aligned area-minification fast path, or
/// `None` to keep the generic per-pixel path.
fn area_min_mask(img: &crate::image::PreparedImage) -> Option<AreaMinMask<'_>> {
    if let Some(sm) = img.smask.as_deref() {
        // `sample_smask` only box-filters at all when the mask minifies on an
        // axis; point-sampled masks are not what this path reproduces. Its
        // packed-bit branch is reproduced at 1 bpc, its multi-bit branch at
        // 8 bpc over a complete byte-per-texel buffer.
        let footprint = img.smask_footprint(sm);
        let minified = sm.width > 0 && sm.height > 0 && (footprint[0] > 1.0 || footprint[1] > 1.0);
        if !minified {
            return None;
        }
        return match sm.bits_per_component {
            1 => Some(AreaMinMask::Soft(sm)),
            8 if sm.samples.len() >= sm.width as usize * sm.height as usize => {
                Some(AreaMinMask::Soft8(sm))
            }
            _ => None,
        };
    }
    match img.mask.as_ref()? {
        // A colour-key mask tests the base image's own samples, not an
        // independent bitmap, so it does not separate per axis.
        pdf_page_ir::ImageMask::ColorKey(_) => None,
        pdf_page_ir::ImageMask::Stencil(sm) => {
            (sm.width > 0 && sm.height > 0).then_some(AreaMinMask::Stencil(sm))
        }
    }
}

/// The prepared mask lookup for one destination column or row.
#[derive(Clone, Copy)]
enum MaskAxis {
    /// Box-filter taps into a one-bit `/SMask`.
    Taps(crate::image::AxisTaps),
    /// The point-sampled texel index of a hard `/Mask` stencil.
    Texel(u32),
}

/// Area-minify an axis-aligned opaque RGB8 image cut out by a mask — the MRC
/// scanned-page shape (a JPX foreground over a JPX background, cut out by a
/// JBIG2 layer sharing the foreground's placement) and the cover-image shape
/// (a Flate RGB/palette image under an eight-bit transparency layer).
///
/// The generic per-pixel loop recomputes, for every destination pixel, the
/// inverse map, the base image's two box-filter tap ranges, and the mask's own
/// per-axis lookup. With `inv.b == inv.c == 0` every one of those depends only
/// on the destination column (X) or row (Y), so they are prepared once each
/// here. What remains per pixel — the weighted box average, the mask's
/// coverage or stencil test, the `/Decode` remap, and the source-over
/// composite — is the generic path's arithmetic term for term, so the painted
/// pixels are unchanged.
// Called once per draw, and large enough (two prepared axis passes, the box
// average, both mask encodings, and the generic border fallback) that inlining
// it into `paint_image` measurably degrades the *sibling* fast paths' codegen —
// the unmasked JPX scan lost 37 % of its raster time to the bloat before this.
#[inline(never)]
fn paint_axis_aligned_rgb8_area_min_masked(
    img: &crate::image::PreparedImage,
    mask: AreaMinMask<'_>,
    surface: &mut Surface,
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64) {
    let stride = img.width as usize * 3;
    let rgb: &[u8] = &img.samples;
    // `edge_coverage` returns `None` — provably interior, full weight — exactly
    // when the mapped pixel square stays inside the unit square on both axes.
    // With `inv.b == inv.c == 0` that test separates, so the interior is a
    // rectangle of whole destination columns and rows: the prepared body runs
    // there, and the anti-aliased border band keeps the generic per-pixel
    // treatment. The draw therefore stays pixel-identical, edges included.
    let half_u = 0.5 * img.inv.a.abs();
    let half_v = 0.5 * img.inv.d.abs();
    let mask_footprint = match mask {
        AreaMinMask::Soft(sm) | AreaMinMask::Soft8(sm) => img.smask_footprint(sm),
        AreaMinMask::Stencil(_) => [0.0, 0.0],
    };

    // Per destination column: whether it is interior in u, the base image's X
    // tap box, and the mask's X lookup.
    let mut columns: Vec<(bool, Option<(crate::image::AxisTaps, MaskAxis)>)> =
        Vec::with_capacity(x1 - x0);
    for x in x0..x1 {
        let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
        let interior = u >= half_u && u <= 1.0 - half_u;
        if !(0.0..1.0).contains(&u) {
            columns.push((interior, None));
            continue;
        }
        let base = img.box_taps_x(u * img.width as f64 - 0.5);
        let axis = match mask {
            AreaMinMask::Soft(sm) | AreaMinMask::Soft8(sm) => {
                crate::image::PreparedImage::smask_box_taps(
                    u * sm.width as f64 - 0.5,
                    mask_footprint[0],
                    sm.width,
                )
                .map(MaskAxis::Taps)
            }
            AreaMinMask::Stencil(sm) => Some(MaskAxis::Texel(crate::image::stencil_col(sm, u))),
        };
        columns.push((interior, base.zip(axis)));
    }

    // Per destination row, the same.
    let mut rows: Vec<(bool, Option<(crate::image::AxisTaps, MaskAxis)>)> =
        Vec::with_capacity(y1 - y0);
    for y in y0..y1 {
        let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
        let interior = v >= half_v && v <= 1.0 - half_v;
        if !(0.0..1.0).contains(&v) {
            rows.push((interior, None));
            continue;
        }
        let base = img.box_taps_y((1.0 - v) * img.height as f64 - 0.5);
        let axis = match mask {
            AreaMinMask::Soft(sm) | AreaMinMask::Soft8(sm) => {
                crate::image::PreparedImage::smask_box_taps(
                    (1.0 - v) * sm.height as f64 - 0.5,
                    mask_footprint[1],
                    sm.height,
                )
                .map(MaskAxis::Taps)
            }
            AreaMinMask::Stencil(sm) => Some(MaskAxis::Texel(crate::image::stencil_row(sm, v))),
        };
        rows.push((interior, base.zip(axis)));
    }

    let output_origin = surface.origin_x;
    let dest_w = x1 - x0;
    let dest_h = y1 - y0;
    let columns = columns.as_slice();
    // The alpha a soft mask yields over a box whose texels are all clear or
    // all set. Both coverages are exact in either filter (see
    // `MaskRowSummary`), so these are the bytes the per-tap filter produces.
    let (alpha_clear, alpha_set) = match mask {
        AreaMinMask::Soft(sm) | AreaMinMask::Soft8(sm) => (
            (255.0 * crate::image::apply_smask_decode(sm, 0.0)) as u8,
            (255.0 * crate::image::apply_smask_decode(sm, 1.0)) as u8,
        ),
        AreaMinMask::Stencil(_) => (0, 0),
    };
    // The prepared body: the columns that are interior and map into the
    // image. `u` is monotonic in `x`, so they are one run; its base and mask
    // taps are kept compact, apart from the column table. Every other column
    // is the border band, painted generically.
    let body = body_run(
        columns
            .iter()
            .map(|&(interior, axes)| interior && axes.is_some()),
    );
    let (body_start, body_end) = body.unwrap_or((0, 0));
    let (body_taps, body_masks): (Vec<crate::image::AxisTaps>, Vec<MaskAxis>) = columns
        [body_start..body_end]
        .iter()
        .filter_map(|&(_, axes)| axes)
        .unzip();
    let border_cols: Vec<usize> = (0..columns.len())
        .filter(|x| !(body_start..body_end).contains(x))
        .collect();
    let border_cols = border_cols.as_slice();
    // The body's soft-mask texel span, to test a whole row's boxes for clear
    // at once, and its summed tap widths for the tap telemetry.
    let body_mask_span = body_masks.iter().fold(None, |span, mx| match (span, mx) {
        (None, MaskAxis::Taps(m)) => Some((m.lo, m.hi)),
        (Some((lo, hi)), MaskAxis::Taps(m)) => Some((m.lo.min(lo), m.hi.max(hi))),
        (span, MaskAxis::Texel(_)) => span,
    });
    let body_col_taps: u64 = body_taps.iter().map(|t| (t.hi - t.lo + 1) as u64).sum();
    // Whether the body's soft-mask boxes advance with x (an unmirrored draw;
    // `u` is monotonic in `x`, so a mirrored one descends). The alpha pass
    // skips clear runs only in that order.
    let masks_ascending = body_masks.windows(2).all(|pair| match (pair[0], pair[1]) {
        (MaskAxis::Taps(a), MaskAxis::Taps(b)) => a.lo <= b.lo,
        _ => false,
    });

    // A border-band pixel, or one whose center maps outside the image: the
    // generic per-pixel treatment, minus the clip and soft-clip terms this
    // path's eligibility already excluded. Returns whether it painted.
    let paint_generic = |x: usize, y: usize, px: &mut [u8]| -> bool {
        let (dx, dy) = (x as f64 + 0.5, y as f64 + 0.5);
        let edge = img.edge_coverage(dx, dy);
        if edge == Some(0) {
            return false;
        }
        let color = match edge {
            None => img.shade(dx, dy),
            Some(_) => img.shade_clamped(dx, dy),
        };
        let Some(color) = color else { return false };
        let cov = match edge {
            Some(ec) => mul_div_255(255, ec),
            None => 255,
        };
        let a = mul_div_255(color[3] as u16, cov);
        if a == 0 {
            return false;
        }
        composite_px_blended(
            px,
            [color[0], color[1], color[2]],
            a as u8,
            BlendChoice::Normal,
        );
        true
    };

    // The row filter: a body row with many visible pixels (a cover under a
    // mostly opaque layer) is box-averaged a whole row at a time, a sparse
    // one (a scanned page's foreground layer) per visible pixel. Both are
    // exact, so the choice is only about speed.
    let max_row_total = rows
        .iter()
        .filter_map(|&(_, axes)| axes.map(|(ty, _)| ty.total))
        .max();
    let template = max_row_total
        .filter(|_| !body_taps.is_empty())
        .and_then(|rt| RowBoxFilter::new(rgb, stride, &body_taps, rt));

    let (output, first_output_y, output_stride) = surface.rows_mut_abs(y0, y1);
    // Align the row table with the clipped output window.
    let row_offset = first_output_y.saturating_sub(y0);
    let row_slice = rows.get(row_offset..).unwrap_or(&[]);

    // One band of destination rows: reads only its own base/mask source rows
    // and writes only its own output rows, so bands are independent and the
    // per-pixel arithmetic is untouched by how they are scheduled.
    let paint_band = |first_y: usize,
                      band: &mut [u8],
                      band_rows: &[(bool, Option<(crate::image::AxisTaps, MaskAxis)>)]|
     -> (u64, u64) {
        let mut painted = 0u64;
        let mut taps = 0u64;
        let mut filter = template.as_ref().map(RowBoxFilter::fresh);
        let mut alphas = vec![0u8; body_taps.len()];
        let mut colors = vec![0u8; body_taps.len() * 4];
        for (i, (row, &(row_interior, row_axes))) in band
            .chunks_exact_mut(output_stride)
            .zip(band_rows)
            .enumerate()
        {
            let y = first_y + i;
            // The union and intersection of the soft-mask rows this row's
            // boxes span, so a box that is uniformly clear or set — most of a
            // scanned page's foreground mask, most of a cover's opacity layer
            // — is classified without its per-tap filter.
            let summary = match (mask, row_axes) {
                (AreaMinMask::Soft(sm), Some((_, MaskAxis::Taps(mty)))) => {
                    Some(MaskRowSummary::bits(sm, &mty))
                }
                (AreaMinMask::Soft8(sm), Some((_, MaskAxis::Taps(mty)))) => {
                    Some(MaskRowSummary::bytes(sm, &mty))
                }
                _ => None,
            };
            // The one-bit mask rows this row's boxes span, with their bit
            // offsets and weights laid out once for the mixed-box filter.
            let mask_rows = match (mask, row_axes) {
                (AreaMinMask::Soft(sm), Some((_, MaskAxis::Taps(mty)))) => {
                    BilevelMaskRows::new(sm, &mty)
                }
                _ => None,
            };
            // Every body box of this row clear, and clear means transparent
            // (no inverting `/Decode`): the body paints nothing, so only the
            // border band remains — the blank lines and margins of a scanned
            // page's foreground layer cost one scan of the mask union.
            if alpha_clear == 0
                && row_interior
                && let (Some(summary), Some((ty, _)), Some((lo, hi))) =
                    (&summary, row_axes, body_mask_span)
                && summary.clear_over(lo, hi)
            {
                taps += body_col_taps * (ty.hi - ty.lo + 1) as u64;
                for &local_x in border_cols {
                    let x = x0 + local_x;
                    if paint_generic(x, y, &mut row[(x - output_origin) * 4..][..4]) {
                        painted += 1;
                    }
                }
                continue;
            }

            // The border band, generically.
            for &local_x in border_cols {
                let x = x0 + local_x;
                if paint_generic(x, y, &mut row[(x - output_origin) * 4..][..4]) {
                    painted += 1;
                }
            }
            let (Some((ty, my)), true) = (row_axes, row_interior && !body_taps.is_empty()) else {
                // Not a body row: the body columns are generic too.
                for x in x0 + body_start..x0 + body_end {
                    if paint_generic(x, y, &mut row[(x - output_origin) * 4..][..4]) {
                        painted += 1;
                    }
                }
                continue;
            };
            taps += body_col_taps * (ty.hi - ty.lo + 1) as u64;

            // Pass 1: each body pixel's mask alpha, first — a transparent
            // pixel skips the box average.
            //
            // A scanned page's foreground mask is clear over most of every
            // row, so the pass walks the union row's set texels: every box
            // that ends before the next set texel is clear, and a whole run
            // of them takes one fill instead of a window test each. Only a
            // box that reaches a set texel is classified per column. The
            // alpha of a clear box is `alpha_clear` either way (a clear
            // window is `uniform == Some(false)`; a box too wide to test
            // filters to coverage 0.0, which decodes to the same byte), so
            // the row's alphas are unchanged.
            let mut visible = 0usize;
            // The first set texel at or after the last queried `lo`, once
            // known; valid for every later box because `lo` only grows.
            let mut next_set: Option<Option<usize>> = None;
            let mut k = 0usize;
            while k < body_masks.len() {
                let mx = body_masks[k];
                if masks_ascending && let (Some(s), MaskAxis::Taps(mtx)) = (&summary, mx) {
                    let lo = mtx.lo as usize;
                    let ns = match next_set {
                        Some(ns) if ns.is_none_or(|p| p >= lo) => ns,
                        _ => {
                            let ns = s.next_set(lo);
                            next_set = Some(ns);
                            ns
                        }
                    };
                    let run_end = match ns {
                        // No set texel from here on: every remaining box is
                        // clear.
                        None => body_masks.len(),
                        Some(p) if p > mtx.hi as usize => {
                            // Every box ending before `p` is clear.
                            k + body_masks[k..]
                                .iter()
                                .take_while(
                                    |m| matches!(m, MaskAxis::Taps(t) if (t.hi as usize) < p),
                                )
                                .count()
                        }
                        Some(_) => k,
                    };
                    if run_end > k {
                        alphas[k..run_end].fill(alpha_clear);
                        if alpha_clear != 0 {
                            visible += run_end - k;
                        }
                        k = run_end;
                        continue;
                    }
                }
                let a = &mut alphas[k];
                k += 1;
                // After the run walk the box is known to reach a set texel,
                // so only the all-set test remains of the uniform check.
                let known_set = masks_ascending && next_set.is_some();
                let uniform = summary.as_ref().and_then(|s| match mx {
                    MaskAxis::Taps(mtx) if known_set => s.all_set(&mtx).then_some(true),
                    MaskAxis::Taps(mtx) => s.uniform(&mtx),
                    MaskAxis::Texel(_) => None,
                });
                let alpha = match (mask, mx, my) {
                    _ if uniform.is_some() => {
                        if uniform == Some(true) {
                            alpha_set
                        } else {
                            alpha_clear
                        }
                    }
                    (AreaMinMask::Soft(sm), MaskAxis::Taps(mtx), MaskAxis::Taps(mty)) => {
                        let coverage = match &mask_rows {
                            Some(rows) if mtx.hi - mtx.lo < 57 => {
                                bilevel_coverage_prepared(sm, rows, &mtx)
                            }
                            _ => bilevel_smask_coverage_windowed(sm, &mtx, &mty),
                        };
                        (255.0 * crate::image::apply_smask_decode(sm, coverage)) as u8
                    }
                    (AreaMinMask::Soft8(sm), MaskAxis::Taps(mtx), MaskAxis::Taps(mty)) => {
                        let coverage = crate::image::gray8_smask_coverage(sm, &mtx, &mty);
                        (255.0 * crate::image::apply_smask_decode(sm, coverage)) as u8
                    }
                    (AreaMinMask::Stencil(sm), MaskAxis::Texel(col), MaskAxis::Texel(srow)) => {
                        if crate::image::stencil_hides_at(sm, col, srow) {
                            0
                        } else {
                            255
                        }
                    }
                    // The axis kinds are built from `mask` above, so the mixed
                    // combinations cannot occur.
                    _ => 0,
                };
                *a = alpha;
                visible += (alpha != 0) as usize;
            }
            if visible == 0 {
                continue;
            }

            // Pass 2: colour and composite the visible body pixels — a dense
            // row through the row filter, a sparse one pixel by pixel.
            let dense = visible * 2 >= body_taps.len()
                && filter
                    .as_mut()
                    .is_some_and(|f| f.average_row(&ty, &mut colors));
            let body_row = &mut row[(x0 + body_start - output_origin) * 4..];
            for (k, ((&alpha, tx), px)) in alphas
                .iter()
                .zip(&body_taps)
                .zip(body_row.chunks_exact_mut(4))
                .enumerate()
            {
                if alpha == 0 {
                    continue;
                }
                let color = if dense {
                    [colors[k * 4], colors[k * 4 + 1], colors[k * 4 + 2]]
                } else {
                    rgb8_box_average(rgb, stride, tx, &ty, tx.total * ty.total)
                };
                composite_px_blended(px, color, alpha, BlendChoice::Normal);
                painted += 1;
            }
        }
        (painted, taps)
    };

    let parallel = image_row_parallel_enabled()
        && dest_w.saturating_mul(dest_h) >= IMAGE_ROW_PAR_PIXEL_THRESHOLD;
    if parallel {
        const BAND: usize = 32;
        output
            .par_chunks_mut(output_stride * BAND)
            .zip(row_slice.par_chunks(BAND))
            .enumerate()
            .map(|(i, (band, rows))| paint_band(first_output_y + i * BAND, band, rows))
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    } else {
        paint_band(first_output_y, output, row_slice)
    }
}

/// Core of the opaque axis-aligned area-minification fast paths. `rgb` is a
/// packed `width*height*3` RGB8 view of the source (the samples themselves for
/// an RGB image; a converted buffer for CMYK). Prepares per-column source X
/// boxes and per-row Y boxes, then box-averages each destination pixel exactly
/// as `image.rs::area_average` does and writes it opaque.
fn area_min_box_average_opaque(
    img: &crate::image::PreparedImage,
    surface: &mut Surface,
    rgb: &[u8],
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
) -> (u64, u64) {
    let stride = img.width as usize * 3;

    let mut source_columns: Vec<Option<crate::image::AxisTaps>> = Vec::with_capacity(x1 - x0);
    for x in x0..x1 {
        let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
        if !(0.0..1.0).contains(&u) {
            source_columns.push(None);
            continue;
        }
        let fx = u * img.width as f64 - 0.5;
        source_columns.push(img.box_taps_x(fx));
    }

    let mut source_rows: Vec<Option<crate::image::AxisTaps>> = Vec::with_capacity(y1 - y0);
    for y in y0..y1 {
        let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
        if !(0.0..1.0).contains(&v) {
            source_rows.push(None);
            continue;
        }
        let fy = (1.0 - v) * img.height as f64 - 0.5;
        source_rows.push(img.box_taps_y(fy));
    }

    let output_origin = surface.origin_x;
    let dest_w = x1 - x0;
    let dest_h = y1 - y0;
    let columns = source_columns.as_slice();

    // The columns that map into the image form one run (u is monotonic in x);
    // the row filter serves it whole.
    let body = body_run(columns.iter().map(Option::is_some));
    let body_taps: Option<Vec<crate::image::AxisTaps>> =
        body.map(|(bx0, bx1)| columns[bx0..bx1].iter().flatten().copied().collect());
    let max_row_total = source_rows.iter().flatten().map(|t| t.total).max();
    let filter_template = body_taps
        .as_deref()
        .zip(max_row_total)
        .and_then(|(cols, rt)| RowBoxFilter::new(rgb, stride, cols, rt));
    if let (Some(template), Some((bx0, _))) = (&filter_template, body) {
        let cols = template.cols;
        let col_span: u64 = cols.iter().map(|t| (t.hi - t.lo + 1) as u64).sum();
        let (output, first_output_y, output_stride) = surface.rows_mut_abs(y0, y1);
        let row_offset = first_output_y.saturating_sub(y0);
        let row_slice = source_rows.get(row_offset..).unwrap_or(&[]);
        let first_target = (x0 + bx0 - output_origin) * 4;
        // One band of destination rows with its own row cache; bands are
        // independent, and each row's bytes do not depend on the cache.
        let paint_band = |band: &mut [u8], band_rows: &[Option<crate::image::AxisTaps>]| {
            let mut filter = template.fresh();
            let (mut painted, mut taps) = (0u64, 0u64);
            for (row, ty) in band.chunks_exact_mut(output_stride).zip(band_rows) {
                let Some(ty) = ty else { continue };
                if !filter.average_row(ty, &mut row[first_target..]) {
                    for (x, tx) in cols.iter().enumerate() {
                        let c = rgb8_box_average(rgb, stride, tx, ty, tx.total * ty.total);
                        let t = first_target + x * 4;
                        row[t..t + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                    }
                }
                painted += cols.len() as u64;
                taps += col_span * (ty.hi - ty.lo + 1) as u64;
            }
            (painted, taps)
        };
        let parallel = image_row_parallel_enabled()
            && dest_w.saturating_mul(dest_h) >= IMAGE_ROW_PAR_PIXEL_THRESHOLD;
        return if parallel {
            const BAND: usize = 32;
            output
                .par_chunks_mut(output_stride * BAND)
                .zip(row_slice.par_chunks(BAND))
                .map(|(band, rows)| paint_band(band, rows))
                .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
        } else {
            paint_band(output, row_slice)
        };
    }

    // One destination row: reads only its own source rows and writes only its
    // own output row, so rows are independent and the per-pixel arithmetic is
    // untouched by how they are scheduled.
    let paint_row = |row_taps: &Option<crate::image::AxisTaps>, row: &mut [u8]| -> (u64, u64) {
        let Some(ty) = *row_taps else {
            return (0, 0);
        };
        let mut painted = 0u64;
        let mut taps = 0u64;
        for (local_x, col_taps) in columns.iter().enumerate() {
            let Some(tx) = *col_taps else { continue };
            let weight = tx.total * ty.total;
            if weight == 0 {
                continue;
            }
            let color = rgb8_box_average(rgb, stride, &tx, &ty, weight);
            let target = (x0 + local_x - output_origin) * 4;
            row[target..target + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
            painted += 1;
            taps += (tx.hi - tx.lo + 1) as u64 * (ty.hi - ty.lo + 1) as u64;
        }
        (painted, taps)
    };

    let (output, first_output_y, output_stride) = surface.rows_mut_abs(y0, y1);
    // Align the row table with the clipped output window.
    let row_offset = first_output_y.saturating_sub(y0);
    let row_slice = source_rows.get(row_offset..).unwrap_or(&[]);

    // Same fan-out policy as the sibling bilinear fast path: below this many
    // destination pixels the rayon scheduling costs more than it saves.
    let parallel = image_row_parallel_enabled()
        && dest_w.saturating_mul(dest_h) >= IMAGE_ROW_PAR_PIXEL_THRESHOLD;
    let (painted, taps) = if parallel {
        output
            .par_chunks_mut(output_stride)
            .zip(row_slice.par_iter())
            .map(|(row, ty)| paint_row(ty, row))
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    } else {
        output
            .chunks_exact_mut(output_stride)
            .zip(row_slice.iter())
            .map(|(row, ty)| paint_row(ty, row))
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    };
    (painted, taps)
}

/// Weighted box average of a packed RGB8 source over one destination pixel's
/// prepared tap boxes: per channel `(Σ wy·wx·texel + weight/2) / weight`,
/// clamped to 255 — the integer arithmetic of `PreparedImage::area_average`'s
/// RGB8 branch, tap for tap. `weight` is `tx.total * ty.total` (non-zero, the
/// caller's check).
#[inline(always)]
fn rgb8_box_average(
    rgb: &[u8],
    stride: usize,
    tx: &crate::image::AxisTaps,
    ty: &crate::image::AxisTaps,
    weight: u64,
) -> [u8; 3] {
    let (mut a0, mut a1, mut a2) = (0u64, 0u64, 0u64);
    for sr in ty.lo..=ty.hi {
        let wy = ty.weight_at(sr);
        let base = sr as usize * stride + tx.lo as usize * 3;
        let end = sr as usize * stride + (tx.hi as usize + 1) * 3;
        for (col, texel) in (tx.lo..).zip(rgb[base..end].chunks_exact(3)) {
            let w = wy * tx.weight_at(col);
            a0 += w * texel[0] as u64;
            a1 += w * texel[1] as u64;
            a2 += w * texel[2] as u64;
        }
    }
    [
        ((a0 + weight / 2) / weight).min(255) as u8,
        ((a1 + weight / 2) / weight).min(255) as u8,
        ((a2 + weight / 2) / weight).min(255) as u8,
    ]
}

/// The single run of `true` in `flags` as `(start, end)`, or `None` when
/// there is none or more than one.
fn body_run(flags: impl Iterator<Item = bool>) -> Option<(usize, usize)> {
    let mut run: Option<(usize, usize)> = None;
    for (i, flag) in flags.enumerate() {
        if !flag {
            continue;
        }
        run = match run {
            None => Some((i, i + 1)),
            Some((start, end)) if end == i => Some((start, i + 1)),
            Some(_) => return None,
        };
    }
    run
}

/// An exact box filter that averages a whole destination row at once over a
/// contiguous run of destination columns (`cols`, their prepared X taps).
///
/// The per-pixel filter sums `Σ_rows wy · Σ_cols wx · texel` for every
/// destination pixel and divides three times; the rows of one destination row
/// share their row weights, and neighbouring destination rows share most of
/// their source rows. Here each source row's horizontal sums `Σ wx · texel`
/// are computed once per band of destination rows and cached; a destination
/// row is then a weighted sum of a few cached rows and one division per
/// channel, both over contiguous planar arrays the compiler vectorises.
///
/// The arithmetic is carried in `f64` but every value in it is an integer:
/// the horizontal sums, the products with a row weight and their sums stay
/// below 2⁴⁹ (the constructor requires `tx.total · ty.total < 2⁴⁰`, and a
/// texel is at most 255), so all of them are exact, and the sums are the
/// per-pixel filter's `u64` sums regrouped — integer addition is
/// associative. The rounding division `(acc + ⌊w/2⌋) / w`, truncated, is the
/// integer floor: an IEEE quotient is correctly rounded, the quotient is
/// below 256 where the rounding error is under 2⁻⁴⁶, and a quotient that is
/// not an integer is at least `1/w > 2⁻⁴⁰` from the next one, so truncation
/// never crosses an integer. The bytes are the per-pixel filter's.
struct RowBoxFilter<'a> {
    rgb: &'a [u8],
    stride: usize,
    cols: &'a [crate::image::AxisTaps],
    /// `tx.total` per column.
    col_total: Vec<f64>,
    /// When every box is at most four texels wide: per column, the byte
    /// offsets of four texels within a source row and their weights, the
    /// absent ones weighted zero (and pointed at a texel that exists), and
    /// how many of the four the widest box uses. Otherwise empty, and the
    /// horizontal pass sums each box's taps in a loop.
    taps4: Vec<([usize; 4], [u32; 4])>,
    taps_used: usize,
    /// Cached horizontal passes: `(source row, last use, R | G | B planes)`.
    slots: Vec<(u32, u64, Vec<f64>)>,
    clock: u64,
    /// The weighted sums of the current destination row, planar.
    acc: Vec<f64>,
    kernel: ImageKernel,
}

impl<'a> RowBoxFilter<'a> {
    /// Tallest box the filter serves; a taller one (a minification of more
    /// than ~15× on Y) is left to the per-pixel filter.
    const MAX_ROWS: usize = 16;

    /// `None` when a box's total weight could leave the exact range, or the
    /// run is empty. `max_row_total` bounds `ty.total` over the rows served.
    fn new(
        rgb: &'a [u8],
        stride: usize,
        cols: &'a [crate::image::AxisTaps],
        max_row_total: u64,
    ) -> Option<Self> {
        let max_col_total = cols.iter().map(|t| t.total).max()?;
        if max_col_total.checked_mul(max_row_total)? >= 1 << 40 {
            return None;
        }
        let n = cols.len();
        let taps_used = cols.iter().map(|t| (t.hi - t.lo) as usize + 1).max()?;
        let taps4 = if taps_used <= 4 {
            cols.iter()
                .map(|t| {
                    let (lo, span) = (t.lo as usize, (t.hi - t.lo) as usize);
                    let mut offsets = [lo * 3; 4];
                    let mut weights = [0u32; 4];
                    for (k, (o, w)) in offsets.iter_mut().zip(&mut weights).enumerate() {
                        if k <= span {
                            *o = (lo + k) * 3;
                            *w = t.weight_at((lo + k) as u32) as u32;
                        }
                    }
                    (offsets, weights)
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(Self {
            rgb,
            stride,
            cols,
            col_total: cols.iter().map(|t| t.total as f64).collect(),
            taps4,
            taps_used,
            slots: Vec::new(),
            clock: 0,
            acc: vec![0.0; 3 * n],
            kernel: image_kernel(),
        })
    }

    /// The same filter with an empty row cache, for another band of rows.
    fn fresh(&self) -> Self {
        Self {
            rgb: self.rgb,
            stride: self.stride,
            cols: self.cols,
            col_total: self.col_total.clone(),
            taps4: self.taps4.clone(),
            taps_used: self.taps_used,
            slots: Vec::new(),
            clock: 0,
            acc: vec![0.0; self.acc.len()],
            kernel: self.kernel,
        }
    }

    /// `Σ wx · texel` of source row `row` for every column, planar.
    fn horizontal(&self, row: u32, planes: &mut [f64]) {
        let n = self.cols.len();
        let (r, rest) = planes.split_at_mut(n);
        let (g, b) = rest.split_at_mut(n);
        let row_base = row as usize * self.stride;
        let src = &self.rgb[row_base..row_base + self.stride];
        // Weights are at most `S` and texels 255, so four products fit `u32`;
        // the absent taps add `0 · texel`. The tap count is fixed per draw so
        // the loop body is straight-line code.
        let fixed = |taps: usize, r: &mut [f64], g: &mut [f64], b: &mut [f64]| {
            for (x, (o, w)) in self.taps4.iter().enumerate() {
                let mut sum = [0u32; 3];
                for k in 0..taps {
                    let t = &src[o[k]..o[k] + 3];
                    sum[0] += w[k] * t[0] as u32;
                    sum[1] += w[k] * t[1] as u32;
                    sum[2] += w[k] * t[2] as u32;
                }
                r[x] = sum[0] as f64;
                g[x] = sum[1] as f64;
                b[x] = sum[2] as f64;
            }
        };
        match (self.taps4.is_empty(), self.taps_used) {
            (false, 1..=2) => return fixed(2, r, g, b),
            (false, 3) => return fixed(3, r, g, b),
            (false, _) => return fixed(4, r, g, b),
            (true, _) => {}
        }
        for (x, tx) in self.cols.iter().enumerate() {
            let base = row_base + tx.lo as usize * 3;
            let end = row_base + (tx.hi as usize + 1) * 3;
            let (mut a0, mut a1, mut a2) = (0u64, 0u64, 0u64);
            for (col, texel) in (tx.lo..).zip(self.rgb[base..end].chunks_exact(3)) {
                let w = tx.weight_at(col);
                a0 += w * texel[0] as u64;
                a1 += w * texel[1] as u64;
                a2 += w * texel[2] as u64;
            }
            r[x] = a0 as f64;
            g[x] = a1 as f64;
            b[x] = a2 as f64;
        }
    }

    /// The cache slot holding source row `row`, filling one if needed. Rows
    /// in `keep` (the box being assembled) are never evicted.
    fn slot(&mut self, row: u32, keep: (u32, u32)) -> usize {
        self.clock += 1;
        if let Some(i) = self.slots.iter().position(|s| s.0 == row) {
            self.slots[i].1 = self.clock;
            return i;
        }
        let i = if self.slots.len() <= Self::MAX_ROWS {
            self.slots.push((row, 0, vec![0.0; 3 * self.cols.len()]));
            self.slots.len() - 1
        } else {
            // More slots than the tallest box, so one lies outside it.
            self.slots
                .iter()
                .enumerate()
                .filter(|(_, s)| s.0 < keep.0 || s.0 > keep.1)
                .min_by_key(|(_, s)| s.1)
                .map_or(0, |(i, _)| i)
        };
        let mut planes = std::mem::take(&mut self.slots[i].2);
        self.horizontal(row, &mut planes);
        self.slots[i] = (row, self.clock, planes);
        i
    }

    /// Average every column over the rows `ty` and write the pixels opaque
    /// into `dst` (RGBA, one pixel per column). `false` when the box is taller
    /// than [`Self::MAX_ROWS`] (nothing is written).
    fn average_row(&mut self, ty: &crate::image::AxisTaps, dst: &mut [u8]) -> bool {
        if (ty.hi - ty.lo) as usize >= Self::MAX_ROWS {
            return false;
        }
        self.acc.fill(0.0);
        for r in ty.lo..=ty.hi {
            let i = self.slot(r, (ty.lo, ty.hi));
            box_accumulate(
                self.kernel,
                &mut self.acc,
                &self.slots[i].2,
                ty.weight_at(r) as f64,
            );
        }
        box_finish(
            self.kernel,
            &self.acc,
            &self.col_total,
            ty.total as f64,
            dst,
        );
        true
    }
}

/// `acc[i] += wy · h[i]`: one cached source row's horizontal sums, weighted,
/// into a destination row's planar sums. Exact (see [`RowBoxFilter`]).
#[inline]
#[allow(
    unsafe_code,
    reason = "dispatches the AVX2 build of the loop behind a runtime probe; see SAFETY comment"
)]
fn box_accumulate(kernel: ImageKernel, acc: &mut [f64], h: &[f64], wy: f64) {
    match kernel {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: AVX2 is present (`image_kernel` probed it); the body is
        // safe code.
        ImageKernel::Avx2 => unsafe { box_accumulate_avx2(acc, h, wy) },
        ImageKernel::Scalar => box_accumulate_body(acc, h, wy),
    }
}

#[inline(always)]
fn box_accumulate_body(acc: &mut [f64], h: &[f64], wy: f64) {
    for (a, &v) in acc.iter_mut().zip(h) {
        *a += wy * v;
    }
}

/// [`box_accumulate_body`] compiled for AVX2, which the compiler vectorises
/// four lanes wide (separate multiply and add, never fused: Rust does not
/// contract floating-point expressions).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn box_accumulate_avx2(acc: &mut [f64], h: &[f64], wy: f64) {
    box_accumulate_body(acc, h, wy);
}

/// Divide a destination row's planar sums (`acc`: `R | G | B`, one entry per
/// column) by each box's weight `row_total · col_total[x]`, rounding to
/// nearest as `(acc + ⌊w/2⌋) / w` does in integers — computed as
/// `trunc((acc + w/2) / w)`, which is the same floor (see [`RowBoxFilter`];
/// the half-integer numerator cannot reach the next multiple of `w` either)
/// — and write the pixels opaque RGBA into `dst`.
#[inline]
#[allow(
    unsafe_code,
    reason = "dispatches the AVX2 kernel behind a runtime probe; see SAFETY comment"
)]
fn box_finish(kernel: ImageKernel, acc: &[f64], col_total: &[f64], row_total: f64, dst: &mut [u8]) {
    let n = col_total.len();
    // Bounds-check once, here, so both kernels share one invariant.
    let dst = &mut dst[..n * 4];
    let acc = &acc[..n * 3];
    match kernel {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: AVX2 is present (`image_kernel` probed it); `acc` holds
        // `3n` sums, `col_total` `n` totals and `dst` `4n` bytes, which is all
        // the kernel reads and writes.
        ImageKernel::Avx2 => unsafe { box_finish_avx2(acc, col_total, row_total, dst) },
        ImageKernel::Scalar => box_finish_scalar(acc, col_total, row_total, dst, 0),
    }
}

/// Scalar reference for [`box_finish`], from column `start` on.
#[inline]
fn box_finish_scalar(acc: &[f64], col_total: &[f64], row_total: f64, dst: &mut [u8], start: usize) {
    let n = col_total.len();
    for x in start..n {
        let w = row_total * col_total[x];
        let q = |a: f64| ((a + 0.5 * w) / w) as i32 as u8;
        dst[x * 4..x * 4 + 4].copy_from_slice(&[q(acc[x]), q(acc[n + x]), q(acc[2 * n + x]), 255]);
    }
}

/// AVX2 [`box_finish`]: four columns per iteration — the same multiply, add,
/// divide and truncation as the scalar reference, lane for lane — packed to
/// four RGBA pixels with one store.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(
    unsafe_code,
    reason = "AVX2 intrinsics over bounds-checked slices; see SAFETY comments"
)]
fn box_finish_avx2(acc: &[f64], col_total: &[f64], row_total: f64, dst: &mut [u8]) {
    use std::arch::x86_64::*;
    let n = col_total.len();
    let (ar, rest) = acc.split_at(n);
    let (ag, ab) = rest.split_at(n);
    let rt = _mm256_set1_pd(row_total);
    let half = _mm256_set1_pd(0.5);
    let alpha = _mm_set1_epi32(0xFF00_0000u32 as i32);
    let mut x = 0;
    while x + 4 <= n {
        // SAFETY: `x + 4 <= n` and every plane and `col_total` hold `n`
        // entries; `dst` holds `4n` bytes, so the 16-byte store at `4x` is in
        // bounds.
        unsafe {
            let w = _mm256_mul_pd(rt, _mm256_loadu_pd(col_total.as_ptr().add(x)));
            let hw = _mm256_mul_pd(half, w);
            let q = |plane: &[f64]| {
                let a = _mm256_loadu_pd(plane.as_ptr().add(x));
                _mm256_cvttpd_epi32(_mm256_div_pd(_mm256_add_pd(a, hw), w))
            };
            let (r, g, b) = (q(ar), q(ag), q(ab));
            let px = _mm_or_si128(
                _mm_or_si128(r, _mm_slli_epi32::<8>(g)),
                _mm_or_si128(_mm_slli_epi32::<16>(b), alpha),
            );
            _mm_storeu_si128(dst.as_mut_ptr().add(x * 4).cast::<__m128i>(), px);
        }
        x += 4;
    }
    box_finish_scalar(acc, col_total, row_total, dst, x);
}

/// The union and intersection of the soft-mask rows one destination row's
/// boxes span (`bits`: packed one-bit rows; `bytes`: eight-bit rows), padded
/// so a 64-bit window read never leaves the buffer. A box whose texels are
/// all clear or all set has an exact coverage in both mask filters — `0.0`,
/// or the full weight over itself, `1.0`: a one-bit box's weighted ones equal
/// `tx.total · ty.total` exactly when every bit is set (every tap weighs at
/// least 1), and an eight-bit box of `255` texels sums the integral products
/// `wy · wx · 1.0` without rounding (they stay far below 2⁵³) — so its alpha
/// is known without running the filter.
struct MaskRowSummary {
    any: Vec<u8>,
    all: Vec<u8>,
    one_bit: bool,
}

impl MaskRowSummary {
    fn new(sm: &pdf_page_ir::ImageSMask, rows: &crate::image::AxisTaps, one_bit: bool) -> Self {
        let row_bytes = crate::image::smask_row_bits(sm) / 8;
        let mut any = vec![0u8; row_bytes + 8];
        let mut all = vec![0u8; row_bytes + 8];
        all[..row_bytes].fill(0xff);
        for r in rows.lo..=rows.hi {
            // Bytes past the end of the samples read as zero, as the filters'
            // own reads do.
            let src = sm
                .samples
                .get(r as usize * row_bytes..)
                .map_or(&[][..], |s| &s[..s.len().min(row_bytes)]);
            for ((a, b), &v) in any.iter_mut().zip(all.iter_mut()).zip(src) {
                *a |= v;
                *b &= v;
            }
            all[src.len()..row_bytes].fill(0);
        }
        Self { any, all, one_bit }
    }

    fn bits(sm: &pdf_page_ir::ImageSMask, rows: &crate::image::AxisTaps) -> Self {
        Self::new(sm, rows, true)
    }

    fn bytes(sm: &pdf_page_ir::ImageSMask, rows: &crate::image::AxisTaps) -> Self {
        Self::new(sm, rows, false)
    }

    /// Whether every texel in columns `lo..=hi` of the spanned rows is clear.
    fn clear_over(&self, lo: u32, hi: u32) -> bool {
        let (lo, hi) = (lo as usize, hi as usize);
        if !self.one_bit {
            return self.any[lo..=hi].iter().all(|&v| v == 0);
        }
        let (first, last) = (lo / 8, hi / 8);
        let head = 0xffu8 >> (lo % 8);
        let tail = 0xffu8 << (7 - hi % 8);
        if first == last {
            return self.any[first] & head & tail == 0;
        }
        self.any[first] & head == 0
            && self.any[last] & tail == 0
            && self.any[first + 1..last].iter().all(|&v| v == 0)
    }

    /// The first texel at or after `from` that is set in any spanned row, or
    /// `None` when the rest of the row is clear.
    #[inline]
    fn next_set(&self, from: usize) -> Option<usize> {
        if !self.one_bit {
            let rest = self.any.get(from..)?;
            return rest.iter().position(|&v| v != 0).map(|i| from + i);
        }
        let byte = from / 8;
        let head = self.any.get(byte)? & (0xffu8 >> (from % 8));
        if head != 0 {
            return Some(byte * 8 + head.leading_zeros() as usize);
        }
        let rest = self.any.get(byte + 1..)?;
        let i = rest.iter().position(|&v| v != 0)?;
        Some((byte + 1 + i) * 8 + rest[i].leading_zeros() as usize)
    }

    /// Whether every texel of the box is set, for a box already known to
    /// hold a set texel: the second half of [`Self::uniform`], with the same
    /// width limit (a wider box reads as not all set, and filters).
    #[inline]
    fn all_set(&self, cols: &crate::image::AxisTaps) -> bool {
        if self.one_bit {
            let width = (cols.hi - cols.lo + 1) as usize;
            width <= 57 && bit_window(&self.all, cols.lo as usize, width) == (1u64 << width) - 1
        } else {
            self.all[cols.lo as usize..=cols.hi as usize]
                .iter()
                .all(|&v| v == 0xff)
        }
    }

    /// `Some(false)` when every texel of the box is clear, `Some(true)` when
    /// every one is set, `None` when it is mixed (or too wide to test in one
    /// window, which only costs the shortcut).
    #[inline]
    fn uniform(&self, cols: &crate::image::AxisTaps) -> Option<bool> {
        if self.one_bit {
            let width = (cols.hi - cols.lo + 1) as usize;
            if width > 57 {
                return None;
            }
            let full = (1u64 << width) - 1;
            if bit_window(&self.any, cols.lo as usize, width) == 0 {
                Some(false)
            } else if bit_window(&self.all, cols.lo as usize, width) == full {
                Some(true)
            } else {
                None
            }
        } else {
            let range = cols.lo as usize..=cols.hi as usize;
            if self.any[range.clone()].iter().all(|&v| v == 0) {
                Some(false)
            } else if self.all[range].iter().all(|&v| v == 0xff) {
                Some(true)
            } else {
                None
            }
        }
    }
}

/// `width` (≤ 57) bits of an MSB-first packed buffer starting at bit `bit`,
/// right-aligned: the first bit lands at `width - 1`, the last at 0. Bytes
/// past the end of `data` read as zero.
#[inline(always)]
fn bit_window(data: &[u8], bit: usize, width: usize) -> u64 {
    let byte = bit >> 3;
    let word = match data.get(byte..byte + 8) {
        Some(b) => u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        None => {
            let mut buf = [0u8; 8];
            if let Some(tail) = data.get(byte..) {
                buf[..tail.len()].copy_from_slice(tail);
            }
            u64::from_be_bytes(buf)
        }
    };
    (word << (bit & 7)) >> (64 - width)
}

/// The one-bit mask rows one destination row's boxes span: each row's first
/// bit offset and weight, prepared once per destination row so the mixed-box
/// filter walks them without recomputing the row stride or the weight of
/// every row for every column.
struct BilevelMaskRows {
    n: usize,
    bit_offset: [usize; Self::MAX_ROWS],
    weight: [u64; Self::MAX_ROWS],
    total: u64,
}

impl BilevelMaskRows {
    /// Tallest box prepared; a taller one (a minification of more than ~15×
    /// on Y) takes the per-column filter.
    const MAX_ROWS: usize = 16;

    fn new(sm: &pdf_page_ir::ImageSMask, rows: &crate::image::AxisTaps) -> Option<Self> {
        let n = (rows.hi - rows.lo) as usize + 1;
        if n > Self::MAX_ROWS {
            return None;
        }
        let row_bits = crate::image::smask_row_bits(sm);
        let mut prepared = Self {
            n,
            bit_offset: [0; Self::MAX_ROWS],
            weight: [0; Self::MAX_ROWS],
            total: rows.total,
        };
        for (i, row) in (rows.lo..=rows.hi).enumerate() {
            prepared.bit_offset[i] = row as usize * row_bits;
            prepared.weight[i] = rows.weight_at(row);
        }
        Some(prepared)
    }
}

/// [`bilevel_smask_coverage_windowed`] over prepared rows: the same window
/// per row, the same `w_lo·first + w_hi·last + S·popcount(interior)`, the
/// same saturating row-weighted sum in the same row order, the same `f32`
/// division — so the identical coverage. The box must be at most 57 wide.
#[inline]
fn bilevel_coverage_prepared(
    sm: &pdf_page_ir::ImageSMask,
    rows: &BilevelMaskRows,
    tx: &crate::image::AxisTaps,
) -> f32 {
    const S: u64 = crate::image::AXIS_TAP_SCALE;
    let width = (tx.hi - tx.lo + 1) as usize;
    let weight = tx.total.saturating_mul(rows.total).max(1);
    let mut ones_w = 0u64;
    for i in 0..rows.n {
        let win = bit_window(&sm.samples, rows.bit_offset[i] + tx.lo as usize, width);
        let row_ones = if width == 1 {
            tx.w_lo * win
        } else {
            let interior = (win >> 1) & ((1u64 << (width - 2)) - 1);
            tx.w_lo * (win >> (width - 1)) + tx.w_hi * (win & 1) + S * interior.count_ones() as u64
        };
        ones_w = ones_w.saturating_add(rows.weight[i].saturating_mul(row_ones));
    }
    ones_w as f32 / weight as f32
}

/// [`crate::image::bilevel_smask_coverage`] with each mask row's taps read as
/// one 64-bit window instead of bit by bit: the same `w_lo·first + w_hi·last
/// + S·popcount(interior)` per row, the same saturating row-weighted sum, the
/// same `f32` division — so the identical coverage. Boxes wider than one
/// window take the original.
#[inline]
fn bilevel_smask_coverage_windowed(
    sm: &pdf_page_ir::ImageSMask,
    tx: &crate::image::AxisTaps,
    ty: &crate::image::AxisTaps,
) -> f32 {
    const S: u64 = crate::image::AXIS_TAP_SCALE;
    let width = (tx.hi - tx.lo + 1) as usize;
    if width > 57 {
        return crate::image::bilevel_smask_coverage(sm, tx, ty);
    }
    let row_bits = crate::image::smask_row_bits(sm);
    let weight = tx.total.saturating_mul(ty.total).max(1);
    let mut ones_w = 0u64;
    for row in ty.lo..=ty.hi {
        let win = bit_window(&sm.samples, row as usize * row_bits + tx.lo as usize, width);
        let row_ones = if width == 1 {
            tx.w_lo * win
        } else {
            let interior = (win >> 1) & ((1u64 << (width - 2)) - 1);
            tx.w_lo * (win >> (width - 1)) + tx.w_hi * (win & 1) + S * interior.count_ones() as u64
        };
        ones_w = ones_w.saturating_add(ty.weight_at(row).saturating_mul(row_ones));
    }
    ones_w as f32 / weight as f32
}

/// Referenced source bits above which a minified bilevel draw weights its
/// source rows across the rayon pool even when its destination is small.
/// Below it the serial walk is already a fraction of a millisecond and the
/// fan-out would not pay for itself.
const BILEVEL_ROWS_PAR_MIN_BITS: usize = 1 << 20;

/// Destination-pixel count above which an image draw paints its independent
/// destination rows across the rayon pool. Mirrors the bilinear fast path's
/// own long-standing `PAR_PIXEL_THRESHOLD`.
const IMAGE_ROW_PAR_PIXEL_THRESHOLD: usize = 256 * 256;

/// Page executions running in this process right now. Intra-page row fan-out
/// only pays when the render has the machine to itself: under the document
/// scheduler every core is already carrying another page, and fanning out
/// there oversubscribes the box — measured at −31 % whole-document throughput
/// on a 640-page bilevel scan — for no extra work done.
static PAGES_IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// RAII counter for [`PAGES_IN_FLIGHT`], held for one top-level page execution.
struct PageInFlight;

impl PageInFlight {
    fn enter() -> Self {
        PAGES_IN_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self
    }
}

impl Drop for PageInFlight {
    fn drop(&mut self) {
        PAGES_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How the image row loops decide to fan out over the rayon pool.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowParPolicy {
    /// Always serial.
    Never,
    /// Fan out only while this is the sole page render in flight.
    Auto,
    /// Always fan out above the pixel threshold.
    Always,
}

/// `PDF_RENDERER_IMAGE_ROW_PAR` selects the policy: unset or `auto` is
/// load-aware, `off`/`0`/`false` forces the serial row loop and `on`/`1`/
/// `always` forces the fan-out. The override exists for paired A/B isolation
/// on one binary. Read once.
fn row_par_policy() -> RowParPolicy {
    use std::sync::OnceLock;
    static POLICY: OnceLock<RowParPolicy> = OnceLock::new();
    *POLICY.get_or_init(|| match std::env::var("PDF_RENDERER_IMAGE_ROW_PAR") {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" => RowParPolicy::Never,
            "on" | "1" | "true" | "always" => RowParPolicy::Always,
            _ => RowParPolicy::Auto,
        },
        Err(_) => RowParPolicy::Auto,
    })
}

/// Whether the image row loops should fan their destination rows out over the
/// rayon pool. Consulted once per draw, never per pixel.
fn image_row_parallel_enabled() -> bool {
    match row_par_policy() {
        RowParPolicy::Never => false,
        RowParPolicy::Always => true,
        RowParPolicy::Auto => PAGES_IN_FLIGHT.load(std::sync::atomic::Ordering::Relaxed) <= 1,
    }
}

/// Render a tiling-pattern fill: replicate the compiled cell across the
/// lattice into a fill-sized offscreen, then composite it onto `surface`
/// masked by the fill shape (advice §9-style bounded offscreen).
#[allow(clippy::too_many_arguments)]
fn render_tiling(
    page: &CpuPreparedPage,
    t: &crate::prepared::PreparedTiling,
    surface: &mut Surface,
    raster: &mut RasterKernel,
    kernels: &KernelSet,
    masks: &mut [Option<ClipMask>],
    stats: &mut RenderStats,
) {
    let b = t.bounds;
    let (w, h) = (b.width as usize, b.height as usize);
    let (bx, by) = (b.x as usize, b.y as usize);
    if w == 0 || h == 0 {
        return;
    }

    // Cell instances render into a fill-sized surface whose local origin is
    // the fill's device origin (so cell draws are clipped to the fill bounds).
    let mut pat = Surface::new(w, h, pdf_render_api::Background::Transparent);
    let size = pdf_page_ir::DeviceSize {
        width: b.width,
        height: b.height,
    };
    let shift = pdf_page_ir::Matrix::translate(-(b.x as f64), -(b.y as f64));
    // Tile-invariant state hoisted out of the instance loop (A3): one parsed-
    // font residency shared by every tile instance of this fill. Geometry
    // still lowers per instance — each tile's device transform differs by a
    // generally-fractional translation, and floating-point point transforms
    // are not translation-invariant, so a lower-once-translate-many cell
    // would not be byte-identical to per-instance lowering.
    let mut cell_fonts = crate::prepared::FontProgramCache::default();
    for tile in &t.tiles {
        // A cancelled request stops between tile instances too — an
        // adversarial many-tile fill must not pin the worker.
        if let Some(cancel) = &page.decode_limits.should_cancel
            && cancel()
        {
            stats.cancelled = true;
            return;
        }
        let cell_prepared = crate::prepared::lower_cell(
            &t.cell,
            tile.then(shift),
            size,
            page,
            t.color_policy,
            &mut cell_fonts,
        );
        let mut cmasks: Vec<Option<ClipMask>> =
            (0..cell_prepared.clips.len()).map(|_| None).collect();
        let mut csoft: Vec<Option<ClipMask>> = Vec::new();
        run_ops(
            &cell_prepared,
            0,
            cell_prepared.ops.len(),
            &mut pat,
            raster,
            kernels,
            &mut cmasks,
            &mut csoft,
            stats,
        );
        // A codec image dropped while lowering this tile cell counts too.
        stats.absorb_diagnostics(&cell_prepared.diagnostics);
    }

    // The fill shape as an Alpha8 coverage mask over the bounds.
    let subs = &page.subpaths[t.fill_subpaths.0 as usize..t.fill_subpaths.1 as usize];
    let mut fmask = vec![0u8; w * h];
    let dev_w = page.size.width as usize;
    let dev_h = page.size.height as usize;
    if !raster.fill_cancellable(
        &page.points,
        subs,
        dev_w,
        dev_h,
        t.fill_rule,
        page.decode_limits.should_cancel.as_deref(),
        |y, x0, x1, cov| {
            if y < by || y >= by + h {
                return;
            }
            let ly = y - by;
            let lx0 = x0.max(bx);
            let lx1 = x1.min(bx + w - 1);
            for x in lx0..=lx1 {
                fmask[ly * w + (x - bx)] = cov[x];
            }
        },
    ) {
        stats.cancelled = true;
        return;
    }

    // Intersect with the outer non-rectangular clip mask, if any.
    if t.clip_has_mask {
        // Lowering guarantees a masked draw carries its clip id; skip the
        // draw (never panic a page) if that IR invariant broke upstream.
        let Some(cid) = t.clip else {
            page.diagnostics
                .note_degraded("tiling fill skipped: masked clip without a clip id".into());
            return;
        };
        // Share the nearest path-clip ancestor's mask across rect
        // descendants, as every other draw does: a rect clip contributes only
        // its bounds, which the tile bounds are already intersected with, so
        // inside them the ancestor's mask is this clip's mask, and it is
        // rasterized once instead of once per descendant.
        let cid = page.clips[cid as usize].mask_source.unwrap_or(cid);
        if masks[cid as usize].is_none() {
            let Some(mask) = build_render_clip_mask(page, raster, cid, stats) else {
                return;
            };
            masks[cid as usize] = Some(mask);
        }
        let Some(cm) = masks[cid as usize].as_ref() else {
            return; // unreachable: just built above
        };
        let mw = cm.stride();
        for ly in 0..h {
            let dy = by + ly;
            if dy < cm.bounds.y as usize || dy >= cm.bounds.y as usize + cm.bounds.height as usize {
                for lx in 0..w {
                    fmask[ly * w + lx] = 0;
                }
                continue;
            }
            let base = (dy - cm.bounds.y as usize) * mw;
            let cmx = cm.bounds.x as usize;
            let cmw = cm.bounds.width as usize;
            for lx in 0..w {
                let dx = bx + lx;
                let v = if dx >= cmx && dx < cmx + cmw {
                    cm.data[base + (dx - cmx)]
                } else {
                    0
                };
                fmask[ly * w + lx] = mul_div_255(fmask[ly * w + lx] as u16, v as u16) as u8;
            }
        }
    }

    // An /ImageMask stencil painted with this pattern: the pattern shows only
    // through the stencil's per-pixel coverage. Sampled with the same edge-AA
    // path as an ordinary stencil blit, then folded into the fill mask.
    if let Some(stencil) = &t.stencil {
        for ly in 0..h {
            for lx in 0..w {
                let idx = ly * w + lx;
                if fmask[idx] == 0 {
                    continue;
                }
                let (dx, dy) = ((bx + lx) as f64 + 0.5, (by + ly) as f64 + 0.5);
                let edge = stencil.edge_coverage(dx, dy);
                let scov = if edge == Some(0) {
                    0
                } else {
                    let color = match edge {
                        None => stencil.shade(dx, dy),
                        Some(_) => stencil.shade_clamped(dx, dy),
                    };
                    let base = color.map(|c| c[3] as u16).unwrap_or(0);
                    match edge {
                        Some(ec) => mul_div_255(base, ec),
                        None => base,
                    }
                };
                fmask[idx] = mul_div_255(fmask[idx] as u16, scov) as u8;
            }
        }
    }

    // Composite the tiled offscreen onto `surface`, masked by the fill shape
    // and scaled by the fill's constant alpha.
    let sox = surface.origin_x;
    // Composite blend: `Normal` keeps the exact integer source-over below; a
    // non-Normal pattern fill un-premultiplies the cell pixel and goes
    // through the general per-pixel compositor.
    let blend = choose_blend(t.blend);
    let blend_is_normal = matches!(blend, BlendChoice::Normal);
    for ly in 0..h {
        let dy = by + ly;
        // `pat` is this function's own offscreen (built above), distinct from
        // `surface`, so the row can be borrowed rather than copied.
        let prow = pat.local_row(ly);
        let srow = surface.row_mut(dy);
        for lx in 0..w {
            let cov = fmask[ly * w + lx];
            if cov == 0 {
                continue;
            }
            let src = &prow[lx * 4..lx * 4 + 4];
            if src[3] == 0 {
                continue;
            }
            let kk = mul_div_255(cov as u16, t.alpha as u16);
            let dst = &mut srow[(bx + lx - sox) * 4..(bx + lx - sox) * 4 + 4];
            if !blend_is_normal {
                // Straight source color + effective alpha for the compositor.
                let (rgb, sa) = if t.uncolored {
                    let stencil = src[3] as u16;
                    let a = mul_div_255(mul_div_255(t.under[3] as u16, stencil), kk);
                    ([t.under[0], t.under[1], t.under[2]], a as u8)
                } else {
                    let s3 = src[3] as u16;
                    let un = |ch: usize| ((src[ch] as u16 * 255 + s3 / 2) / s3).min(255) as u8;
                    ([un(0), un(1), un(2)], mul_div_255(s3, kk) as u8)
                };
                composite_px_blended(dst, rgb, sa, blend);
                continue;
            }
            // Effective premultiplied source: the cell pixel (colored) or the
            // under-color through the cell's alpha stencil (uncolored).
            let (spr, sa) = if t.uncolored {
                let stencil = src[3] as u16;
                let a = mul_div_255(mul_div_255(t.under[3] as u16, stencil), kk);
                let pm = |ch: usize| {
                    mul_div_255(
                        mul_div_255(mul_div_255(t.under[ch] as u16, t.under[3] as u16), stencil),
                        kk,
                    ) as u8
                };
                ([pm(0), pm(1), pm(2)], a as u8)
            } else {
                let pm = |ch: usize| mul_div_255(src[ch] as u16, kk) as u8;
                ([pm(0), pm(1), pm(2)], mul_div_255(src[3] as u16, kk) as u8)
            };
            if sa == 0 {
                continue;
            }
            let ia = 255 - sa as u16;
            for ch in 0..3 {
                dst[ch] = (spr[ch] as u16 + mul_div_255(dst[ch] as u16, ia)) as u8;
            }
            dst[3] = (sa as u16 + mul_div_255(dst[3] as u16, ia)) as u8;
        }
    }
    stats.commands += 1;
    stats.ops_painted += 1;
}

/// Evaluate a shading at device point `(dx, dy)`. Returns the straight RGBA
/// ramp color, or `None` when the point falls outside the shading's painted
/// extent (before the axis start / past the end without `/Extend`).
fn shade_pixel(sh: &crate::prepared::PreparedShading, dx: f64, dy: f64) -> Option<[u8; 4]> {
    use crate::prepared::ShadingSpanKind;
    // The shading's `/BBox` clip (§8.7.4.3): a device-space box outside which
    // nothing is painted, regardless of shading type or `/Extend`.
    if let Some(bb) = sh.bbox {
        let (x, y) = (dx as f32, dy as f32);
        if x < bb[0] || x > bb[2] || y < bb[1] || y > bb[3] {
            return None;
        }
    }
    // Pre-rasterized mesh layer: direct device-pixel lookup (alpha 0 = the
    // mesh does not paint here; the /Background, when honored, was baked in).
    if let ShadingSpanKind::Layer { x0, y0, w, h } = sh.kind {
        let lx = dx.floor() as i64 - x0 as i64;
        let ly = dy.floor() as i64 - y0 as i64;
        if lx < 0 || ly < 0 || lx >= w as i64 || ly >= h as i64 {
            return sh.background;
        }
        let c = sh.ramp[ly as usize * w + lx as usize];
        return if c[3] == 0 { None } else { Some(c) };
    }
    // Map the device point back into the shading's coordinate space.
    let p = sh.inv.apply(pdf_page_ir::Point { x: dx, y: dy });
    if let ShadingSpanKind::Grid { domain, gw, gh } = sh.kind {
        let dw = domain[1] - domain[0];
        let dh = domain[3] - domain[2];
        if dw.abs() < 1e-12 || dh.abs() < 1e-12 {
            return sh.background;
        }
        let s = (p.x - domain[0]) / dw;
        let t = (p.y - domain[2]) / dh;
        if !(0.0..=1.0).contains(&s) || !(0.0..=1.0).contains(&t) {
            return sh.background;
        }
        let gx = ((s * gw as f64) as usize).min(gw - 1);
        let gy = ((t * gh as f64) as usize).min(gh - 1);
        return Some(sh.ramp[gy * gw + gx]);
    }
    let s = match sh.kind {
        ShadingSpanKind::Axial { p0, d, dd } => ((p.x - p0[0]) * d[0] + (p.y - p0[1]) * d[1]) / dd,
        // Outside both circles with `/Extend` off: the `/Background`, if any.
        ShadingSpanKind::Radial { c0, c1 } => match radial_param(c0, c1, p.x, p.y, sh.extend) {
            Some(s) => s,
            None => return sh.background,
        },
        // Handled above.
        ShadingSpanKind::Grid { .. } | ShadingSpanKind::Layer { .. } => return None,
    };
    // Past the axis with `/Extend` off: paint the `/Background`, if any.
    let Some(s) = apply_extend(s, sh.extend) else {
        return sh.background;
    };
    let n = sh.ramp.len();
    if n == 0 {
        return None;
    }
    let idx = (s * (n - 1) as f64).round().clamp(0.0, (n - 1) as f64) as usize;
    Some(sh.ramp[idx])
}

/// Color a glyph-coverage span from a shading (a shading-pattern text fill):
/// each covered pixel takes the shading's color at its device center, with the
/// glyph coverage folded into the source alpha. `dst` and `cov` are aligned —
/// `cov[i]`'s device coordinate is `(x0 + i, y)`.
fn shade_glyph_span(
    dst: &mut [u8],
    cov: &[u8],
    sh: &crate::prepared::PreparedShading,
    x0: usize,
    y: usize,
    alpha: u8,
    blend: BlendChoice,
) {
    for (i, &c) in cov.iter().enumerate() {
        if c == 0 {
            continue;
        }
        let Some(color) = shade_pixel(sh, (x0 + i) as f64 + 0.5, y as f64 + 0.5) else {
            continue;
        };
        let a = mul_div_255(c as u16, mul_div_255(color[3] as u16, alpha as u16)) as u8;
        if a == 0 {
            continue;
        }
        composite_px_blended(
            &mut dst[i * 4..i * 4 + 4],
            [color[0], color[1], color[2]],
            a,
            blend,
        );
    }
}

/// Clamp a shading parameter to `[0, 1]` honoring the extend flags; returns
/// `None` when the parameter is out of range and extension is disabled.
#[inline]
fn apply_extend(s: f64, extend: [bool; 2]) -> Option<f64> {
    if s < 0.0 {
        extend[0].then_some(0.0)
    } else if s > 1.0 {
        extend[1].then_some(1.0)
    } else {
        Some(s)
    }
}

/// Solve the radial-shading parameter at point `(px, py)`: the largest `s`
/// (outer circles paint over inner) with a non-negative radius that lies in
/// the painted range. Returns the *unclamped* `s` (extend handling is applied
/// by the caller).
fn radial_param(c0: [f64; 3], c1: [f64; 3], px: f64, py: f64, extend: [bool; 2]) -> Option<f64> {
    let dcx = c1[0] - c0[0];
    let dcy = c1[1] - c0[1];
    let dr = c1[2] - c0[2];
    let fx = px - c0[0];
    let fy = py - c0[1];
    let a = dcx * dcx + dcy * dcy - dr * dr;
    let b = -2.0 * (fx * dcx + fy * dcy + c0[2] * dr);
    let c = fx * fx + fy * fy - c0[2] * c0[2];

    // Candidate roots, largest first.
    let mut roots = [f64::NAN; 2];
    if a.abs() < 1e-9 {
        if b.abs() < 1e-12 {
            return None;
        }
        roots[0] = -c / b;
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let sq = disc.sqrt();
        let r0 = (-b + sq) / (2.0 * a);
        let r1 = (-b - sq) / (2.0 * a);
        roots = if r0 >= r1 { [r0, r1] } else { [r1, r0] };
    }
    for &s in roots.iter() {
        if s.is_nan() {
            continue;
        }
        // Radius must be non-negative at this parameter.
        if c0[2] + s * dr < 0.0 {
            continue;
        }
        let ok = (0.0..=1.0).contains(&s) || (s < 0.0 && extend[0]) || (s > 1.0 && extend[1]);
        if ok {
            return Some(s);
        }
    }
    None
}

/// Rec.601 luma of one premultiplied pixel's RGB bytes.
#[inline]
fn luma601(r: u8, g: u8, b: u8) -> u8 {
    (0.3 * r as f32 + 0.59 * g as f32 + 0.11 * b as f32 + 0.5) as u8
}

/// Derive an `Alpha8` soft mask from a rendered mask-group offscreen: its alpha
/// channel (Alpha) or luminosity (Luminosity / LuminosityBc — for the latter
/// the offscreen was pre-filled with the `/BC` backdrop, and pixels outside
/// the mask's extent take the backdrop's luminosity).
///
/// The `/TR` transfer function (a compile-side sampled 256-entry LUT)
/// applies here: `value = lut[value]` after the per-pixel derivation and to
/// `outside`.
fn derive_soft_mask(
    off: &Surface,
    kind: pdf_page_ir::MaskKind,
    transfer: Option<&[u8; 256]>,
) -> ClipMask {
    let (w, h) = (off.width, off.height);
    let mut data = vec![0u8; w * h];
    for ly in 0..h {
        let row = off.local_row(ly);
        for lx in 0..w {
            let px = &row[lx * 4..lx * 4 + 4];
            let value = match kind {
                pdf_page_ir::MaskKind::Alpha => px[3],
                // Premultiplied RGB already equals the color composited over
                // the backdrop (black, or the pre-filled /BC), so its Rec.601
                // luma is the luminosity mask value.
                pdf_page_ir::MaskKind::Luminosity | pdf_page_ir::MaskKind::LuminosityBc { .. } => {
                    luma601(px[0], px[1], px[2])
                }
            };
            data[ly * w + lx] = apply_transfer(value, transfer);
        }
    }
    ClipMask {
        bounds: DeviceRect {
            x: off.origin_x as i32,
            y: off.origin_y as i32,
            width: w as u32,
            height: h as u32,
        },
        data,
        outside: apply_transfer(soft_mask_outside(kind), transfer),
        // A soft mask, not an elidable rectangular clip.
        all_opaque: false,
    }
}

/// Route one derived mask value through the `/TR` LUT (identity when absent).
#[inline]
fn apply_transfer(value: u8, transfer: Option<&[u8; 256]>) -> u8 {
    match transfer {
        Some(lut) => lut[value as usize],
        None => value,
    }
}

/// The soft-mask value outside the mask's rendered extent: 0 for Alpha and
/// plain Luminosity (black backdrop), the backdrop's luminosity for `/BC`.
fn soft_mask_outside(kind: pdf_page_ir::MaskKind) -> u8 {
    match kind {
        pdf_page_ir::MaskKind::Alpha | pdf_page_ir::MaskKind::Luminosity => 0,
        pdf_page_ir::MaskKind::LuminosityBc { backdrop } => {
            luma601(backdrop[0], backdrop[1], backdrop[2])
        }
    }
}

/// Blit a run of cached glyph coverage bitmaps (PDFium's per-run
/// accumulate-and-blit). Each placement's bbox-tight `u8` coverage is composited
/// at its snapped device position: the fast case (Normal blend, no non-rect clip
/// mask, no soft mask, solid colour) is a single `blend_mask_rect` per glyph,
/// which also counts the covered pixels in the same pass; masked or non-Normal
/// runs take a per-row scratch that applies the clip/soft coverage before the
/// general compositor. No rasterization happens here — that was memoized at
/// lowering time.
#[allow(clippy::too_many_arguments)]
fn paint_glyph_run(
    page: &CpuPreparedPage,
    gr: &PreparedGlyphRun,
    surface: &mut Surface,
    raster: &mut RasterKernel,
    kernels: &KernelSet,
    masks: &mut [Option<ClipMask>],
    soft: Option<&ClipMask>,
    stats: &mut RenderStats,
) {
    #[cfg(feature = "profiling")]
    let command_start = Instant::now();
    stats.commands += 1;

    let mask_cid = if gr.clip_has_mask {
        // Lowering guarantees a masked draw carries its clip id; skip the
        // draw (never panic a page) if that IR invariant broke upstream.
        let Some(cid) = gr.clip else {
            page.diagnostics
                .note_degraded("glyph run skipped: masked clip without a clip id".into());
            return;
        };
        // Use the nearest path-clip ancestor's mask (rect children only narrow
        // bounds, applied per draw): one build shared across all descendants.
        let cid = page.clips[cid as usize].mask_source.unwrap_or(cid) as usize;
        if masks[cid].is_none() {
            let Some(mask) = build_render_clip_mask(page, raster, cid as u32, stats) else {
                return;
            };
            masks[cid] = Some(mask);
        }
        Some(cid)
    } else {
        None
    };
    let raw_mask = mask_cid.and_then(|cid| masks[cid].as_ref());
    // A path clip that rasterized to full coverage over its envelope is
    // rectangular for this draw (already reflected in `bounds`): drop the mask.
    let opaque_clip_elided = raw_mask.is_some_and(|cm| cm.all_opaque);
    let mask = raw_mask.filter(|_| !opaque_clip_elided);

    let blend = choose_blend(gr.blend);
    let simple = matches!(blend, BlendChoice::Normal) && mask.is_none() && soft.is_none();

    let b = gr.bounds;
    let (cbx0, cby0) = (b.x, b.y);
    let cbx1 = b.x + b.width as i32; // exclusive
    let cby1 = b.y + b.height as i32;
    let ox = surface.origin_x;
    let mut scratch: Vec<u8> = Vec::new();
    let mut covered: u64 = 0;

    for pl in &gr.placements {
        let bmp = &pl.bitmap;
        let bw = bmp.width as usize;
        let (gw, gh) = (bmp.width as i32, bmp.height as i32);
        // Device columns/rows this bitmap touches, clamped to the clip envelope.
        let rx0 = pl.dx.max(cbx0);
        let rx1 = (pl.dx + gw).min(cbx1);
        let ry0 = pl.dy.max(cby0);
        let ry1 = (pl.dy + gh).min(cby1);
        if rx0 >= rx1 || ry0 >= ry1 {
            continue;
        }
        let span = (rx1 - rx0) as usize;
        let src_off = (rx0 - pl.dx) as usize;
        let dst0 = (rx0 as usize - ox) * 4;
        let dst1 = (rx1 as usize - ox) * 4;

        if simple && gr.shading.is_none() {
            // The common case: the whole clamped bitmap in one kernel call,
            // which also returns its non-zero coverage count (no second pass).
            let rows = (ry1 - ry0) as usize;
            let (buf, first_y, stride) = surface.rows_mut_abs(ry0 as usize, ry1 as usize);
            if first_y == ry0 as usize && buf.len() == rows * stride {
                let src0 = (ry0 - pl.dy) as usize * bw + src_off;
                covered += (kernels.blend_mask_rect)(
                    &mut buf[dst0..],
                    stride,
                    &bmp.cov[src0..],
                    bw,
                    span,
                    rows,
                    gr.rgb,
                    gr.alpha,
                );
                continue;
            }
            // Rows outside the surface: keep the per-row path's behaviour.
        }

        for y in ry0..ry1 {
            let ly = (y - pl.dy) as usize;
            let src = &bmp.cov[ly * bw + src_off..ly * bw + src_off + span];
            let row = surface.row_mut(y as usize);

            if simple {
                if let Some(sh) = &gr.shading {
                    shade_glyph_span(
                        &mut row[dst0..dst1],
                        src,
                        sh,
                        rx0 as usize,
                        y as usize,
                        gr.alpha,
                        blend,
                    );
                } else {
                    (kernels.blend_mask)(&mut row[dst0..dst1], src, gr.rgb, gr.alpha);
                }
                covered += src.iter().filter(|&&c| c != 0).count() as u64;
                continue;
            }

            // Masked / non-Normal path: apply clip + soft coverage into scratch.
            scratch.clear();
            scratch.extend_from_slice(src);
            if let Some(m) = mask {
                let mw = m.stride();
                let (mbx, mby) = (m.bounds.x as usize, m.bounds.y as usize);
                let (mbw, mbh) = (m.bounds.width as usize, m.bounds.height as usize);
                let in_row = (y as usize) >= mby && (y as usize) < mby + mbh;
                let base = if in_row { (y as usize - mby) * mw } else { 0 };
                for (i, c) in scratch.iter_mut().enumerate() {
                    let x = rx0 as usize + i;
                    let mv = if in_row && x >= mbx && x < mbx + mbw {
                        m.data[base + (x - mbx)]
                    } else {
                        0
                    };
                    *c = mul_div_255(*c as u16, mv as u16) as u8;
                }
            }
            if let Some(sm) = soft {
                let sw = sm.stride();
                let (sbx, sby) = (sm.bounds.x as usize, sm.bounds.y as usize);
                let (sbw, sbh) = (sm.bounds.width as usize, sm.bounds.height as usize);
                let in_row = (y as usize) >= sby && (y as usize) < sby + sbh;
                let base = if in_row { (y as usize - sby) * sw } else { 0 };
                for (i, c) in scratch.iter_mut().enumerate() {
                    let x = rx0 as usize + i;
                    let sv = if in_row && x >= sbx && x < sbx + sbw {
                        sm.data[base + (x - sbx)]
                    } else {
                        sm.outside
                    };
                    *c = mul_div_255(*c as u16, sv as u16) as u8;
                }
            }
            covered += scratch.iter().filter(|&&c| c != 0).count() as u64;
            let dst = &mut row[dst0..dst1];
            if let Some(sh) = &gr.shading {
                shade_glyph_span(dst, &scratch, sh, rx0 as usize, y as usize, gr.alpha, blend);
            } else {
                match blend {
                    BlendChoice::Normal => (kernels.blend_mask)(dst, &scratch, gr.rgb, gr.alpha),
                    BlendChoice::Separable(f) => blend_span(dst, &scratch, gr.rgb, gr.alpha, f),
                    BlendChoice::NonSeparable(f) => {
                        blend_span_nonsep(dst, &scratch, gr.rgb, gr.alpha, f)
                    }
                }
            }
        }
    }

    stats.covered_pixels += covered;
    stats.ops_painted += 1;
    #[cfg(feature = "profiling")]
    {
        let profile = &mut stats.profile;
        let elapsed = command_start.elapsed();
        profile.add_duration("render.path", elapsed);
        profile.add_duration("render.path.glyph_runs", elapsed);
        profile.increment("render.glyph_runs", 1);
        profile.increment("render.glyph_blits", gr.placements.len() as u64);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_command(
    page: &CpuPreparedPage,
    cmd: &PreparedCommand,
    surface: &mut Surface,
    raster: &mut RasterKernel,
    kernels: &KernelSet,
    masks: &mut [Option<ClipMask>],
    soft: Option<&ClipMask>,
    stats: &mut RenderStats,
) {
    #[cfg(feature = "profiling")]
    let command_start = Instant::now();
    stats.commands += 1;
    #[cfg(feature = "profiling")]
    let mut dispatch_elapsed = std::time::Duration::ZERO;
    let mask_cid = if cmd.clip_has_mask {
        // Lowering guarantees a masked draw carries its clip id; skip the
        // draw (never panic a page) if that IR invariant broke upstream.
        let Some(cid) = cmd.clip else {
            page.diagnostics
                .note_degraded("fill skipped: masked clip without a clip id".into());
            return;
        };
        // Share the nearest path-clip ancestor's mask across rect descendants.
        let cid = page.clips[cid as usize].mask_source.unwrap_or(cid) as usize;
        if masks[cid].is_none() {
            let Some(mask) = build_render_clip_mask(page, raster, cid as u32, stats) else {
                return;
            };
            masks[cid] = Some(mask);
        }
        Some(cid)
    } else {
        None
    };

    match cmd.class {
        DrawClass::OpaqueRect => {
            paint_opaque_rect(cmd, surface, kernels);
            // An opaque rect marks its whole device rectangle; count it toward
            // coverage so an opaque-fill-only page is never a "silent blank".
            let b = cmd.bounds;
            stats.covered_pixels += b.width as u64 * b.height as u64;
            stats.ops_painted += 1;
        }
        DrawClass::SolidPath => {
            let (rs, re) = cmd.subpath_range;
            let subs = &page.subpaths[rs as usize..re as usize];
            let b = cmd.bounds;
            let (bx0, by0) = (b.x as usize, b.y as usize);
            let bx1 = (b.x as i64 + b.width as i64) as usize; // exclusive
            let by1 = (b.y as i64 + b.height as i64) as usize;
            // A path clip that rasterized to full coverage over its envelope
            // is rectangular for this fill, as for glyphs and images: drop the
            // mask so a solid fill keeps the run-aware path. Exact: the mask
            // multiply already skips 255, and the fill lies inside the mask
            // (checked, since pixels outside it would read `outside`).
            let mask = mask_cid
                .and_then(|cid| masks[cid].as_ref())
                .filter(|m| !(m.all_opaque && rect_contains(m.bounds, b)));
            let (rgb, alpha, opaque, premul) = (cmd.rgb, cmd.alpha, cmd.opaque, cmd.premul);
            let blend = choose_blend(cmd.blend);
            let shading = cmd.shading.as_deref();
            let stencil = cmd.stencil.as_deref();
            // The coverage buffer is sized to the device plane, not the
            // (possibly offset) surface; pass full device dims.
            let width = surface.origin_x + surface.width;
            let height = surface.origin_y + surface.height;
            // A solid Normal fill with no per-pixel modulation paints the
            // raster's constant runs directly; anything else wants the plain
            // coverage buffer.
            let runs_direct = mask.is_none()
                && soft.is_none()
                && stencil.is_none()
                && shading.is_none()
                && matches!(blend, BlendChoice::Normal);

            if !raster.fill_runs_cancellable(
                &page.points,
                subs,
                width,
                height,
                cmd.rule,
                page.decode_limits.should_cancel.as_deref(),
                |y, x0, x1, cov, runs| {
                    if y < by0 || y >= by1 {
                        return false;
                    }
                    #[cfg(feature = "profiling")]
                    let dispatch_start = Instant::now();
                    let dirty = if runs_direct {
                        dispatch_runs(
                            surface, kernels, y, x0, x1, cov, runs, bx0, bx1, rgb, alpha, opaque,
                            premul,
                        );
                        false
                    } else {
                        for r in runs {
                            if let Some(c) = r.fill {
                                cov[r.x0 as usize..r.x1 as usize].fill(c);
                            }
                        }
                        dispatch_row(
                            surface, kernels, y, x0, x1, cov, bx0, bx1, mask, soft, rgb, alpha,
                            opaque, premul, blend, shading, stencil,
                        );
                        true
                    };
                    #[cfg(feature = "profiling")]
                    {
                        dispatch_elapsed += dispatch_start.elapsed();
                    }
                    dirty
                },
            ) {
                stats.cancelled = true;
                return;
            }
            stats.ops_painted += 1;
            stats.edges += raster.last_edges;
            stats.rows_rasterized += raster.last_rows;
            stats.covered_pixels += raster.last_covered;
        }
    }
    #[cfg(feature = "profiling")]
    {
        let profile = &mut stats.profile;
        let elapsed = command_start.elapsed();
        profile.add_duration("render.path", elapsed);
        match cmd.class {
            DrawClass::OpaqueRect => {
                profile.add_duration("render.path.opaque_rects", elapsed);
                profile.increment("render.opaque_rects", 1);
            }
            DrawClass::SolidPath => {
                // Span classification + kernel time inside the coverage
                // callback; the remainder of `render.path.solid_paths` is
                // edge build + coverage accumulation in `RasterKernel`.
                profile.add_duration("render.path.solid_paths", elapsed);
                profile.add_duration("render.path.solid_dispatch", dispatch_elapsed);
                profile.increment("render.solid_paths", 1);
            }
        }
    }
}

/// Whether `inner` lies entirely inside `outer` (an empty `inner` always does).
fn rect_contains(outer: DeviceRect, inner: DeviceRect) -> bool {
    if inner.width == 0 || inner.height == 0 {
        return true;
    }
    let (ox1, oy1) = (
        outer.x as i64 + outer.width as i64,
        outer.y as i64 + outer.height as i64,
    );
    let (ix1, iy1) = (
        inner.x as i64 + inner.width as i64,
        inner.y as i64 + inner.height as i64,
    );
    inner.x >= outer.x && inner.y >= outer.y && ix1 <= ox1 && iy1 <= oy1
}

/// Composite an isolated group's offscreen surface back onto `parent` with
/// constant `opacity` and blend mode.
fn composite_group(
    parent: &mut Surface,
    group: &Surface,
    opacity: u8,
    blend: BlendChoice,
    soft: Option<&ClipMask>,
) {
    let pox = parent.origin_x;
    for ly in 0..group.height {
        let abs_y = group.origin_y + ly;
        let grow = group.local_row(ly);
        let prow = parent.row_mut(abs_y);
        for lx in 0..group.width {
            let src = &grow[lx * 4..lx * 4 + 4];
            if src[3] == 0 {
                continue;
            }
            let alpha = match soft {
                Some(sm) => {
                    let sv = soft_mask_at(sm, group.origin_x + lx, abs_y);
                    if sv == 0 {
                        continue;
                    }
                    mul_div_255(opacity as u16, sv as u16) as u8
                }
                None => opacity,
            };
            let plx = group.origin_x + lx - pox;
            composite_layer_px(&mut prow[plx * 4..plx * 4 + 4], src, alpha, blend);
        }
    }
}

/// Sample a soft mask at absolute device coordinates, yielding `sm.outside`
/// beyond its rendered extent (the same convention the per-paint path uses).
#[inline]
fn soft_mask_at(sm: &ClipMask, x: usize, y: usize) -> u8 {
    let (sbx, sby) = (sm.bounds.x as usize, sm.bounds.y as usize);
    let (sbw, sbh) = (sm.bounds.width as usize, sm.bounds.height as usize);
    if y >= sby && y < sby + sbh && x >= sbx && x < sbx + sbw {
        sm.data[(y - sby) * sm.stride() + (x - sbx)]
    } else {
        sm.outside
    }
}

/// Fold a *seeded* non-isolated group (Normal composite blend) back onto its
/// parent. The offscreen already holds `backdrop ∘ content`, so we do not
/// re-composite the backdrop; we linearly blend it over the parent at the
/// group's constant opacity: `parent = lerp(parent, offscreen, opacity)`. At
/// opacity 1 this is a replace (exact for the group); at opacity 0 a no-op.
/// Since offscreen and parent share the backdrop, untouched pixels lerp to
/// themselves — no darkening halo around the group's marks.
fn composite_group_seeded(
    parent: &mut Surface,
    group: &Surface,
    opacity: u8,
    soft: Option<&ClipMask>,
) {
    if opacity == 0 {
        return;
    }
    let pox = parent.origin_x;
    // Fully opaque and unmasked, every pixel takes the `t == 255` copy below,
    // so whole rows can be copied at once (the common wrapper group).
    let whole_rows = opacity == 255 && soft.is_none();
    for ly in 0..group.height {
        let abs_y = group.origin_y + ly;
        let grow = group.local_row(ly);
        let prow = parent.row_mut(abs_y);
        if whole_rows {
            let plx = group.origin_x - pox;
            prow[plx * 4..(plx + group.width) * 4].copy_from_slice(&grow[..group.width * 4]);
            continue;
        }
        for lx in 0..group.width {
            let src = &grow[lx * 4..lx * 4 + 4];
            // Masking the lerp weight is exactly right here: at mask 0 the
            // parent keeps the backdrop it already shares with the offscreen.
            let t = match soft {
                Some(sm) => {
                    let sv = soft_mask_at(sm, group.origin_x + lx, abs_y);
                    if sv == 0 {
                        continue;
                    }
                    mul_div_255(opacity as u16, sv as u16)
                }
                None => opacity as u16,
            };
            let plx = group.origin_x + lx - pox;
            let dst = &mut prow[plx * 4..plx * 4 + 4];
            if t == 255 {
                dst.copy_from_slice(src);
                continue;
            }
            for c in 0..4 {
                // lerp(dst, src, t/255) in integer space.
                dst[c] =
                    (mul_div_255(dst[c] as u16, 255 - t) + mul_div_255(src[c] as u16, t)) as u8;
            }
        }
    }
}

/// Composite one straight-alpha source color onto a premultiplied pixel under
/// an arbitrary blend choice. `rgb` is the straight source color; `a` is the
/// *effective* source alpha (coverage, constant alpha, and the source color's
/// own alpha already folded in). The `Normal` arm is the exact integer
/// source-over that the fast paths inline (byte-identical); the non-Normal
/// arms are the per-pixel-color analog of `blend_span`/`blend_span_nonsep`,
/// used by paints whose color varies per pixel (shadings, images, tiling
/// cells).
#[inline]
fn composite_px_blended(px: &mut [u8], rgb: [u8; 3], a: u8, blend: BlendChoice) {
    if a == 0 {
        return;
    }
    match blend {
        BlendChoice::Normal => {
            let a = a as u16;
            let ia = 255 - a;
            px[0] = (mul_div_255(rgb[0] as u16, a) + mul_div_255(px[0] as u16, ia)) as u8;
            px[1] = (mul_div_255(rgb[1] as u16, a) + mul_div_255(px[1] as u16, ia)) as u8;
            px[2] = (mul_div_255(rgb[2] as u16, a) + mul_div_255(px[2] as u16, ia)) as u8;
            px[3] = (a + mul_div_255(px[3] as u16, ia)) as u8;
        }
        BlendChoice::Separable(f) => {
            let cs = [
                rgb[0] as f32 / 255.0,
                rgb[1] as f32 / 255.0,
                rgb[2] as f32 / 255.0,
            ];
            let a_s = a as f32 / 255.0;
            let da = px[3] as f32 / 255.0;
            let inv_da = if da > 0.0 { 1.0 / da } else { 0.0 };
            let b = [
                f(px[0] as f32 / 255.0 * inv_da, cs[0]),
                f(px[1] as f32 / 255.0 * inv_da, cs[1]),
                f(px[2] as f32 / 255.0 * inv_da, cs[2]),
            ];
            composite_px(px, cs, a_s, b, da);
        }
        BlendChoice::NonSeparable(f) => {
            let cs = [
                rgb[0] as f32 / 255.0,
                rgb[1] as f32 / 255.0,
                rgb[2] as f32 / 255.0,
            ];
            let a_s = a as f32 / 255.0;
            let da = px[3] as f32 / 255.0;
            let inv_da = if da > 0.0 { 1.0 / da } else { 0.0 };
            let cb = [
                px[0] as f32 / 255.0 * inv_da,
                px[1] as f32 / 255.0 * inv_da,
                px[2] as f32 / 255.0 * inv_da,
            ];
            let b = f(cb, cs);
            composite_px(px, cs, a_s, b, da);
        }
    }
}

/// Composite one premultiplied group pixel `src` onto `dst` at group opacity.
///
/// An opaque source at full opacity under Normal replaces `dst` outright:
/// the general path below returns exactly `src` for every source value and
/// every backdrop (`opaque_normal_layer_composite_is_a_copy` checks all
/// 256 x 256 x 256 channel, backdrop and backdrop-alpha combinations), so
/// the copy skips its float work for the bulk of a typical group.
#[inline]
fn composite_layer_px(dst: &mut [u8], src: &[u8], opacity: u8, blend: BlendChoice) {
    if src[3] == 255 && opacity == 255 && matches!(blend, BlendChoice::Normal) {
        dst[..4].copy_from_slice(&src[..4]);
        return;
    }
    composite_layer_px_general(dst, src, opacity, blend);
}

/// [`composite_layer_px`] without the opaque-copy shortcut.
#[inline]
fn composite_layer_px_general(dst: &mut [u8], src: &[u8], opacity: u8, blend: BlendChoice) {
    let sa = src[3] as f32 / 255.0;
    if sa <= 0.0 {
        return;
    }
    let a_s = sa * (opacity as f32 / 255.0);
    if a_s <= 0.0 {
        return;
    }
    let inv = 1.0 / sa;
    let cs = [
        src[0] as f32 / 255.0 * inv,
        src[1] as f32 / 255.0 * inv,
        src[2] as f32 / 255.0 * inv,
    ];
    let da = dst[3] as f32 / 255.0;
    let b = match blend {
        BlendChoice::Normal => cs,
        BlendChoice::Separable(f) => {
            let inv_da = if da > 0.0 { 1.0 / da } else { 0.0 };
            [
                f(dst[0] as f32 / 255.0 * inv_da, cs[0]),
                f(dst[1] as f32 / 255.0 * inv_da, cs[1]),
                f(dst[2] as f32 / 255.0 * inv_da, cs[2]),
            ]
        }
        BlendChoice::NonSeparable(f) => {
            let inv_da = if da > 0.0 { 1.0 / da } else { 0.0 };
            let cb = [
                dst[0] as f32 / 255.0 * inv_da,
                dst[1] as f32 / 255.0 * inv_da,
                dst[2] as f32 / 255.0 * inv_da,
            ];
            f(cb, cs)
        }
    };
    composite_px(dst, cs, a_s, b, da);
}

/// Direct row fill for an integer-aligned opaque rectangle — no coverage. The
/// bounds already include any rectangular clip intersection.
fn paint_opaque_rect(cmd: &PreparedCommand, surface: &mut Surface, kernels: &KernelSet) {
    let b = cmd.bounds;
    let ox = surface.origin_x;
    let x0 = b.x as usize;
    let x1 = (b.x as i64 + b.width as i64) as usize;
    let y0 = b.y as usize;
    let y1 = (b.y as i64 + b.height as i64) as usize;
    for y in y0..y1 {
        let row = surface.row_mut(y);
        (kernels.fill_opaque)(&mut row[(x0 - ox) * 4..(x1 - ox) * 4], cmd.premul);
    }
}

/// Clip the coverage row to `[bx0, bx1)`, apply the optional mask, then
/// classify the surviving coverage into spans and dispatch a kernel per span.
#[allow(clippy::too_many_arguments)]
#[inline]
fn dispatch_row(
    surface: &mut Surface,
    kernels: &KernelSet,
    y: usize,
    x0: usize,
    x1: usize,
    cov: &mut [u8],
    bx0: usize,
    bx1: usize,
    mask: Option<&ClipMask>,
    soft: Option<&ClipMask>,
    rgb: [u8; 3],
    alpha: u8,
    opaque: bool,
    premul: [u8; 4],
    blend: BlendChoice,
    shading: Option<&crate::prepared::PreparedShading>,
    stencil: Option<&crate::image::PreparedImage>,
) {
    // Restrict to the clip's column envelope.
    let cxa = x0.max(bx0);
    let cxb = x1.min(bx1.saturating_sub(1));
    if cxa > cxb {
        return;
    }
    // Apply the non-rectangular clip mask (per-pixel multiply).
    if let Some(m) = mask {
        let mw = m.stride();
        let base = (y - m.bounds.y as usize) * mw;
        let mbx = m.bounds.x as usize;
        #[allow(clippy::needless_range_loop)] // cov and mask index at different offsets
        for x in cxa..=cxb {
            let mv = m.data[base + (x - mbx)];
            if mv != 255 {
                cov[x] = mul_div_255(cov[x] as u16, mv as u16) as u8;
            }
        }
    }
    // Apply the active soft mask: pixels outside its bounds are fully masked.
    if let Some(sm) = soft {
        let sw = sm.stride();
        let (sbx, sby) = (sm.bounds.x as usize, sm.bounds.y as usize);
        let sbw = sm.bounds.width as usize;
        let sbh = sm.bounds.height as usize;
        let in_row = y >= sby && y < sby + sbh;
        let base = if in_row { (y - sby) * sw } else { 0 };
        #[allow(clippy::needless_range_loop)] // cov and mask index at different offsets
        for x in cxa..=cxb {
            let sv = if in_row && x >= sbx && x < sbx + sbw {
                sm.data[base + (x - sbx)]
            } else {
                sm.outside
            };
            cov[x] = mul_div_255(cov[x] as u16, sv as u16) as u8;
        }
    }
    // A shading-painted `/ImageMask` contributes coverage rather than color.
    // Sampling the already-prepared image preserves decode polarity,
    // interpolation, minification, rotation and shear.
    if let Some(stencil) = stencil {
        #[allow(clippy::needless_range_loop)]
        for x in cxa..=cxb {
            let alpha = stencil
                .shade(x as f64 + 0.5, y as f64 + 0.5)
                .map_or(0, |sample| sample[3]);
            cov[x] = mul_div_255(cov[x] as u16, alpha as u16) as u8;
        }
    }

    // Local column offset for a possibly-offset (group offscreen) surface.
    let ox = surface.origin_x;

    // Shading paint: evaluate the ramp per pixel and composite under the
    // command's blend (`composite_px_blended`'s Normal arm is the exact
    // integer source-over this path always used).
    if let Some(sh) = shading {
        let row = surface.row_mut(y);
        for x in cxa..=cxb {
            let c = cov[x];
            if c == 0 {
                continue;
            }
            let Some(color) = shade_pixel(sh, x as f64 + 0.5, y as f64 + 0.5) else {
                continue;
            };
            let a = mul_div_255(c as u16, mul_div_255(color[3] as u16, alpha as u16)) as u8;
            if a == 0 {
                continue;
            }
            let px = &mut row[(x - ox) * 4..(x - ox) * 4 + 4];
            composite_px_blended(px, [color[0], color[1], color[2]], a, blend);
        }
        return;
    }

    let row = surface.row_mut(y);

    // Non-Normal blend modes take the general per-pixel compositor.
    match blend {
        BlendChoice::Separable(bfn) => {
            blend_span(
                &mut row[(cxa - ox) * 4..(cxb + 1 - ox) * 4],
                &cov[cxa..=cxb],
                rgb,
                alpha,
                bfn,
            );
            return;
        }
        BlendChoice::NonSeparable(bfn) => {
            blend_span_nonsep(
                &mut row[(cxa - ox) * 4..(cxb + 1 - ox) * 4],
                &cov[cxa..=cxb],
                rgb,
                alpha,
                bfn,
            );
            return;
        }
        BlendChoice::Normal => {}
    }

    classify_spans(row, ox, cov, cxa, cxb, kernels, rgb, alpha, opaque, premul);
}

/// [`dispatch_row`] for the common solid case (Normal blend, no clip mask,
/// soft mask, stencil or shading) with the row handed over as
/// [`CovRun`]s: constant runs are painted directly — a full-coverage run is
/// one `fill_opaque`/`blend_const`, a partial constant one `blend_const` at
/// the scaled alpha — and only the cell runs go through span
/// classification. Byte-identical to materializing the runs and calling
/// `dispatch_row`: every pixel gets the kernel the classifier would choose
/// for its coverage byte (`blend_const` at `a = c·alpha` is `blend_mask`'s
/// arithmetic at constant coverage `c`; checked by
/// `kernels::tests::blend_const_matches_blend_mask_at_constant_coverage`).
#[allow(clippy::too_many_arguments)]
#[inline]
fn dispatch_runs(
    surface: &mut Surface,
    kernels: &KernelSet,
    y: usize,
    x0: usize,
    x1: usize,
    cov: &[u8],
    runs: &[CovRun],
    bx0: usize,
    bx1: usize,
    rgb: [u8; 3],
    alpha: u8,
    opaque: bool,
    premul: [u8; 4],
) {
    let cxa = x0.max(bx0);
    let cxb = x1.min(bx1.saturating_sub(1));
    if cxa > cxb {
        return;
    }
    let ox = surface.origin_x;
    let row = surface.row_mut(y);
    for r in runs {
        let s = (r.x0 as usize).max(cxa);
        let e = (r.x1 as usize).min(cxb + 1);
        if s >= e {
            continue;
        }
        match r.fill {
            None => classify_spans(row, ox, cov, s, e - 1, kernels, rgb, alpha, opaque, premul),
            Some(0) => {}
            Some(255) => {
                let dst = &mut row[(s - ox) * 4..(e - ox) * 4];
                if opaque {
                    (kernels.fill_opaque)(dst, premul);
                } else {
                    (kernels.blend_const)(dst, rgb, alpha);
                }
            }
            Some(c) => {
                let a = if alpha == 255 {
                    c
                } else {
                    mul_div_255(c as u16, alpha as u16) as u8
                };
                (kernels.blend_const)(&mut row[(s - ox) * 4..(e - ox) * 4], rgb, a);
            }
        }
    }
}

/// Classify `cov[cxa..=cxb]` into empty, full and partial spans and dispatch
/// one kernel per span onto `row` (a surface row whose column 0 is device
/// column `ox`).
#[allow(clippy::too_many_arguments)]
#[inline]
fn classify_spans(
    row: &mut [u8],
    ox: usize,
    cov: &[u8],
    cxa: usize,
    cxb: usize,
    kernels: &KernelSet,
    rgb: [u8; 3],
    alpha: u8,
    opaque: bool,
    premul: [u8; 4],
) {
    // Span classification. Runs of empty or full coverage are the common case
    // (the interior of any shape wider than a few pixels), so both are walked
    // eight bytes at a time before the per-byte tail.
    const CHUNK: usize = 8;
    let mut x = cxa;
    while x <= cxb {
        let c = cov[x];
        if c == 0 {
            x += 1;
            while x + CHUNK <= cxb + 1 && cov[x..x + CHUNK] == [0u8; CHUNK] {
                x += CHUNK;
            }
            while x <= cxb && cov[x] == 0 {
                x += 1;
            }
            continue;
        }
        if c == 255 {
            let start = x;
            x += 1;
            while x + CHUNK <= cxb + 1 && cov[x..x + CHUNK] == [255u8; CHUNK] {
                x += CHUNK;
            }
            while x <= cxb && cov[x] == 255 {
                x += 1;
            }
            let dst = &mut row[(start - ox) * 4..(x - ox) * 4];
            if opaque {
                (kernels.fill_opaque)(dst, premul);
            } else {
                (kernels.blend_const)(dst, rgb, alpha);
            }
        } else {
            let start = x;
            while x <= cxb && cov[x] != 0 && cov[x] != 255 {
                x += 1;
            }
            let dst = &mut row[(start - ox) * 4..(x - ox) * 4];
            (kernels.blend_mask)(dst, &cov[start..x], rgb, alpha);
        }
    }
}

#[cfg(test)]
mod rgb8_area_min_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::image::PreparedImage;
    use crate::surface::Surface;
    use pdf_page_ir::{BlendMode, DeviceRect, ImageColorSpace, InterpolationMode, Matrix};
    use pdf_render_api::Background;
    use std::sync::Arc;

    /// Tiny deterministic xorshift, so the fuzz is reproducible.
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

    fn make_image_cs(
        sw: u32,
        sh: u32,
        dev_w: u32,
        dev_h: u32,
        color_space: ImageColorSpace,
        samples: Vec<u8>,
    ) -> PreparedImage {
        let mut img = make_image(sw, sh, dev_w, dev_h, Vec::new());
        img.color_space = color_space;
        img.samples = Arc::from(samples);
        img
    }

    fn make_image(sw: u32, sh: u32, dev_w: u32, dev_h: u32, samples: Vec<u8>) -> PreparedImage {
        // Image unit square maps to device [0,dev_w)x[0,dev_h): u = dx/dev_w,
        // v = dy/dev_h, off-diagonals exactly zero. Footprint = source texels
        // per device pixel, exactly as lowering computes it.
        let inv = Matrix {
            a: 1.0 / dev_w as f64,
            b: 0.0,
            c: 0.0,
            d: 1.0 / dev_h as f64,
            e: 0.0,
            f: 0.0,
        };
        PreparedImage {
            origin: pdf_page_ir::PaintOrigin::PageContent,
            bounds: DeviceRect {
                x: 0,
                y: 0,
                width: dev_w,
                height: dev_h,
            },
            clip: None,
            clip_has_mask: false,
            inv,
            width: sw,
            height: sh,
            bpc: 8,
            color_space: ImageColorSpace::Rgb,
            decode: None,
            samples: Arc::from(samples),
            sample_lut: None,
            smask: None,
            mask: None,
            interpolation: InterpolationMode::Nearest,
            footprint: [sw as f64 / dev_w as f64, sh as f64 / dev_h as f64],
            is_stencil: false,
            stencil_rgb: [0, 0, 0],
            alpha: 255,
            blend: BlendMode::Normal,
        }
    }

    /// The fast path must reproduce, byte for byte, the generic per-pixel
    /// `shade()` area-average + opaque write, over a fuzzed image and several
    /// minifying footprints.
    #[test]
    fn fast_path_matches_generic_shade() {
        let mut rng = Rng(0x1234_5678_9abc_def0);
        // A spread of source sizes and device sizes (all minifying: dev < src).
        let cases = [
            (7u32, 5u32, 3u32, 3u32),
            (16, 16, 5, 7),
            (33, 9, 8, 4),
            (9, 33, 4, 8),
            (64, 48, 20, 15),
            (5, 5, 4, 2), // one axis barely minified, other strongly
        ];
        for &(sw, sh, dev_w, dev_h) in &cases {
            for _trial in 0..8 {
                let samples: Vec<u8> = (0..(sw * sh * 3)).map(|_| rng.byte()).collect();
                let img = make_image(sw, sh, dev_w, dev_h, samples);
                // Fast path renders into `fast`.
                let mut fast =
                    Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                let (painted, taps) = paint_axis_aligned_rgb8_area_min_opaque(
                    &img,
                    &mut fast,
                    0,
                    0,
                    dev_w as usize,
                    dev_h as usize,
                );
                // Reference: generic `shade()` + opaque source-over over a
                // transparent surface (which reduces to a direct copy).
                let mut reference =
                    Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                let mut ref_painted = 0u64;
                let _ = taps; // profiling-only telemetry; not asserted here
                for y in 0..dev_h as usize {
                    let row = reference.row_mut(y);
                    for x in 0..dev_w as usize {
                        let color = img.shade(x as f64 + 0.5, y as f64 + 0.5);
                        if let Some(c) = color {
                            let a = mul_div_255(mul_div_255(c[3] as u16, 255), 255);
                            if a == 0 {
                                continue;
                            }
                            let ia = 255 - a;
                            let px = &mut row[x * 4..x * 4 + 4];
                            px[0] =
                                (mul_div_255(c[0] as u16, a) + mul_div_255(px[0] as u16, ia)) as u8;
                            px[1] =
                                (mul_div_255(c[1] as u16, a) + mul_div_255(px[1] as u16, ia)) as u8;
                            px[2] =
                                (mul_div_255(c[2] as u16, a) + mul_div_255(px[2] as u16, ia)) as u8;
                            px[3] = (a + mul_div_255(px[3] as u16, ia)) as u8;
                            ref_painted += 1;
                        }
                    }
                }
                assert_eq!(
                    painted, ref_painted,
                    "painted count sw={sw} sh={sh} dev={dev_w}x{dev_h}"
                );
                for y in 0..dev_h as usize {
                    let fr = fast.row_mut(y).to_vec();
                    let rr = reference.row_mut(y).to_vec();
                    assert_eq!(
                        fr, rr,
                        "row {y} differs sw={sw} sh={sh} dev={dev_w}x{dev_h}"
                    );
                }
            }
        }
    }

    /// A packed one-bit mask bitmap of `w x h`, byte-aligned rows.
    fn bilevel(w: u32, h: u32, rng: &mut Rng) -> Arc<pdf_page_ir::ImageSMask> {
        let row_bytes = (w as usize).div_ceil(8);
        let samples: Vec<u8> = (0..row_bytes * h as usize).map(|_| rng.byte()).collect();
        Arc::new(pdf_page_ir::ImageSMask {
            width: w,
            height: h,
            bits_per_component: 1,
            decode: None,
            samples: Arc::from(samples),
            codec: None,
            codec_data: None,
            codec_parms: None,
        })
    }

    /// The generic `paint_image` body for the shape the masked area-min path
    /// serves: no clip mask, no soft clip, opaque `alpha`, Normal blend.
    fn generic_masked_reference(img: &PreparedImage, dev_w: u32, dev_h: u32) -> (Surface, u64) {
        let mut reference = Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
        let mut painted = 0u64;
        for y in 0..dev_h as usize {
            let row = reference.row_mut(y);
            for x in 0..dev_w as usize {
                let (dx, dy) = (x as f64 + 0.5, y as f64 + 0.5);
                let edge = img.edge_coverage(dx, dy);
                if edge == Some(0) {
                    continue;
                }
                let color = match edge {
                    None => img.shade(dx, dy),
                    Some(_) => img.shade_clamped(dx, dy),
                };
                let Some(c) = color else { continue };
                let cov = match edge {
                    Some(ec) => mul_div_255(255, ec),
                    None => 255,
                };
                let a = mul_div_255(mul_div_255(c[3] as u16, 255), cov);
                if a == 0 {
                    continue;
                }
                let px = &mut row[x * 4..x * 4 + 4];
                composite_px_blended(px, [c[0], c[1], c[2]], a as u8, BlendChoice::Normal);
                painted += 1;
            }
        }
        (reference, painted)
    }

    /// The MRC path — minified RGB8 cut out by a bilevel layer — must reproduce
    /// the generic per-pixel body byte for byte, in **both** mask encodings and
    /// including the anti-aliased border band. Mask dimensions are varied
    /// independently of the base image's, as MRC producers do.
    #[test]
    fn masked_fast_path_matches_generic_shade() {
        let mut rng = Rng(0x00c0_ffee_0bad_f00d);
        let cases = [
            (7u32, 5u32, 3u32, 3u32),
            (16, 16, 5, 7),
            (33, 9, 8, 4),
            (9, 33, 4, 8),
            (64, 48, 20, 15),
            (5, 5, 4, 2),
        ];
        for &(sw, sh, dev_w, dev_h) in &cases {
            // Mask geometries: same as the base, coarser, and finer.
            for &(mw, mh) in &[(sw, sh), (sw * 2, sh * 2), (sw.div_ceil(2), sh.div_ceil(2))] {
                for trial in 0..6 {
                    let samples: Vec<u8> = (0..(sw * sh * 3)).map(|_| rng.byte()).collect();
                    let mut img = make_image(sw, sh, dev_w, dev_h, samples);
                    let mask = bilevel(mw, mh, &mut rng);
                    // Alternate the two encodings, and exercise `/Decode [1 0]`
                    // polarity on the soft-mask side.
                    let soft = trial % 2 == 0;
                    if soft {
                        let mut sm = (*mask).clone();
                        if trial % 4 == 0 {
                            sm.decode = Some(Arc::from(vec![[1.0f32, 0.0f32]]));
                        }
                        img.smask = Some(Arc::new(sm));
                    } else {
                        img.mask = Some(pdf_page_ir::ImageMask::Stencil(mask));
                    }

                    let Some(kind) = area_min_mask(&img) else {
                        // Only a non-minifying soft mask is turned away, and
                        // then the generic path is already what runs.
                        assert!(soft, "a stencil mask is always eligible");
                        continue;
                    };
                    let mut fast =
                        Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                    let (painted, _taps) = paint_axis_aligned_rgb8_area_min_masked(
                        &img,
                        kind,
                        &mut fast,
                        0,
                        0,
                        dev_w as usize,
                        dev_h as usize,
                    );

                    let (mut reference, ref_painted) = generic_masked_reference(&img, dev_w, dev_h);
                    let what = if soft { "smask" } else { "stencil" };
                    assert_eq!(
                        painted, ref_painted,
                        "{what} painted count sw={sw} sh={sh} mask={mw}x{mh} dev={dev_w}x{dev_h}"
                    );
                    for y in 0..dev_h as usize {
                        assert_eq!(
                            fast.row_mut(y).to_vec(),
                            reference.row_mut(y).to_vec(),
                            "{what} row {y} differs sw={sw} sh={sh} mask={mw}x{mh} dev={dev_w}x{dev_h}"
                        );
                    }
                }
            }
        }
    }

    /// The eight-bit soft-mask variant of the MRC path must reproduce the
    /// generic per-pixel body byte for byte — the f64 box filter, the
    /// `/Decode` remap, the alpha narrowing and the source-over composite —
    /// including the anti-aliased border band, over mask geometries that
    /// differ from the base image's.
    #[test]
    fn masked8_fast_path_matches_generic_shade() {
        let mut rng = Rng(0x8b17_5caf_e000_0001);
        let cases = [
            (7u32, 5u32, 3u32, 3u32),
            (16, 16, 5, 7),
            (33, 9, 8, 4),
            (9, 33, 4, 8),
            (64, 48, 20, 15),
            (5, 5, 4, 2),
        ];
        for &(sw, sh, dev_w, dev_h) in &cases {
            for &(mw, mh) in &[(sw, sh), (sw * 2, sh * 2), (sw.div_ceil(2), sh.div_ceil(2))] {
                for trial in 0..4 {
                    let samples: Vec<u8> = (0..(sw * sh * 3)).map(|_| rng.byte()).collect();
                    let mut img = make_image(sw, sh, dev_w, dev_h, samples);
                    // Mostly opaque with transparent holes, like a cover's
                    // transparency layer, so both the skip and the composite
                    // run; odd trials invert through `/Decode [1 0]`.
                    let mask: Vec<u8> = (0..mw * mh)
                        .map(|_| if rng.byte() < 40 { rng.byte() } else { 255 })
                        .collect();
                    img.smask = Some(Arc::new(pdf_page_ir::ImageSMask {
                        width: mw,
                        height: mh,
                        bits_per_component: 8,
                        decode: (trial % 2 == 1).then(|| Arc::from(vec![[1.0f32, 0.0f32]])),
                        samples: Arc::from(mask),
                        codec: None,
                        codec_data: None,
                        codec_parms: None,
                    }));
                    let Some(kind) = area_min_mask(&img) else {
                        // A non-minifying mask is turned away; the generic
                        // path is then already what runs.
                        continue;
                    };
                    assert!(matches!(kind, AreaMinMask::Soft8(_)));
                    let mut fast =
                        Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                    let (painted, _taps) = paint_axis_aligned_rgb8_area_min_masked(
                        &img,
                        kind,
                        &mut fast,
                        0,
                        0,
                        dev_w as usize,
                        dev_h as usize,
                    );
                    let (mut reference, ref_painted) = generic_masked_reference(&img, dev_w, dev_h);
                    let what = format!(
                        "smask8 sw={sw} sh={sh} mask={mw}x{mh} dev={dev_w}x{dev_h} trial={trial}"
                    );
                    assert_eq!(painted, ref_painted, "{what}: painted count");
                    assert_surfaces_equal(&mut fast, &mut reference, &what);
                }
            }
        }
    }

    /// A blocky mask like a scanned page's foreground layer or a cover's
    /// opacity layer: large clear and set regions, a band of noise, so the
    /// uniform-box shortcuts and the per-tap filters both run, at one and
    /// eight bits, both `/Decode` polarities, mask geometries narrower and
    /// much wider than the base (boxes wider than one 57-bit window), and
    /// placements offset into the surface.
    #[test]
    fn masked_uniform_boxes_match_generic_shade() {
        let mut rng = Rng(0x5eed_b10c_4a11_0001);
        let cases = [
            (7u32, 5u32, 3u32, 3u32),
            (33, 9, 8, 4),
            (64, 48, 20, 15),
            (90, 70, 31, 23),
            (40, 40, 3, 2),
        ];
        for &(sw, sh, dev_w, dev_h) in &cases {
            for &(mw, mh) in &[
                (sw, sh),
                (sw * 3, sh * 3),
                (sw * 8 + 5, sh * 2 + 1),
                (sw.div_ceil(2), sh.div_ceil(2)),
            ] {
                for trial in 0..8 {
                    let samples: Vec<u8> = (0..(sw * sh * 3)).map(|_| rng.byte()).collect();
                    let mut img = make_image(sw, sh, dev_w, dev_h, samples);
                    let one_bit = trial % 2 == 0;
                    // Blocks of 16x8 mask texels: clear, set, or noisy, with
                    // every fourth band of rows clear across the whole width
                    // (a blank line).
                    let block = |x: u32, y: u32, rng: &mut Rng| -> u8 {
                        if (y / 8) % 4 == 3 {
                            return 0;
                        }
                        let kind = ((x / 16) * 7 + (y / 8) * 3 + trial) % 5;
                        match kind {
                            0 | 1 => 0,
                            2 | 3 => 255,
                            _ => rng.byte(),
                        }
                    };
                    let samples: Vec<u8> = if one_bit {
                        let row_bytes = (mw as usize).div_ceil(8);
                        let mut packed = vec![0u8; row_bytes * mh as usize];
                        for y in 0..mh {
                            for x in 0..mw {
                                if block(x, y, &mut rng) >= 128 {
                                    packed[y as usize * row_bytes + x as usize / 8] |=
                                        0x80 >> (x % 8);
                                }
                            }
                        }
                        packed
                    } else {
                        (0..mh)
                            .flat_map(|y| (0..mw).map(move |x| (x, y)))
                            .map(|(x, y)| block(x, y, &mut rng))
                            .collect()
                    };
                    img.smask = Some(Arc::new(pdf_page_ir::ImageSMask {
                        width: mw,
                        height: mh,
                        bits_per_component: if one_bit { 1 } else { 8 },
                        decode: (trial % 4 >= 2).then(|| Arc::from(vec![[1.0f32, 0.0f32]])),
                        samples: Arc::from(samples),
                        codec: None,
                        codec_data: None,
                        codec_parms: None,
                    }));
                    let Some(kind) = area_min_mask(&img) else {
                        continue;
                    };
                    let what = format!(
                        "bits={} sw={sw} sh={sh} mask={mw}x{mh} dev={dev_w}x{dev_h} trial={trial}",
                        if one_bit { 1 } else { 8 }
                    );
                    let mut fast =
                        Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                    let (painted, _taps) = paint_axis_aligned_rgb8_area_min_masked(
                        &img,
                        kind,
                        &mut fast,
                        0,
                        0,
                        dev_w as usize,
                        dev_h as usize,
                    );
                    let (mut reference, ref_painted) = generic_masked_reference(&img, dev_w, dev_h);
                    assert_eq!(painted, ref_painted, "{what}: painted count");
                    assert_surfaces_equal(&mut fast, &mut reference, &what);
                }
            }
        }
    }

    /// The windowed one-bit coverage and the regrouped RGB8 box sum must equal
    /// the per-tap originals for arbitrary tap boxes: every phase of `lo`
    /// within a byte, widths from one texel to past the 57-bit window, rows
    /// at the very end of a truncated buffer.
    #[test]
    fn windowed_mask_and_box_sum_match_per_tap_reference() {
        let mut rng = Rng(0x0dd5_eed5_7a95_0002);
        for _ in 0..4000 {
            let mw = 1 + (rng.next() % 150) as u32;
            let mh = 1 + (rng.next() % 12) as u32;
            let row_bytes = (mw as usize).div_ceil(8);
            let density = rng.byte();
            let mut samples: Vec<u8> = (0..row_bytes * mh as usize)
                .map(|_| {
                    let mut b = 0u8;
                    for bit in 0..8 {
                        if rng.byte() < density {
                            b |= 1 << bit;
                        }
                    }
                    b
                })
                .collect();
            if rng.byte() < 32 {
                // A truncated payload: the filters read missing bytes as zero.
                let keep = (rng.next() as usize) % (samples.len() + 1);
                samples.truncate(keep);
            }
            let sm = pdf_page_ir::ImageSMask {
                width: mw,
                height: mh,
                bits_per_component: 1,
                decode: None,
                samples: Arc::from(samples),
                codec: None,
                codec_data: None,
                codec_parms: None,
            };
            let f =
                |len: u32, rng: &mut Rng| (rng.next() % (len as u64 * 100)) as f64 / 100.0 - 0.5;
            let half = |rng: &mut Rng| 0.5 + (rng.next() % 4000) as f64 / 100.0;
            let Some(tx) = crate::image::axis_box_taps(f(mw, &mut rng), half(&mut rng), mw) else {
                continue;
            };
            let Some(ty) = crate::image::axis_box_taps(f(mh, &mut rng), half(&mut rng), mh) else {
                continue;
            };
            let fast = bilevel_smask_coverage_windowed(&sm, &tx, &ty);
            let reference = crate::image::bilevel_smask_coverage(&sm, &tx, &ty);
            assert_eq!(
                fast.to_bits(),
                reference.to_bits(),
                "coverage {tx:?} {ty:?}"
            );

            // The same boxes over an RGB8 image of the mask's size.
            let rgb: Vec<u8> = (0..mw * mh * 3).map(|_| rng.byte()).collect();
            let stride = mw as usize * 3;
            let weight = tx.total * ty.total;
            let mut acc = [0u64; 3];
            for sr in ty.lo..=ty.hi {
                for col in tx.lo..=tx.hi {
                    let w = ty.weight_at(sr) * tx.weight_at(col);
                    let o = sr as usize * stride + col as usize * 3;
                    for c in 0..3 {
                        acc[c] += w * rgb[o + c] as u64;
                    }
                }
            }
            let expect = acc.map(|a| ((a + weight / 2) / weight).min(255) as u8);
            assert_eq!(
                rgb8_box_average(&rgb, stride, &tx, &ty, weight),
                expect,
                "box {tx:?} {ty:?}"
            );
        }
    }

    /// The row filter must reproduce the per-pixel box average byte for byte
    /// with every kernel: boxes of one to dozens of texels on either axis
    /// (the three-tap and the looped horizontal sums), rows revisited in
    /// both directions and out of order (the cache), boxes taller than the
    /// filter serves (it declines), and the near-white sums where the
    /// rounding division lands on exact multiples.
    #[test]
    fn row_box_filter_matches_per_pixel_box_average() {
        let mut rng = Rng(0x0b0c_5f11_7e40_0004);
        for kernel in available_kernels() {
            for trial in 0..60 {
                let sw = 1 + (rng.next() % 90) as u32;
                let sh = 1 + (rng.next() % 90) as u32;
                let rgb: Vec<u8> = (0..sw * sh * 3)
                    .map(|_| match trial % 3 {
                        0 => rng.byte(),
                        1 => 255 - (rng.byte() & 3),
                        _ => [0, 255][(rng.byte() & 1) as usize],
                    })
                    .collect();
                let stride = sw as usize * 3;
                let half = |rng: &mut Rng| 0.5 + (rng.next() % 1200) as f64 / 100.0;
                let (hx, hy) = (half(&mut rng), half(&mut rng));
                let pos =
                    |len: u32, rng: &mut Rng| (rng.next() % (len as u64 * 64)) as f64 / 64.0 - 0.5;
                let n = 1 + (rng.next() % 40) as usize;
                let mut cols: Vec<crate::image::AxisTaps> = (0..n)
                    .filter_map(|_| crate::image::axis_box_taps(pos(sw, &mut rng), hx, sw))
                    .collect();
                if cols.is_empty() {
                    continue;
                }
                cols.sort_by_key(|t| t.lo);
                let rows: Vec<crate::image::AxisTaps> = (0..30)
                    .filter_map(|_| crate::image::axis_box_taps(pos(sh, &mut rng), hy, sh))
                    .collect();
                let max_row_total = rows.iter().map(|t| t.total).max().unwrap_or(1);
                let mut filter = RowBoxFilter::new(&rgb, stride, &cols, max_row_total).unwrap();
                filter.kernel = kernel;
                let mut dst = vec![0u8; cols.len() * 4];
                for ty in &rows {
                    let served = filter.average_row(ty, &mut dst);
                    assert_eq!(
                        served,
                        ((ty.hi - ty.lo) as usize) < RowBoxFilter::MAX_ROWS,
                        "{kernel:?} {ty:?}"
                    );
                    if !served {
                        continue;
                    }
                    for (x, tx) in cols.iter().enumerate() {
                        let expect = rgb8_box_average(&rgb, stride, tx, ty, tx.total * ty.total);
                        assert_eq!(
                            dst[x * 4..x * 4 + 4],
                            [expect[0], expect[1], expect[2], 255],
                            "{kernel:?} trial={trial} col={tx:?} row={ty:?}"
                        );
                    }
                }
            }
        }
    }

    /// The CMYK fast path (convert source to RGB8 once, then box-average) must
    /// reproduce the generic per-tap `pixel()`→`cmyk_to_rgb`→average, byte for
    /// byte, over a fuzzed CMYK image and several minifying footprints.
    #[test]
    fn cmyk_fast_path_matches_generic_shade() {
        let mut rng = Rng(0x0fee_1dea_dbad_c0de);
        let cases = [
            (7u32, 5u32, 3u32, 3u32),
            (16, 16, 5, 7),
            (33, 9, 8, 4),
            (64, 48, 20, 15),
            (5, 5, 4, 2),
        ];
        for &(sw, sh, dev_w, dev_h) in &cases {
            for _trial in 0..8 {
                let samples: Vec<u8> = (0..(sw * sh * 4)).map(|_| rng.byte()).collect();
                let img = make_image_cs(sw, sh, dev_w, dev_h, ImageColorSpace::Cmyk, samples);
                let mut fast =
                    Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                let (painted, _taps) = paint_axis_aligned_cmyk_area_min_opaque(
                    &img,
                    &mut fast,
                    0,
                    0,
                    dev_w as usize,
                    dev_h as usize,
                )
                .expect("cmyk fast path applies to this small image");
                let mut reference =
                    Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                let mut ref_painted = 0u64;
                for y in 0..dev_h as usize {
                    let row = reference.row_mut(y);
                    for x in 0..dev_w as usize {
                        if let Some(c) = img.shade(x as f64 + 0.5, y as f64 + 0.5) {
                            let a = mul_div_255(mul_div_255(c[3] as u16, 255), 255);
                            if a == 0 {
                                continue;
                            }
                            let ia = 255 - a;
                            let px = &mut row[x * 4..x * 4 + 4];
                            px[0] =
                                (mul_div_255(c[0] as u16, a) + mul_div_255(px[0] as u16, ia)) as u8;
                            px[1] =
                                (mul_div_255(c[1] as u16, a) + mul_div_255(px[1] as u16, ia)) as u8;
                            px[2] =
                                (mul_div_255(c[2] as u16, a) + mul_div_255(px[2] as u16, ia)) as u8;
                            px[3] = (a + mul_div_255(px[3] as u16, ia)) as u8;
                            ref_painted += 1;
                        }
                    }
                }
                assert_eq!(
                    painted, ref_painted,
                    "cmyk painted count sw={sw} sh={sh} dev={dev_w}x{dev_h}"
                );
                for y in 0..dev_h as usize {
                    assert_eq!(
                        fast.row_mut(y).to_vec(),
                        reference.row_mut(y).to_vec(),
                        "cmyk row {y} differs sw={sw} sh={sh} dev={dev_w}x{dev_h}"
                    );
                }
            }
        }
    }

    /// The summed-area-table weighted ones count must agree exactly with the
    /// direct per-box weighted popcount (`binary_box_average`) — both are
    /// integer arithmetic over the same fractional tap weights, so equality
    /// is exact, over fuzzed bilevel images and minifying footprints.
    #[test]
    fn sat_weighted_ones_matches_direct_binary_average() {
        let mut rng = Rng(0xfeed_beef_dead_cafe);
        let cases = [(13u32, 7u32, 5u32, 3u32), (32, 32, 9, 11), (7, 19, 3, 6)];
        for &(sw, sh, dev_w, dev_h) in &cases {
            let row_bytes = (sw as usize).div_ceil(8);
            let samples: Vec<u8> = (0..row_bytes * sh as usize).map(|_| rng.byte()).collect();
            let mut img = make_image(sw, sh, dev_w, dev_h, samples);
            img.bpc = 1;
            img.color_space = ImageColorSpace::Gray;
            img.sample_lut = crate::image::build_sample_lut(1, &ImageColorSpace::Gray, None, false);
            assert!(img.is_binary_box_filterable());
            let (zero, one) = img.binary_box_lut().unwrap();

            let cols: Vec<Option<crate::image::AxisTaps>> = (0..dev_w)
                .map(|x| {
                    let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
                    let fx = u * img.width as f64 - 0.5;
                    img.box_taps_x(fx)
                })
                .collect();
            let rows: Vec<Option<crate::image::AxisTaps>> = (0..dev_h)
                .map(|y| {
                    let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
                    let fy = (1.0 - v) * img.height as f64 - 0.5;
                    img.box_taps_y(fy)
                })
                .collect();
            let sat = BilevelIntegral::build(&img, &cols, &rows).unwrap();

            for ty in rows.iter().flatten() {
                for tx in cols.iter().flatten() {
                    let weight = tx.total * ty.total;
                    let via_sat =
                        crate::image::mix_bilevel(zero, one, sat.weighted_ones(tx, ty), weight);
                    let (direct, _) = img.binary_box_average(tx, ty).unwrap();
                    assert_eq!(via_sat, direct, "sw={sw} sh={sh} tx={tx:?} ty={ty:?}");
                }
            }
        }
    }

    /// Magnified / 1:1 bilinear fast path must match generic `shade()`
    /// byte-for-byte (continuous-tone JPEG/JPX magnification policy).
    #[test]
    fn bilinear_fast_path_matches_generic_shade() {
        let mut rng = Rng(0xc0ff_ee12_3456_789a);
        // All magnifying or 1:1: dest ≥ source on both axes.
        let cases = [
            (2u32, 1u32, 8u32, 8u32),
            (3, 3, 3, 3),
            (4, 4, 16, 16),
            (7, 5, 21, 15),
            (16, 12, 32, 24),
            (5, 9, 11, 19),
            (1, 1, 8, 8),
        ];
        for &(sw, sh, dev_w, dev_h) in &cases {
            for _trial in 0..8 {
                let samples: Vec<u8> = (0..(sw * sh * 3)).map(|_| rng.byte()).collect();
                let mut img = make_image(sw, sh, dev_w, dev_h, samples);
                img.interpolation = InterpolationMode::Bilinear;
                let mut fast =
                    Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                let (painted, taps) = paint_axis_aligned_rgb8_bilinear_opaque(
                    &img,
                    &mut fast,
                    0,
                    0,
                    dev_w as usize,
                    dev_h as usize,
                );
                assert_eq!(taps, painted * 4, "tap accounting");
                let mut reference =
                    Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                let mut ref_painted = 0u64;
                for y in 0..dev_h as usize {
                    let row = reference.row_mut(y);
                    for x in 0..dev_w as usize {
                        if let Some(c) = img.shade(x as f64 + 0.5, y as f64 + 0.5) {
                            let a = mul_div_255(mul_div_255(c[3] as u16, 255), 255);
                            if a == 0 {
                                continue;
                            }
                            // Opaque over transparent → direct write.
                            let px = &mut row[x * 4..x * 4 + 4];
                            px[0] = c[0];
                            px[1] = c[1];
                            px[2] = c[2];
                            px[3] = 255;
                            ref_painted += 1;
                        }
                    }
                }
                assert_eq!(
                    painted, ref_painted,
                    "bilinear painted count sw={sw} sh={sh} dev={dev_w}x{dev_h}"
                );
                for y in 0..dev_h as usize {
                    assert_eq!(
                        fast.row_mut(y).to_vec(),
                        reference.row_mut(y).to_vec(),
                        "bilinear row {y} differs sw={sw} sh={sh} dev={dev_w}x{dev_h}"
                    );
                }
            }
        }
    }

    /// Generic reference: `shade()` + opaque source-over onto a transparent
    /// surface (a direct write for an opaque sample). Returns the painted count.
    fn generic_reference(img: &PreparedImage, dev_w: u32, dev_h: u32) -> (Surface, u64) {
        let mut reference = Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
        let mut painted = 0u64;
        for y in 0..dev_h as usize {
            let row = reference.row_mut(y);
            for x in 0..dev_w as usize {
                if let Some(c) = img.shade(x as f64 + 0.5, y as f64 + 0.5) {
                    let a = mul_div_255(mul_div_255(c[3] as u16, 255), 255);
                    if a == 0 {
                        continue;
                    }
                    let ia = 255 - a;
                    let px = &mut row[x * 4..x * 4 + 4];
                    px[0] = (mul_div_255(c[0] as u16, a) + mul_div_255(px[0] as u16, ia)) as u8;
                    px[1] = (mul_div_255(c[1] as u16, a) + mul_div_255(px[1] as u16, ia)) as u8;
                    px[2] = (mul_div_255(c[2] as u16, a) + mul_div_255(px[2] as u16, ia)) as u8;
                    px[3] = (a + mul_div_255(px[3] as u16, ia)) as u8;
                    painted += 1;
                }
            }
        }
        (reference, painted)
    }

    fn assert_surfaces_equal(a: &mut Surface, b: &mut Surface, what: &str) {
        for y in 0..a.height {
            assert_eq!(
                a.row_mut(y).to_vec(),
                b.row_mut(y).to_vec(),
                "{what}: row {y} differs"
            );
        }
    }

    /// The kernels the image painters can run on this host.
    fn available_kernels() -> Vec<ImageKernel> {
        let mut kernels = vec![ImageKernel::Scalar];
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            kernels.push(ImageKernel::Avx2);
        }
        kernels
    }

    /// A packed Indexed source: `bpc`-bit MSB-first samples, byte-aligned rows.
    fn packed_indexed_samples(sw: u32, sh: u32, bpc: u8, rng: &mut Rng) -> Vec<u8> {
        let row_bytes = (sw as usize * bpc as usize).div_ceil(8);
        (0..row_bytes * sh as usize).map(|_| rng.byte()).collect()
    }

    /// The Indexed fast path (resolve the palette once, expand to RGB8, then
    /// box-average) must reproduce the generic per-tap lookup + average, byte
    /// for byte: at 8 bpc the generic path resolves each tap through
    /// `pixel()`, at 2/4 bpc through the prepared `sample_lut`; with and
    /// without a `/Decode` remap; over Gray, RGB and CMYK palettes.
    #[test]
    fn indexed_fast_path_matches_generic_shade() {
        let mut rng = Rng(0x1d1d_1d1d_5eed_0001);
        let cases = [
            (7u32, 5u32, 3u32, 3u32),
            (16, 16, 5, 7),
            (33, 9, 8, 4),
            (64, 48, 20, 15),
            (5, 5, 4, 2),
        ];
        let bases: [ImageColorSpace; 3] = [
            ImageColorSpace::Gray,
            ImageColorSpace::Rgb,
            ImageColorSpace::Cmyk,
        ];
        for &bpc in &[2u8, 4, 8] {
            for base in &bases {
                for &(sw, sh, dev_w, dev_h) in &cases {
                    for &with_decode in &[false, true] {
                        // A palette shorter than `2^bpc` exercises the `hival` clamp.
                        let hival = ((1u32 << bpc) - 1).min(200 + (rng.byte() as u32 % 56));
                        let lookup: Vec<u8> = (0..(hival as usize + 1) * base.components())
                            .map(|_| rng.byte())
                            .collect();
                        let color_space = ImageColorSpace::Indexed {
                            base: Box::new(base.clone()),
                            hival,
                            lookup: Arc::from(lookup),
                        };
                        let samples = packed_indexed_samples(sw, sh, bpc, &mut rng);
                        let mut img =
                            make_image_cs(sw, sh, dev_w, dev_h, color_space.clone(), samples);
                        img.bpc = bpc;
                        img.decode = with_decode.then(|| {
                            // `/Decode [hi lo]`: inverts the index range, in index units.
                            Arc::from(vec![[hival as f32, 0.0f32]])
                        });
                        // What prepared.rs attaches: a LUT for 1/2/4 bpc, none at 8.
                        img.sample_lut = crate::image::build_sample_lut(
                            bpc,
                            &color_space,
                            img.decode.as_deref(),
                            false,
                        );
                        assert_eq!(img.sample_lut.is_some(), bpc <= 4);
                        let promoted = img
                            .indexed_promoted_to_rgb8()
                            .expect("indexed promotion applies to this small image");
                        let what = format!(
                            "indexed bpc={bpc} base={base:?} decode={with_decode} sw={sw} sh={sh} dev={dev_w}x{dev_h}"
                        );
                        // Unmasked: the promoted image takes the RGB8 area path.
                        let mut fast =
                            Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                        let (painted, _taps) = paint_axis_aligned_rgb8_area_min_opaque(
                            &promoted,
                            &mut fast,
                            0,
                            0,
                            dev_w as usize,
                            dev_h as usize,
                        );
                        let (mut reference, ref_painted) = generic_reference(&img, dev_w, dev_h);
                        assert_eq!(painted, ref_painted, "{what}: painted count");
                        assert_surfaces_equal(&mut fast, &mut reference, &what);
                        // Cut out by a bilevel soft mask: the promoted image
                        // takes the masked MRC path.
                        let mut masked = img.clone();
                        masked.smask = Some(bilevel(sw, sh, &mut rng));
                        let promoted_masked = masked.indexed_promoted_to_rgb8().unwrap();
                        let kind = area_min_mask(&promoted_masked).expect("bilevel mask");
                        let mut fast =
                            Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                        let (painted, _taps) = paint_axis_aligned_rgb8_area_min_masked(
                            &promoted_masked,
                            kind,
                            &mut fast,
                            0,
                            0,
                            dev_w as usize,
                            dev_h as usize,
                        );
                        let (mut reference, ref_painted) =
                            generic_masked_reference(&masked, dev_w, dev_h);
                        assert_eq!(painted, ref_painted, "{what} smask: painted count");
                        assert_surfaces_equal(&mut fast, &mut reference, &format!("{what} smask"));
                        // Under an 8-bit soft mask no fast path applies; the
                        // generic sampler itself must see the same image.
                        let mut soft8 = img.clone();
                        soft8.smask = Some(Arc::new(pdf_page_ir::ImageSMask {
                            width: sw,
                            height: sh,
                            bits_per_component: 8,
                            decode: None,
                            samples: Arc::from(
                                (0..sw * sh).map(|_| rng.byte()).collect::<Vec<u8>>(),
                            ),
                            codec: None,
                            codec_data: None,
                            codec_parms: None,
                        }));
                        let promoted_soft8 = soft8.indexed_promoted_to_rgb8().unwrap();
                        let (mut a, pa) = generic_masked_reference(&promoted_soft8, dev_w, dev_h);
                        let (mut b, pb) = generic_masked_reference(&soft8, dev_w, dev_h);
                        assert_eq!(pa, pb, "{what} smask8: painted count");
                        assert_surfaces_equal(&mut a, &mut b, &format!("{what} smask8"));
                    }
                }
            }
        }
    }

    /// A per-pixel-transform image (non-sRGB ICC RGB, Lab, one- and
    /// two-colorant tint tables), with and without `/Decode`, resolved to
    /// RGB8 once must draw byte-identically to the generic per-tap
    /// transform: the promoted image through the generic sampler and through
    /// the RGB8 area-min fast path, each against the original image through
    /// the generic sampler.
    #[test]
    fn generic_promotion_matches_generic_shade() {
        let mut rng = Rng(0x1cc0_1cc0_5eed_0003);
        let cases = [
            (7u32, 5u32, 3u32, 3u32),
            (16, 16, 5, 7),
            (33, 9, 8, 4),
            (40, 30, 13, 11),
        ];
        for variant in 0..4 {
            for &(sw, sh, dev_w, dev_h) in &cases {
                for &with_decode in &[false, true] {
                    let (color_space, ncomp) = match variant {
                        0 => {
                            let trc: Vec<f32> =
                                (0..768).map(|_| rng.byte() as f32 / 255.0).collect();
                            let mut matrix = [0f32; 9];
                            for m in matrix.iter_mut() {
                                *m = rng.byte() as f32 / 255.0;
                            }
                            (
                                ImageColorSpace::IccRgb {
                                    trc: Arc::from(trc),
                                    matrix,
                                },
                                3,
                            )
                        }
                        1 => (
                            ImageColorSpace::Lab {
                                white_point: [0.9505, 1.0, 1.089],
                                range: [-100.0, 100.0, -100.0, 100.0],
                            },
                            3,
                        ),
                        2 => {
                            let rgb: Vec<u8> = (0..768).map(|_| rng.byte()).collect();
                            (
                                ImageColorSpace::TintLut {
                                    rgb: Arc::from(rgb),
                                },
                                1,
                            )
                        }
                        _ => {
                            let rgb: Vec<u8> = (0..256 * 256 * 3).map(|_| rng.byte()).collect();
                            (
                                ImageColorSpace::TintLut2 {
                                    rgb: Arc::from(rgb),
                                },
                                2,
                            )
                        }
                    };
                    let samples: Vec<u8> = (0..(sw * sh) as usize * ncomp)
                        .map(|_| rng.byte())
                        .collect();
                    let mut img = make_image_cs(sw, sh, dev_w, dev_h, color_space, samples);
                    img.decode = with_decode.then(|| {
                        Arc::from((0..ncomp).map(|_| [1.0f32, 0.0f32]).collect::<Vec<_>>())
                    });
                    assert!(img.generic_promotion_eligible());
                    let promoted = img.generic_promoted_to_rgb8().expect("eligible");
                    let what = format!(
                        "variant {variant} decode={with_decode} sw={sw} sh={sh} dev={dev_w}x{dev_h}"
                    );
                    let (mut reference, ref_painted) = generic_reference(&img, dev_w, dev_h);
                    let (mut via_promoted, promoted_painted) =
                        generic_reference(&promoted, dev_w, dev_h);
                    assert_eq!(promoted_painted, ref_painted, "{what}: painted (generic)");
                    assert_surfaces_equal(
                        &mut via_promoted,
                        &mut reference,
                        &format!("{what} (generic over promoted)"),
                    );
                    let mut fast =
                        Surface::new(dev_w as usize, dev_h as usize, Background::Transparent);
                    let (painted, _taps) = paint_axis_aligned_rgb8_area_min_opaque(
                        &promoted,
                        &mut fast,
                        0,
                        0,
                        dev_w as usize,
                        dev_h as usize,
                    );
                    assert_eq!(painted, ref_painted, "{what}: painted (fast)");
                    assert_surfaces_equal(
                        &mut fast,
                        &mut reference,
                        &format!("{what} (fast over promoted)"),
                    );
                }
            }
        }
    }

    /// An image placed at `(tx, ty)` device pixels, magnifying/1:1 by
    /// `dev_w / sw` on X and `dev_h / sh` on Y, inside a `surf_w x surf_h`
    /// surface. Off-diagonals exactly zero, as lowering computes them.
    fn make_image_placed(
        sw: u32,
        sh: u32,
        dev_w: u32,
        dev_h: u32,
        tx: f64,
        ty: f64,
        samples: Vec<u8>,
    ) -> PreparedImage {
        let mut img = make_image(sw, sh, dev_w, dev_h, samples);
        img.inv = Matrix {
            a: 1.0 / dev_w as f64,
            b: 0.0,
            c: 0.0,
            d: 1.0 / dev_h as f64,
            e: -tx / dev_w as f64,
            f: -ty / dev_h as f64,
        };
        img
    }

    /// Magnified / 1:1 bilinear draws — wide enough for the eight-lane
    /// kernels plus a ragged tail, placed on integer translations (the widened
    /// 1:1 blit), on fractional translations (two-tap lerps with tiny weights)
    /// and partly off the surface — must match generic `shade()` byte for
    /// byte on every kernel this host has, and every kernel must agree.
    #[test]
    fn bilinear_kernels_match_generic_shade_and_each_other() {
        let mut rng = Rng(0xb111_4ea8_0000_0042);
        // (sw, sh, dev_w, dev_h, tx, ty, surf_w, surf_h)
        let cases: [(u32, u32, u32, u32, f64, f64, u32, u32); 9] = [
            (64, 8, 64, 8, 0.0, 0.0, 64, 8),     // 1:1, exact blit
            (64, 8, 64, 8, 16.0, 4.0, 96, 16),   // 1:1, integer offset (exact)
            (37, 5, 37, 5, 3.0, 2.0, 48, 9),     // 1:1, non-power-of-two width
            (64, 8, 64, 8, 0.5, 0.25, 80, 12),   // 1:1, fractional offset
            (40, 7, 97, 13, 2.0, 1.0, 101, 15),  // non-integer magnification
            (33, 9, 66, 18, 0.0, 0.0, 66, 18),   // 2x
            (33, 9, 66, 18, -7.0, -3.0, 50, 12), // 2x, partly off the surface
            (9, 3, 9, 3, 0.0, 0.0, 9, 3),        // 1:1, narrower than one group
            (1, 1, 8, 8, 0.0, 0.0, 8, 8),        // degenerate source
        ];
        for &(sw, sh, dev_w, dev_h, tx, ty, surf_w, surf_h) in &cases {
            for _trial in 0..4 {
                let samples: Vec<u8> = (0..(sw * sh * 3)).map(|_| rng.byte()).collect();
                let mut img = make_image_placed(sw, sh, dev_w, dev_h, tx, ty, samples);
                img.interpolation = InterpolationMode::Bilinear;
                let what = format!("bilinear sw={sw} sh={sh} dev={dev_w}x{dev_h} at ({tx},{ty})");
                let (mut reference, ref_painted) = generic_reference(&img, surf_w, surf_h);
                for kernel in available_kernels() {
                    let Some(columns) = bilinear_columns(&img, 0, surf_w as usize) else {
                        panic!("{what}: no columns");
                    };
                    let Some(rows) = bilinear_rows(&img, 0, surf_h as usize) else {
                        panic!("{what}: no rows");
                    };
                    let mut fast =
                        Surface::new(surf_w as usize, surf_h as usize, Background::Transparent);
                    let painted = paint_bilinear_rows(kernel, &img, &mut fast, &columns, &rows);
                    assert_eq!(painted, ref_painted, "{what} {kernel:?}: painted count");
                    assert_surfaces_equal(&mut fast, &mut reference, &format!("{what} {kernel:?}"));
                }
            }
        }
    }

    /// The nearest fast path — widened runs at 1:1, texel-by-texel copies when
    /// magnified — must match generic `shade()` byte for byte.
    #[test]
    fn nearest_fast_path_matches_generic_shade() {
        let mut rng = Rng(0x4ea7_e570_0000_0007);
        let cases: [(u32, u32, u32, u32, f64, f64, u32, u32); 5] = [
            (64, 8, 64, 8, 0.0, 0.0, 64, 8),
            (37, 5, 37, 5, 3.0, 2.0, 48, 9),
            (64, 8, 64, 8, 0.5, 0.25, 80, 12),
            (33, 9, 66, 18, -7.0, -3.0, 50, 12),
            (40, 7, 97, 13, 2.0, 1.0, 101, 15),
        ];
        for &(sw, sh, dev_w, dev_h, tx, ty, surf_w, surf_h) in &cases {
            for _trial in 0..4 {
                let samples: Vec<u8> = (0..(sw * sh * 3)).map(|_| rng.byte()).collect();
                let img = make_image_placed(sw, sh, dev_w, dev_h, tx, ty, samples);
                let what = format!("nearest sw={sw} sh={sh} dev={dev_w}x{dev_h} at ({tx},{ty})");
                let (mut reference, ref_painted) = generic_reference(&img, surf_w, surf_h);
                let mut fast =
                    Surface::new(surf_w as usize, surf_h as usize, Background::Transparent);
                let (painted, _) = paint_axis_aligned_rgb8_nearest_opaque(
                    &img,
                    &mut fast,
                    0,
                    0,
                    surf_w as usize,
                    surf_h as usize,
                );
                assert_eq!(painted, ref_painted, "{what}: painted count");
                assert_surfaces_equal(&mut fast, &mut reference, &what);
            }
        }
    }

    /// The lookup-table nearest path — a packed 1-, 2- or 4-bit
    /// single-component source read through its prepared table — must match
    /// generic `shade()` byte for byte at 1:1, magnified, fractionally placed
    /// and partly off the surface, with and without an inverting `/Decode`.
    #[test]
    fn lut_nearest_fast_path_matches_generic_shade() {
        let mut rng = Rng(0x1b17_4ea7_0000_0011);
        let cases: [(u32, u32, u32, u32, f64, f64, u32, u32); 6] = [
            (64, 8, 64, 8, 0.0, 0.0, 64, 8),
            (37, 5, 37, 5, 3.0, 2.0, 48, 9),
            (64, 8, 64, 8, 0.5, 0.25, 80, 12),
            (33, 9, 66, 18, -7.0, -3.0, 50, 12),
            (40, 7, 97, 13, 2.0, 1.0, 101, 15),
            (21, 4, 63, 12, 0.0, 0.0, 63, 12),
        ];
        let inverting = [[1.0f32, 0.0]];
        for &(sw, sh, dev_w, dev_h, tx, ty, surf_w, surf_h) in &cases {
            for bpc in [1u8, 2, 4] {
                for invert in [false, true] {
                    let row_bytes = (sw as usize * bpc as usize).div_ceil(8);
                    let samples: Vec<u8> =
                        (0..row_bytes * sh as usize).map(|_| rng.byte()).collect();
                    let mut img = make_image_placed(sw, sh, dev_w, dev_h, tx, ty, samples);
                    img.bpc = bpc;
                    img.color_space = ImageColorSpace::Gray;
                    img.sample_lut = crate::image::build_sample_lut(
                        bpc,
                        &ImageColorSpace::Gray,
                        invert.then_some(&inverting[..]),
                        false,
                    );
                    let what = format!(
                        "lut nearest bpc={bpc} invert={invert} sw={sw} sh={sh} dev={dev_w}x{dev_h} at ({tx},{ty})"
                    );
                    let lut = img.lut_nearest_opaque().expect("complete opaque table");
                    let (mut reference, ref_painted) = generic_reference(&img, surf_w, surf_h);
                    let mut fast =
                        Surface::new(surf_w as usize, surf_h as usize, Background::Transparent);
                    let (painted, _) = paint_axis_aligned_lut_nearest_opaque(
                        &img,
                        lut,
                        &mut fast,
                        0,
                        0,
                        surf_w as usize,
                        surf_h as usize,
                    );
                    assert_eq!(painted, ref_painted, "{what}: painted count");
                    assert_surfaces_equal(&mut fast, &mut reference, &what);
                }
            }
        }
    }

    /// The vector widen must equal the scalar one over every run length and
    /// offset, including runs ending at the very end of the buffer where the
    /// vector loads cannot run and the scalar tail takes over.
    #[test]
    fn widen_kernels_match_scalar_bit_exact() {
        let mut rng = Rng(0x71de_0000_0000_0001);
        let src: Vec<u8> = (0..4096).map(|_| rng.byte()).collect();
        for kernel in available_kernels() {
            for &n in &[0usize, 1, 3, 7, 8, 9, 15, 16, 17, 31, 64, 100] {
                for &off in &[
                    0usize,
                    1,
                    5,
                    4096 - n * 3 - 40,
                    4096 - n * 3 - 1,
                    4096 - n * 3,
                ] {
                    if off + n * 3 > src.len() {
                        continue;
                    }
                    let mut expected = vec![0xAAu8; n * 4];
                    widen_rgb8_to_rgba8_scalar(&src[off..off + n * 3], &mut expected);
                    let mut got = vec![0xAAu8; n * 4];
                    widen_rgb8_to_rgba8_opaque(kernel, &src, off, &mut got);
                    assert_eq!(got, expected, "{kernel:?} n={n} off={off}");
                }
            }
        }
    }

    /// Known truncation sample from the integration bilinear fixture:
    /// 2×1 red/blue magnified to 8×8, dest x=2 → [223, 0, 31, 255].
    #[test]
    fn bilinear_fast_path_truncates_like_reference_stretchers() {
        let mut img = make_image(2, 1, 8, 8, vec![255, 0, 0, 0, 0, 255]);
        img.interpolation = InterpolationMode::Bilinear;
        let mut surface = Surface::new(8, 8, Background::Transparent);
        let _ = paint_axis_aligned_rgb8_bilinear_opaque(&img, &mut surface, 0, 0, 8, 8);
        let row = surface.row_mut(4);
        let px = &row[2 * 4..2 * 4 + 4];
        assert_eq!(px, [223, 0, 31, 255]);
    }

    /// The shortcut in `composite_layer_px` must be what the general path
    /// computes: an opaque source at full opacity under Normal yields the
    /// source byte for every channel value, backdrop value and backdrop alpha
    /// (the general path's result depends on nothing else).
    #[test]
    fn opaque_normal_layer_composite_is_a_copy() {
        for s in 0..=255u8 {
            for da in 0..=255u8 {
                for d in 0..=255u8 {
                    let src = [s, s, s, 255];
                    let mut dst = [d, d, d, da];
                    composite_layer_px_general(&mut dst, &src, 255, BlendChoice::Normal);
                    assert_eq!(dst, src, "source {s}, backdrop {d}, backdrop alpha {da}");
                }
            }
        }
    }

    #[test]
    fn rect_contains_is_inclusive_and_treats_empty_as_contained() {
        let r = |x, y, width, height| DeviceRect {
            x,
            y,
            width,
            height,
        };
        let outer = r(2, 3, 10, 5);
        assert!(rect_contains(outer, outer));
        assert!(rect_contains(outer, r(5, 4, 7, 4)));
        assert!(
            !rect_contains(outer, r(5, 4, 8, 4)),
            "one column past the right edge"
        );
        assert!(!rect_contains(outer, r(1, 4, 2, 2)), "left of it");
        assert!(!rect_contains(outer, r(3, 2, 2, 2)), "above it");
        assert!(rect_contains(outer, r(100, 100, 0, 3)), "empty");
    }

    /// A one-bit `/ImageMask` stencil of `sw x sh` random texels, painted in
    /// a fixed colour, re-placed at device `(ox, oy)` with size `w x h`.
    /// `inverted` gives it `/Decode [1 0]` (a set bit paints).
    #[allow(clippy::too_many_arguments)]
    fn stencil_image(
        sw: u32,
        sh: u32,
        ox: f64,
        oy: f64,
        w: f64,
        h: f64,
        inverted: bool,
        rng: &mut Rng,
    ) -> PreparedImage {
        let row_bytes = (sw as usize).div_ceil(8);
        let samples: Vec<u8> = (0..row_bytes * sh as usize).map(|_| rng.byte()).collect();
        let mut img = place(make_image(sw, sh, 1, 1, samples), ox, oy, w, h);
        img.is_stencil = true;
        img.bpc = 1;
        img.color_space = ImageColorSpace::Gray;
        img.stencil_rgb = [200, 40, 90];
        img.decode = inverted.then(|| Arc::from([[1.0f32, 0.0]]));
        img
    }

    /// A clip mask over most of a `side x side` surface with random coverage:
    /// zero, full and partial bytes.
    fn random_clip(side: u32, rng: &mut Rng) -> ClipMask {
        let bounds = DeviceRect {
            x: 1,
            y: 0,
            width: side - 2,
            height: side - 1,
        };
        let data = (0..bounds.width * bounds.height)
            .map(|_| match rng.byte() % 4 {
                0 => 0,
                1 => 255,
                _ => rng.byte(),
            })
            .collect();
        ClipMask {
            bounds,
            data,
            outside: 0,
            all_opaque: false,
        }
    }

    /// A `side x side` surface with an arbitrary backdrop, so compositing at
    /// partial alpha is exercised.
    fn backdrop(side: u32, seed: u64) -> Surface {
        let mut rng = Rng(seed);
        let mut surface = Surface::new(side as usize, side as usize, Background::Transparent);
        for y in 0..side as usize {
            for b in surface.row_mut(y) {
                *b = rng.byte();
            }
        }
        surface
    }

    /// The `/ImageMask` stencil fast path (bilevel box filter when minified,
    /// transparent-skip nearest otherwise, on the interior; generic body on
    /// the border band) must equal the generic per-pixel body byte for byte:
    /// both `/Decode` polarities, with and without a clip mask, opaque and
    /// partial fill alpha, over a backdrop, at fractional placements.
    #[test]
    fn stencil_fast_path_matches_the_generic_body() {
        let mut rng = Rng(0x5a7e_c11a_0dd5_beef);
        // (source w, h, device x, y, w, h): minified, near 1:1 and magnified.
        let placements: [(u32, u32, f64, f64, f64, f64); 7] = [
            (37, 23, 1.3, 2.6, 9.4, 6.2),
            (64, 48, 2.5, 1.125, 20.3, 14.7),
            (9, 33, 3.7, 0.2, 4.6, 11.9),
            (200, 150, 0.4, 0.7, 30.2, 22.9),
            (16, 16, 0.25, 0.75, 15.5, 15.5),
            (5, 7, 0.6, 1.4, 17.8, 21.3),
            (13, 11, 2.0, 3.0, 26.0, 22.0),
        ];
        let mut minified = 0;
        let mut nearest = 0;
        for &(sw, sh, ox, oy, w, h) in &placements {
            let side = (ox + w).max(oy + h).ceil() as u32 + 2;
            for inverted in [false, true] {
                for clipped in [false, true] {
                    for alpha in [255u8, 170] {
                        let mut img = stencil_image(sw, sh, ox, oy, w, h, inverted, &mut rng);
                        img.alpha = alpha;
                        let clip = clipped.then(|| random_clip(side, &mut rng));
                        let lut = stencil_fast_lut(&img).expect("an axis-aligned stencil");
                        assert!(
                            interior_span(img.inv.a, img.inv.e, 0, side as usize).is_some()
                                && interior_span(img.inv.d, img.inv.f, 0, side as usize)
                                    .is_some(),
                            "the placement must have an interior for the fast path"
                        );
                        let b = img.bounds;
                        let (x0, y0) = (b.x as usize, b.y as usize);
                        let (x1, y1) = (x0 + b.width as usize, y0 + b.height as usize);
                        let seed = rng.next();
                        let mut got = backdrop(side, seed);
                        let fast = paint_axis_aligned_stencil(
                            &img,
                            lut,
                            &mut got,
                            clip.as_ref(),
                            BlendChoice::Normal,
                            x0,
                            y0,
                            x1,
                            y1,
                        );
                        let mut want = backdrop(side, seed);
                        let generic = paint_image_generic(
                            &img,
                            &mut want,
                            clip.as_ref(),
                            None,
                            BlendChoice::Normal,
                            x0,
                            y0,
                            x1,
                            y1,
                            None,
                        );
                        let what = format!(
                            "stencil {sw}x{sh} at ({ox}, {oy}) size {w}x{h} inverted {inverted} \
                             clipped {clipped} alpha {alpha}"
                        );
                        assert_surfaces_equal(&mut got, &mut want, &what);
                        assert_eq!(fast.0, generic.0, "{what}: covered pixels");
                        if img.footprint[0] > 1.0 || img.footprint[1] > 1.0 {
                            minified += 1;
                        } else {
                            nearest += 1;
                        }
                    }
                }
            }
        }
        assert!(minified > 0 && nearest > 0, "{minified} minified, {nearest} nearest");
    }

    /// What keeps a stencil on the generic body: a rotated or sheared
    /// placement, a magnified Bilinear draw, and sample data shorter than its
    /// packed rows (the fast paths index rows directly; `pixel()` reads the
    /// missing bytes as zero, which paints). Each still renders, through the
    /// dispatcher, exactly as the generic body does.
    #[test]
    fn stencil_fast_path_eligibility_and_generic_fallbacks() {
        let mut rng = Rng(0x0bad_5eed_1234_4321);
        let side = 24u32;

        // Eligible: minified, magnified Nearest, minified Bilinear, inverted.
        let img = stencil_image(40, 30, 1.5, 2.25, 17.3, 12.8, false, &mut rng);
        assert!(stencil_fast_lut(&img).is_some());
        let mut img = stencil_image(5, 4, 1.5, 2.25, 17.3, 12.8, true, &mut rng);
        assert!(stencil_fast_lut(&img).is_some());
        img.interpolation = InterpolationMode::Bilinear;
        assert!(stencil_fast_lut(&img).is_none(), "magnified Bilinear stays generic");
        let mut img = stencil_image(40, 30, 1.5, 2.25, 17.3, 12.8, false, &mut rng);
        img.interpolation = InterpolationMode::Bilinear;
        assert!(stencil_fast_lut(&img).is_some(), "minification ignores the hint");

        // Polarity: the painting entry is opaque, the masking one clear.
        let plain = stencil_image(8, 8, 0.0, 0.0, 4.0, 4.0, false, &mut rng);
        let (zero, one) = plain.stencil_bilevel_lut().unwrap();
        assert_eq!((zero, one), ([200, 40, 90, 255], [200, 40, 90, 0]));
        let inverted = stencil_image(8, 8, 0.0, 0.0, 4.0, 4.0, true, &mut rng);
        let (zero, one) = inverted.stencil_bilevel_lut().unwrap();
        assert_eq!((zero, one), ([200, 40, 90, 0], [200, 40, 90, 255]));

        let mut cases: Vec<(&str, PreparedImage)> = Vec::new();
        // Rotated a few degrees about the draw's centre.
        let mut rotated = stencil_image(40, 30, 3.0, 3.0, 17.0, 13.0, false, &mut rng);
        let (s, c) = (0.1f64.sin(), 0.1f64.cos());
        let (cx, cy) = (11.5, 9.5);
        // Device -> unit square: rotate back about the centre, then scale.
        rotated.inv = Matrix {
            a: c / 17.0,
            b: -s / 13.0,
            c: s / 17.0,
            d: c / 13.0,
            e: (-c * cx - s * cy) / 17.0 + 0.5,
            f: (s * cx - c * cy) / 13.0 + 0.5,
        };
        rotated.bounds = DeviceRect {
            x: 0,
            y: 0,
            width: side,
            height: side,
        };
        assert!(stencil_fast_lut(&rotated).is_none(), "rotated stays generic");
        cases.push(("rotated", rotated));
        // Truncated: the last two packed rows are missing.
        let mut short = stencil_image(40, 30, 1.5, 2.25, 17.3, 12.8, false, &mut rng);
        let full = short.samples.len();
        short.samples = Arc::from(&short.samples[..full - 2 * 5]);
        assert!(stencil_fast_lut(&short).is_none(), "a short buffer stays generic");
        cases.push(("short", short));
        let mut short_magnified = stencil_image(6, 5, 1.5, 2.25, 17.3, 12.8, true, &mut rng);
        short_magnified.samples = Arc::from(&short_magnified.samples[..3]);
        assert!(stencil_fast_lut(&short_magnified).is_none());
        cases.push(("short magnified", short_magnified));

        for (what, img) in &cases {
            let mut got = dispatched(img, side);
            let (mut want, _) = generic_masked_reference(img, side, side);
            assert_surfaces_equal(&mut got, &mut want, what);
        }
    }

    /// A one-bit gray image of `sw x sh` packed bits from `bits`, placed at
    /// device `(ox, oy)` with size `w x h` (a negative size mirrors that axis).
    #[allow(clippy::too_many_arguments)]
    fn bilevel_image(
        sw: u32,
        sh: u32,
        bits: Vec<u8>,
        ox: f64,
        oy: f64,
        w: f64,
        h: f64,
    ) -> PreparedImage {
        let mut img = make_image(sw, sh, 1, 1, bits);
        img.bpc = 1;
        img.color_space = ImageColorSpace::Gray;
        img.sample_lut = crate::image::build_sample_lut(1, &ImageColorSpace::Gray, None, false);
        // Device -> unit square, either orientation.
        img.inv = Matrix {
            a: 1.0 / w,
            b: 0.0,
            c: 0.0,
            d: 1.0 / h,
            e: -ox / w,
            f: -oy / h,
        };
        let (lx, hx) = (ox.min(ox + w), ox.max(ox + w));
        let (ly, hy) = (oy.min(oy + h), oy.max(oy + h));
        img.bounds = DeviceRect {
            x: lx.floor() as i32,
            y: ly.floor() as i32,
            width: (hx.ceil() - lx.floor()) as u32,
            height: (hy.ceil() - ly.floor()) as u32,
        };
        img.footprint = [sw as f64 / w.abs(), sh as f64 / h.abs()];
        img
    }

    /// The per-column and per-row taps `paint_axis_aligned_binary_box` builds.
    fn binary_taps(
        img: &PreparedImage,
    ) -> (
        Vec<Option<crate::image::AxisTaps>>,
        Vec<Option<crate::image::AxisTaps>>,
    ) {
        let b = img.bounds;
        let cols = (b.x as usize..b.x as usize + b.width as usize)
            .map(|x| {
                let u = img.inv.a * (x as f64 + 0.5) + img.inv.e;
                (0.0..1.0)
                    .contains(&u)
                    .then(|| img.box_taps_x(u * img.width as f64 - 0.5))
                    .flatten()
            })
            .collect();
        let rows = (b.y as usize..b.y as usize + b.height as usize)
            .map(|y| {
                let v = img.inv.d * (y as f64 + 0.5) + img.inv.f;
                (0.0..1.0)
                    .contains(&v)
                    .then(|| img.box_taps_y((1.0 - v) * img.height as f64 - 0.5))
                    .flatten()
            })
            .collect();
        (cols, rows)
    }

    /// The row-band weighted ones count must equal the summed-area table's
    /// and the direct per-row weighted popcount, for every destination box of
    /// random bilevel images at random fractional (and mirrored) minifying
    /// placements, including widths that are not a whole number of bytes
    /// and windows that start mid-byte.
    #[test]
    fn row_band_weighted_ones_match_the_sat_and_the_direct_count() {
        let mut rng = Rng(0x2b0b_a11d_0e5e_1f00);
        let mut boxes = 0u64;
        for trial in 0..60 {
            let sw = 3 + (rng.next() % 140) as u32;
            let sh = 3 + (rng.next() % 120) as u32;
            let row_bytes = (sw as usize).div_ceil(8);
            // Mostly sparse, sometimes dense, so the zero-row shortcut and
            // full bytes are both exercised.
            let dense = trial % 3 == 0;
            let bits: Vec<u8> = (0..row_bytes * sh as usize)
                .map(|_| {
                    let b = rng.byte();
                    if dense || b < 40 { rng.byte() } else { 0 }
                })
                .collect();
            let scale = 1.05 + (rng.next() % 1000) as f64 / 150.0;
            let (w, h) = (sw as f64 / scale, sh as f64 / (scale * 0.8 + 0.3));
            let (w, h) = if trial % 4 == 1 { (-w, h) } else { (w, h) };
            let (w, h) = if trial % 5 == 2 { (w, -h) } else { (w, h) };
            let ox = 1.0 + (rng.next() % 100) as f64 / 37.0 + if w < 0.0 { -w } else { 0.0 };
            let oy = 0.5 + (rng.next() % 100) as f64 / 41.0 + if h < 0.0 { -h } else { 0.0 };
            let img = bilevel_image(sw, sh, bits, ox, oy, w, h);
            let (cols, rows) = binary_taps(&img);
            if cols.iter().all(Option::is_none) || rows.iter().all(Option::is_none) {
                // No destination pixel centre lands in the image.
                assert!(BilevelWindow::new(&img, &cols, &rows).is_none());
                assert!(BilevelIntegral::build(&img, &cols, &rows).is_none());
                continue;
            }
            let window = BilevelWindow::new(&img, &cols, &rows).expect("complete samples");
            let sat = BilevelIntegral::build(&img, &cols, &rows).expect("small table");
            let base = (window.byte_lo * 8) as u32;
            let mut colw = vec![0u32; window.nbytes * 8];
            let mut prefix = vec![0u64; window.nbytes * 8 + 1];
            let row_bits = img.packed_row_bits();
            for ty in rows.iter().flatten() {
                let any = window.column_sums(ty, &mut colw, &mut prefix);
                for tx in cols.iter().flatten() {
                    let banded = if any {
                        row_band_weighted_ones(tx, base, &colw, &prefix)
                    } else {
                        0
                    };
                    let direct: u64 = (ty.lo..=ty.hi)
                        .map(|y| {
                            ty.weight_at(y)
                                * crate::image::weighted_row_ones_in(
                                    &img.samples,
                                    y as usize * row_bits,
                                    tx,
                                )
                        })
                        .sum();
                    assert_eq!(banded, direct, "trial {trial} tx={tx:?} ty={ty:?}");
                    assert_eq!(sat.weighted_ones(tx, ty), direct, "trial {trial}");
                    boxes += 1;
                }
            }
        }
        assert!(boxes > 10_000, "{boxes} boxes");
    }

    /// The row-band painter must leave the same bytes as the per-pixel loop
    /// it replaced the summed-area table in front of: gray and stencil
    /// tables, with and without a clip mask, opaque and partial alpha, over a
    /// backdrop.
    #[test]
    fn row_band_paint_matches_the_per_pixel_loop() {
        let mut rng = Rng(0x0dd_ba11_c0de_5eed);
        for trial in 0..24 {
            let (sw, sh) = (40 + (rng.next() % 90) as u32, 30 + (rng.next() % 90) as u32);
            let row_bytes = (sw as usize).div_ceil(8);
            let bits: Vec<u8> = (0..row_bytes * sh as usize).map(|_| rng.byte()).collect();
            let scale = 1.2 + (rng.next() % 100) as f64 / 25.0;
            let (w, h) = (sw as f64 / scale, sh as f64 / scale);
            let (ox, oy) = (1.3 + (trial % 3) as f64 * 0.27, 0.6 + (trial % 4) as f64 * 0.21);
            let mut img = bilevel_image(sw, sh, bits, ox, oy, w, h);
            img.alpha = if trial % 2 == 0 { 255 } else { 140 };
            let lut = if trial % 3 == 2 {
                img.is_stencil = true;
                img.sample_lut = None;
                img.stencil_rgb = [10, 120, 230];
                img.stencil_bilevel_lut().unwrap()
            } else {
                img.binary_box_lut().unwrap()
            };
            let side = (ox + w).max(oy + h).ceil() as u32 + 2;
            let clip = (trial % 4 >= 2).then(|| random_clip(side, &mut rng));
            let (cols, rows) = binary_taps(&img);
            let (x0, y0) = (img.bounds.x as usize, img.bounds.y as usize);
            let (x1, y1) = (
                x0 + img.bounds.width as usize,
                y0 + img.bounds.height as usize,
            );
            let seed = rng.next();
            let mut got = backdrop(side, seed);
            let banded =
                paint_axis_aligned_binary_box(&img, lut, &mut got, clip.as_ref(), x0, y0, x1, y1);
            let mut want = backdrop(side, seed);
            let serial =
                paint_binary_box_serial(&img, lut, &mut want, clip.as_ref(), &cols, &rows, x0, y0);
            let what = format!("trial {trial}: {sw}x{sh} at ({ox}, {oy}) size {w}x{h}");
            assert_surfaces_equal(&mut got, &mut want, &what);
            assert_eq!(banded.0, serial.0, "{what}: painted");
        }
    }

    /// A 600-dpi Letter page (5100 x 6600 = 33.7M bits) is over the summed-area
    /// table's 32M-entry cap, so before the row-band filter it fell to the
    /// serial per-pixel loop. The row-band filter takes it and paints the same
    /// bytes. A truncated buffer still takes the per-pixel loop (which reads
    /// the missing bytes as zero) instead of indexing past the end.
    #[test]
    fn row_band_paints_a_600_dpi_page_and_declines_a_truncated_one() {
        let mut rng = Rng(0x600d_0600_d0d0_6006);
        let (sw, sh) = (5100u32, 6600u32);
        let row_bytes = (sw as usize).div_ceil(8);
        let bits: Vec<u8> = (0..row_bytes * sh as usize)
            .map(|i| if (i / row_bytes) % 40 < 25 { rng.byte() & rng.byte() } else { 0 })
            .collect();
        // 72 dpi: 612 x 792 device pixels at a fractional offset.
        let img = bilevel_image(sw, sh, bits, 0.4, 0.3, 612.0, 792.0);
        let (cols, rows) = binary_taps(&img);
        assert!(
            BilevelIntegral::build(&img, &cols, &rows).is_none(),
            "over the old table cap"
        );
        assert!(BilevelWindow::new(&img, &cols, &rows).is_some());
        let lut = img.binary_box_lut().unwrap();
        let side = 794u32;
        let mut got = backdrop(side, 7);
        paint_axis_aligned_binary_box(&img, lut, &mut got, None, 0, 0, 613, 793);
        let mut want = backdrop(side, 7);
        paint_binary_box_serial(&img, lut, &mut want, None, &cols, &rows, 0, 0);
        assert_surfaces_equal(&mut got, &mut want, "600 dpi");

        // Truncated: drop the last 100 rows' bytes.
        let mut short = bilevel_image(400, 300, vec![0x5a; 50 * 300], 0.5, 0.5, 61.0, 47.0);
        short.samples = Arc::from(&short.samples[..50 * 200]);
        let (cols, rows) = binary_taps(&short);
        assert!(BilevelWindow::new(&short, &cols, &rows).is_none());
        let lut = short.binary_box_lut().unwrap();
        let mut got = backdrop(64, 9);
        paint_axis_aligned_binary_box(&short, lut, &mut got, None, 0, 0, 62, 48);
        let mut want = backdrop(64, 9);
        paint_binary_box_serial(&short, lut, &mut want, None, &cols, &rows, 0, 0);
        assert_surfaces_equal(&mut got, &mut want, "truncated");
    }

    /// Compare only the pixels the generic body paints at full weight
    /// (`edge_coverage` is `None`). A magnified bilinear draw is allowed ±1
    /// per channel there: the generic bilinear tap weights can sum to 254/255
    /// on an opaque image, where the fast path keeps it opaque.
    fn assert_interior_equal(
        img: &PreparedImage,
        got: &mut Surface,
        want: &mut Surface,
        what: &str,
    ) {
        let bilinear_magnified = img.interpolation == InterpolationMode::Bilinear
            && img.footprint[0] <= 1.0
            && img.footprint[1] <= 1.0;
        for y in 0..got.height {
            let (g, r) = (got.row_mut(y).to_vec(), want.row_mut(y).to_vec());
            for x in 0..got.width {
                if img.edge_coverage(x as f64 + 0.5, y as f64 + 0.5).is_some() {
                    continue;
                }
                let (gp, rp) = (&g[x * 4..x * 4 + 4], &r[x * 4..x * 4 + 4]);
                let close = gp.iter().zip(rp).all(|(a, b)| a.abs_diff(*b) <= 1);
                assert!(
                    gp == rp || (bilinear_magnified && close),
                    "{what}: interior pixel ({x}, {y}) is {gp:?}, generic body {rp:?}"
                );
            }
        }
    }

    /// `img` re-placed at device `(ox, oy)` with device size `w x h`, all
    /// fractional, so the draw has a partially covered border ring.
    fn place(mut img: PreparedImage, ox: f64, oy: f64, w: f64, h: f64) -> PreparedImage {
        img.inv = Matrix {
            a: 1.0 / w,
            b: 0.0,
            c: 0.0,
            d: 1.0 / h,
            e: -ox / w,
            f: -oy / h,
        };
        let (x0, y0) = (ox.floor() as i32, oy.floor() as i32);
        img.bounds = DeviceRect {
            x: x0,
            y: y0,
            width: ((ox + w).ceil() as i32 - x0) as u32,
            height: ((oy + h).ceil() as i32 - y0) as u32,
        };
        img.footprint = [img.width as f64 / w, img.height as f64 / h];
        img
    }

    /// Run `img` through the real image dispatcher (`execute` → `paint_image`,
    /// with whatever promotion and fast path it picks) onto a transparent
    /// `side x side` surface.
    fn dispatched(img: &PreparedImage, side: u32) -> Surface {
        let page = crate::prepared::CpuPreparedPage {
            size: pdf_page_ir::DeviceSize {
                width: side,
                height: side,
            },
            ops: vec![PreparedOp::Image(Box::new(img.clone()))],
            clips: Vec::new(),
            points: Vec::new(),
            subpaths: Vec::new(),
            codecs: pdf_image::CodecRegistry::default(),
            decode_limits: pdf_image::DecodeLimits::default(),
            hinting: pdf_font::HintingPolicy::None,
            color_policy: pdf_render_api::RenderColorPolicy::Original,
            diagnostics: Default::default(),
            image_cache: None,
            decode_floor: None,
            #[cfg(feature = "profiling")]
            profile: Default::default(),
            #[cfg(feature = "profiling")]
            decode_cache: None,
        };
        let mut surface = Surface::new(side as usize, side as usize, Background::Transparent);
        let mut ctx = CpuWorkerContext::default();
        let mut stats = RenderStats::default();
        execute(&page, &mut surface, &mut ctx, &mut stats);
        surface
    }

    /// Every image fast path must equal the full generic body — interior
    /// *and* the anti-aliased border ring — when the image lands on
    /// fractional device coordinates. The other equivalence tests here place
    /// images on whole pixels, where the ring does not exist, or compare
    /// against a `shade()`-only reference that has no edge coverage, so a
    /// fast path that got the ring wrong passed them.
    ///
    /// Covers RGB8, Gray8 (promoted), Indexed (expanded) and CMYK sources,
    /// unmasked and under a stencil, a one-bit and an eight-bit soft mask;
    /// minified, near 1:1 and magnified; nearest and bilinear. Also covers
    /// `/ImageMask` stencils in both `/Decode` polarities.
    ///
    /// Every masked path matches byte for byte, ring included. The opaque
    /// *unmasked* fast paths (`fast_rgb8`, `fast_rgb8_bilinear`,
    /// `fast_rgb8_area`, the CMYK area path, and Gray8/Indexed through their
    /// RGB8 promotions) do not: they paint the ring by pixel centre, at full
    /// weight or not at all, where the generic body paints its A10 fractional
    /// edge coverage. That divergence predates this test and is a rendering
    /// decision, not a fix to slip in (it changes every opaque image that
    /// lands on fractional device coordinates); see the 2026-11-08 scoping
    /// note. Until it is decided, those paths are held to the interior.
    #[test]
    fn fractional_placements_match_the_generic_body_including_the_border_ring() {
        let mut rng = Rng(0xb0d5_7a11_0f0f_1234);
        // (source w, h, device x, y, w, h)
        let placements: [(u32, u32, f64, f64, f64, f64); 5] = [
            (37, 23, 1.3, 2.6, 9.4, 6.2),
            (16, 16, 0.25, 0.75, 15.5, 15.5),
            (64, 48, 2.5, 1.125, 20.3, 14.7),
            (5, 7, 0.6, 1.4, 17.8, 21.3),
            (9, 33, 3.7, 0.2, 4.6, 11.9),
        ];
        let spaces: [(&str, ImageColorSpace, usize); 4] = [
            ("rgb", ImageColorSpace::Rgb, 3),
            ("gray", ImageColorSpace::Gray, 1),
            ("cmyk", ImageColorSpace::Cmyk, 4),
            (
                "indexed",
                ImageColorSpace::Indexed {
                    base: Box::new(ImageColorSpace::Rgb),
                    hival: 255,
                    lookup: Arc::from(
                        (0..256 * 3)
                            .map(|i| (i * 73 % 256) as u8)
                            .collect::<Vec<_>>(),
                    ),
                },
                1,
            ),
        ];
        let mut checked = 0;
        for &(sw, sh, ox, oy, w, h) in &placements {
            let side = (ox + w).max(oy + h).ceil() as u32 + 2;
            for (space_name, space, comps) in &spaces {
                for mask_kind in ["none", "stencil", "smask1", "smask8"] {
                    for interpolation in [InterpolationMode::Nearest, InterpolationMode::Bilinear] {
                        let samples: Vec<u8> = (0..sw as usize * sh as usize * comps)
                            .map(|_| rng.byte())
                            .collect();
                        let mut img = place(
                            make_image_cs(sw, sh, 1, 1, space.clone(), samples),
                            ox,
                            oy,
                            w,
                            h,
                        );
                        img.interpolation = interpolation;
                        let (mw, mh) = (sw.div_ceil(2) + 1, sh + 3);
                        match mask_kind {
                            "stencil" => {
                                img.mask = Some(pdf_page_ir::ImageMask::Stencil(bilevel(
                                    mw, mh, &mut rng,
                                )));
                            }
                            "smask1" => img.smask = Some(bilevel(mw, mh, &mut rng)),
                            "smask8" => {
                                let mask: Vec<u8> = (0..mw * mh)
                                    .map(|_| if rng.byte() < 60 { rng.byte() } else { 255 })
                                    .collect();
                                img.smask = Some(Arc::new(pdf_page_ir::ImageSMask {
                                    width: mw,
                                    height: mh,
                                    bits_per_component: 8,
                                    decode: None,
                                    samples: Arc::from(mask),
                                    codec: None,
                                    codec_data: None,
                                    codec_parms: None,
                                }));
                            }
                            _ => {}
                        }
                        let mut got = dispatched(&img, side);
                        // `paint_image` turns a minified hard stencil into a
                        // coverage soft mask before choosing any path.
                        let generic_img = img
                            .minified_stencil_as_coverage()
                            .unwrap_or_else(|| img.clone());
                        let (mut want, _) = generic_masked_reference(&generic_img, side, side);
                        let what = format!(
                            "{space_name} {mask_kind} {interpolation:?} src {sw}x{sh} at \
                             ({ox}, {oy}) size {w}x{h}"
                        );
                        if mask_kind != "none" {
                            assert_surfaces_equal(&mut got, &mut want, &what);
                        } else {
                            assert_interior_equal(&img, &mut got, &mut want, &what);
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 5 * 4 * 4 * 2);

        // `/ImageMask` stencils: the fast path paints only the interior and
        // leaves the ring to the generic body, so the whole surface matches.
        for &(sw, sh, ox, oy, w, h) in &placements {
            let side = (ox + w).max(oy + h).ceil() as u32 + 2;
            for inverted in [false, true] {
                for interpolation in [InterpolationMode::Nearest, InterpolationMode::Bilinear] {
                    let mut img = stencil_image(sw, sh, ox, oy, w, h, inverted, &mut rng);
                    img.interpolation = interpolation;
                    let mut got = dispatched(&img, side);
                    let (mut want, _) = generic_masked_reference(&img, side, side);
                    let what = format!(
                        "imagemask inverted {inverted} {interpolation:?} src {sw}x{sh} at                          ({ox}, {oy}) size {w}x{h}"
                    );
                    assert_surfaces_equal(&mut got, &mut want, &what);
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 5 * 4 * 4 * 2 + 5 * 2 * 2);

        // One-bit gray images: the low-bit LUT nearest path when magnified,
        // the bilevel box filter when minified, ring left to the generic body.
        for &(sw, sh, ox, oy, w, h) in &placements {
            let side = (ox + w).max(oy + h).ceil() as u32 + 2;
            for interpolation in [InterpolationMode::Nearest, InterpolationMode::Bilinear] {
                let bits: Vec<u8> = (0..(sw as usize).div_ceil(8) * sh as usize)
                    .map(|_| rng.byte())
                    .collect();
                let mut img = bilevel_image(sw, sh, bits, ox, oy, w, h);
                img.interpolation = interpolation;
                let mut got = dispatched(&img, side);
                let (mut want, _) = generic_masked_reference(&img, side, side);
                let what = format!(
                    "gray1 {interpolation:?} src {sw}x{sh} at ({ox}, {oy}) size {w}x{h}"
                );
                assert_interior_equal(&img, &mut got, &mut want, &what);
                checked += 1;
            }
        }
        assert_eq!(checked, 5 * 4 * 4 * 2 + 5 * 2 * 2 + 5 * 2);
    }
}

#[cfg(test)]
mod cancel_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use pdf_page_ir::DeviceSize;
    use pdf_render_api::Background;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A prepared page of `n` identical opaque square fills.
    fn page_with_fills(
        n: usize,
        cancel: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> crate::prepared::CpuPreparedPage {
        let points = vec![[10.0f32, 10.0], [50.0, 10.0], [50.0, 50.0], [10.0, 50.0]];
        let subpaths = vec![(0usize, 4usize)];
        let cmd = crate::prepared::PreparedCommand {
            origin: pdf_page_ir::PaintOrigin::PageContent,
            class: crate::prepared::DrawClass::SolidPath,
            subpath_range: (0, 1),
            rule: crate::raster::FillRule::NonZero,
            rgb: [0, 0, 0],
            premul: [0, 0, 0, 255],
            alpha: 255,
            opaque: true,
            bounds: pdf_page_ir::DeviceRect {
                x: 10,
                y: 10,
                width: 40,
                height: 40,
            },
            clip: None,
            clip_has_mask: false,
            blend: pdf_page_ir::BlendMode::Normal,
            shading: None,
            stencil: None,
        };
        let ops = (0..n).map(|_| PreparedOp::Draw(cmd.clone())).collect();
        crate::prepared::CpuPreparedPage {
            size: DeviceSize {
                width: 64,
                height: 64,
            },
            ops,
            clips: Vec::new(),
            points,
            subpaths,
            codecs: pdf_image::CodecRegistry::default(),
            decode_limits: pdf_image::DecodeLimits {
                should_cancel: cancel,
                ..Default::default()
            },
            hinting: pdf_font::HintingPolicy::None,
            color_policy: pdf_render_api::RenderColorPolicy::Original,
            diagnostics: Default::default(),
            image_cache: None,
            decode_floor: None,
            #[cfg(feature = "profiling")]
            profile: Default::default(),
            #[cfg(feature = "profiling")]
            decode_cache: None,
        }
    }

    #[test]
    fn mid_render_cancellation_stops_between_ops() {
        // The token fires from the very first check: execution must stop at
        // an op boundary with the cancelled flag set, not run all 64 draws.
        let checks = Arc::new(AtomicUsize::new(0));
        let seen = checks.clone();
        let cancel: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            true
        });
        let page = page_with_fills(64, Some(cancel));
        let mut surface = Surface::new(64, 64, Background::White);
        let mut ctx = CpuWorkerContext::default();
        let mut stats = RenderStats::default();
        execute(&page, &mut surface, &mut ctx, &mut stats);
        assert!(stats.cancelled, "mid-render cancellation must be recorded");
        assert_eq!(stats.commands, 0, "no command runs after the flag fires");
        assert!(checks.load(Ordering::Relaxed) >= 1);
    }

    #[test]
    fn cancellation_stops_inside_a_single_coverage_command() {
        // Let the pre-pass and op-boundary probes pass, then fire from the
        // RasterKernel checkpoints while the first (and only) draw is active.
        let checks = Arc::new(AtomicUsize::new(0));
        let seen = checks.clone();
        let cancel: Arc<dyn Fn() -> bool + Send + Sync> =
            Arc::new(move || seen.fetch_add(1, Ordering::Relaxed) >= 3);
        let page = page_with_fills(1, Some(cancel));
        let mut surface = Surface::new(64, 64, Background::White);
        let mut ctx = CpuWorkerContext::default();
        let mut stats = RenderStats::default();
        execute(&page, &mut surface, &mut ctx, &mut stats);
        assert!(stats.cancelled);
        assert_eq!(
            stats.commands, 1,
            "the command began, so cancellation was not merely an op-boundary check"
        );
        assert!(checks.load(Ordering::Relaxed) >= 4);
    }

    #[test]
    fn uncancelled_render_executes_every_op() {
        let page = page_with_fills(64, None);
        let mut surface = Surface::new(64, 64, Background::White);
        let mut ctx = CpuWorkerContext::default();
        let mut stats = RenderStats::default();
        execute(&page, &mut surface, &mut ctx, &mut stats);
        assert!(!stats.cancelled);
        assert_eq!(stats.commands, 64);
    }
}

#[cfg(test)]
mod bilevel_sat_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// The serial reference walk from `BilevelIntegral::build`, verbatim.
    #[allow(clippy::too_many_arguments)]
    fn serial_fill(
        sat: &mut [u32],
        samples: &[u8],
        stride: usize,
        sw: usize,
        sh: usize,
        row_bits: usize,
        col_lo: u32,
        row_lo: u32,
    ) {
        for ly in 0..sh {
            let source_row = row_lo as usize + ly;
            let base_bit = source_row * row_bits + col_lo as usize;
            let (prev, cur) = sat.split_at_mut((ly + 1) * stride);
            let prev = &prev[ly * stride..ly * stride + stride];
            let cur = &mut cur[..stride];
            let mut rowsum = 0u32;
            for (lx, bitpos) in (base_bit..base_bit + sw).enumerate() {
                let byte = samples[bitpos >> 3];
                rowsum += ((byte >> (7 - (bitpos & 7))) & 1) as u32;
                cur[lx + 1] = prev[lx + 1] + rowsum;
            }
        }
    }

    /// The blocked parallel fill must reproduce the serial table exactly —
    /// including its zero sentinel row and column — for row counts that fall
    /// short of, land on, and overrun the block boundary.
    #[test]
    fn parallel_sat_fill_matches_serial_bit_exact() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for &(width, height) in &[
            (1usize, 1usize),
            (7, 3),
            (8, 8),
            (13, 1),
            (1, 97),
            (64, 64),
            (129, 251),
            (255, 129),
        ] {
            let row_bits = width.next_multiple_of(8);
            let samples: Vec<u8> = (0..(row_bits / 8) * height).map(|_| next() as u8).collect();
            // Exercise both a full-image window and an offset sub-window.
            for &(want_col_lo, want_row_lo) in &[(0usize, 0usize), (1, 1)] {
                let (col_lo, row_lo) = (
                    if want_col_lo < width { want_col_lo } else { 0 },
                    if want_row_lo < height { want_row_lo } else { 0 },
                );
                let (sw, sh) = (width - col_lo, height - row_lo);
                let (col_lo, row_lo) = (col_lo as u32, row_lo as u32);
                let stride = sw + 1;
                let entries = stride * (sh + 1);
                let mut want = vec![0u32; entries];
                serial_fill(
                    &mut want, &samples, stride, sw, sh, row_bits, col_lo, row_lo,
                );
                let mut got = vec![0u32; entries];
                BilevelIntegral::fill_parallel(
                    &mut got, &samples, stride, sw, sh, row_bits, col_lo, row_lo,
                );
                assert_eq!(got, want, "{width}x{height} window ({col_lo},{row_lo})");
            }
        }
    }
}
