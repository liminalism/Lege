//! The CPU render surface and pixel compositing.
//!
//! The internal buffer is always 8-bit RGBA with **premultiplied alpha**,
//! matching the frozen output contract (`OutputFormat::Rgba8PremultipliedSrgb`)
//! so the common path is a straight ownership handoff. Compositing is
//! source-over in the device (sRGB-encoded) space — the same space every 8-bit
//! PDF compositor uses; linear-light compositing is a later, opt-in refinement.
//!
//! `Gray8` output is produced by a final downconversion, so there is a single
//! compositing path regardless of the requested format.

use pdf_page_ir::DeviceRect;
use pdf_render_api::{Background, OutputFormat};
use std::{mem::MaybeUninit, sync::Arc};

/// An RGBA8 premultiplied render target. A surface may be a sub-region of the
/// device plane: `origin` is the device coordinate of its local `(0, 0)`, so
/// bounded offscreen group buffers share the executor's absolute-coordinate
/// math (advice §9).
// `Clone` snapshots the pixel buffer — used by knockout groups, whose every
// element composites against the group's *initial* backdrop (§11.6.6).
#[derive(Debug)]
pub struct Surface {
    pub width: usize,
    pub height: usize,
    pub origin_x: usize,
    pub origin_y: usize,
    /// `width * height * 4` bytes, premultiplied RGBA, row-major.
    data: Arc<[u8]>,
}

// A clone is a knockout-group backdrop snapshot, not another handle to the
// same mutable render target. Keep that value semantics even though the main
// page now lives in its final Arc allocation.
impl Clone for Surface {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            origin_x: self.origin_x,
            origin_y: self.origin_y,
            data: Arc::from(self.data.as_ref()),
        }
    }
}

impl Surface {
    /// The surface's pixel count, or `None` when `width * height * 4` does not
    /// fit a `usize`.
    ///
    /// Release builds have `overflow-checks = false`, so an unchecked
    /// `width * height * 4` would wrap silently and leave a small allocation
    /// paired with large `width`/`height` fields — the buffer and the geometry
    /// would disagree, which is the shape every `assume_init` and raw-store in
    /// this module relies on being impossible. Callers already bound the
    /// output against `max_page_bytes`, so this only ever fires on a caller
    /// bug; degrade to an empty surface rather than a torn one.
    fn checked_pixels(width: usize, height: usize) -> Option<usize> {
        let pixels = width.checked_mul(height)?;
        pixels.checked_mul(4).map(|_| pixels)
    }

    /// Allocate the main page surface (origin `(0,0)`) and paint its background.
    pub fn new(width: usize, height: usize, background: Background) -> Self {
        Self::new_recycled(width, height, background, None)
    }

    /// Allocate the main page surface, reusing `recycled` when it is an
    /// exactly-sized buffer this call can take sole ownership of.
    ///
    /// A page surface is a large, short-lived allocation: at sweep resolution
    /// a single page is well past glibc's 32 MiB dynamic `mmap` ceiling, so
    /// every render `mmap`s fresh pages and pays a minor fault plus a kernel
    /// page-zero for each of them *before* the background fill can write a
    /// byte. Reusing the previous render's buffer keeps those pages resident,
    /// which turns the whole background paint into a plain resident memset.
    /// The bytes written are exactly those [`Surface::new`] would have
    /// written, so the surface the executor sees is identical either way.
    pub fn new_recycled(
        width: usize,
        height: usize,
        background: Background,
        recycled: Option<Arc<[u8]>>,
    ) -> Self {
        let Some(pixels) = Self::checked_pixels(width, height) else {
            return Self::empty();
        };
        let len = pixels * 4;
        let data = match reuse(recycled, len) {
            Some(mut buf) => {
                paint_background(unique_arc_mut(&mut buf), background);
                buf
            }
            None => match background {
                Background::Transparent => filled_arc(len, 0),
                Background::White => filled_arc(len, 0xFF),
                Background::Solid(c) => repeated_rgba_arc(pixels, solid_rgba(c)),
            },
        };
        Self {
            width,
            height,
            origin_x: 0,
            origin_y: 0,
            data,
        }
    }

    /// Allocate a transparent offscreen surface covering `bounds` (device
    /// space) — for an isolated transparency group.
    pub fn offscreen(bounds: DeviceRect) -> Self {
        let width = bounds.width as usize;
        let height = bounds.height as usize;
        // Same reasoning as `Surface::new`: group bounds are clamped to the
        // page in lowering, so this cannot overflow in practice — but the
        // buffer and the geometry must never be allowed to disagree.
        let Some(pixels) = Self::checked_pixels(width, height) else {
            return Self::empty();
        };
        Self {
            width,
            height,
            origin_x: bounds.x.max(0) as usize,
            origin_y: bounds.y.max(0) as usize,
            data: filled_arc(pixels * 4, 0),
        }
    }

    /// A zero-sized surface. Every row/pixel loop over it is a no-op, so it is
    /// the safe degradation for a geometry that cannot be allocated.
    fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            origin_x: 0,
            origin_y: 0,
            data: filled_arc(0, 0),
        }
    }

    /// Allocate an offscreen covering `bounds` (device space) **seeded with the
    /// parent's backdrop** — for a *non-isolated* transparency group, whose
    /// elements composite against the group's backdrop rather than a transparent
    /// one (ISO 32000-1 §11.4.7; mirrors PDFium's `GetDIBits` +
    /// `CreateWithBackdrop` in `CPDF_RenderStatus::ProcessTransparency`). Pixels
    /// of `bounds` outside `parent` are left transparent.
    pub fn offscreen_seeded(parent: &Surface, bounds: DeviceRect) -> Self {
        let mut off = Surface::offscreen(bounds);
        let stride = off.width * 4;
        // The overlap is a rectangle, so its column span is the same on every
        // row: resolve it once and copy each row in one `copy_from_slice`
        // rather than testing and copying pixel by pixel. This is the common
        // wrapper case for non-isolated groups, so it runs over whole-page
        // areas.
        let x_start = off.origin_x.max(parent.origin_x);
        let x_end = (off.origin_x + off.width).min(parent.origin_x + parent.width);
        if x_end <= x_start {
            return off;
        }
        let span = (x_end - x_start) * 4;
        let dst_off = (x_start - off.origin_x) * 4;
        let src_off = (x_start - parent.origin_x) * 4;

        let y_start = off.origin_y.max(parent.origin_y);
        let y_end = (off.origin_y + off.height).min(parent.origin_y + parent.height);
        for abs_y in y_start..y_end {
            let ly = abs_y - off.origin_y;
            let prow = parent.local_row(abs_y - parent.origin_y);
            let orow = &mut unique_arc_mut(&mut off.data)[ly * stride..(ly + 1) * stride];
            orow[dst_off..dst_off + span].copy_from_slice(&prow[src_off..src_off + span]);
        }
        off
    }

    pub fn bytes(&self) -> u64 {
        self.data.len() as u64
    }

    /// Mutable access to one row's RGBA bytes, addressed by **absolute** device
    /// `y` — the buffer the span kernels write into.
    #[inline]
    pub fn row_mut(&mut self, y: usize) -> &mut [u8] {
        let stride = self.width * 4;
        let local = y - self.origin_y;
        &mut unique_arc_mut(&mut self.data)[local * stride..(local + 1) * stride]
    }

    /// Contiguous mutable rows for absolute device-Y range `[y0, y1)`, clipped
    /// to this surface. Used by the axis-aligned image fast paths to paint
    /// independent rows in parallel without re-borrowing the surface each row.
    ///
    /// Returns `(buffer, first_absolute_y, row_stride_bytes)`.
    #[inline]
    pub(crate) fn rows_mut_abs(&mut self, y0: usize, y1: usize) -> (&mut [u8], usize, usize) {
        let stride = self.width * 4;
        let local0 = y0.saturating_sub(self.origin_y).min(self.height);
        let local1 = y1.saturating_sub(self.origin_y).min(self.height);
        let local0 = local0.min(local1);
        let first_abs_y = self.origin_y + local0;
        (
            &mut unique_arc_mut(&mut self.data)[local0 * stride..local1 * stride],
            first_abs_y,
            stride,
        )
    }

    /// Immutable access to one row by **local** index (for compositing a group
    /// back onto its parent).
    #[inline]
    pub fn local_row(&self, local_y: usize) -> &[u8] {
        let stride = self.width * 4;
        &self.data[local_y * stride..(local_y + 1) * stride]
    }

    /// Restore pixels from an equal-geometry snapshot without replacing this
    /// surface's allocation. Knockout groups use this between elements so the
    /// scratch surface keeps one allocation for the whole group.
    pub(crate) fn copy_pixels_from(&mut self, source: &Self) {
        debug_assert_eq!(self.width, source.width);
        debug_assert_eq!(self.height, source.height);
        debug_assert_eq!(self.origin_x, source.origin_x);
        debug_assert_eq!(self.origin_y, source.origin_y);
        unique_arc_mut(&mut self.data).copy_from_slice(&source.data);
    }

    /// Consume the surface into output bytes of the requested format.
    /// Returns `(stride, pixels)`.
    #[allow(
        unsafe_code,
        reason = "Arc::new_uninit_slice hands back MaybeUninit; see SAFETY comment"
    )]
    pub fn into_output(self, format: OutputFormat) -> (usize, Arc<[u8]>) {
        self.into_output_recycling(format, &mut None)
    }

    /// [`Surface::into_output`], additionally parking this surface's buffer in
    /// `pool` so the next render of the same geometry can repaint it in place
    /// instead of faulting in a fresh mapping. For the RGBA format the parked
    /// handle is a second `Arc` on the *returned* buffer, so the pool never
    /// keeps a page alive past its consumer: reuse is granted only once that
    /// consumer has dropped it (see [`reuse`]).
    #[allow(
        unsafe_code,
        reason = "Arc::new_uninit_slice hands back MaybeUninit; see SAFETY comment"
    )]
    pub(crate) fn into_output_recycling(
        self,
        format: OutputFormat,
        pool: &mut Option<Arc<[u8]>>,
    ) -> (usize, Arc<[u8]>) {
        match format {
            OutputFormat::Rgba8PremultipliedSrgb => {
                park(pool, Arc::clone(&self.data));
                (self.width * 4, self.data)
            }
            OutputFormat::Gray8 => {
                let pixels = self.width * self.height;
                // `zip` stops at the shorter side. The RGBA buffer is exactly
                // four bytes per output pixel by construction, so the two
                // lengths agree — but "the invariant holds" is not something
                // `assume_init` may be asked to take on trust: a short buffer
                // would leave a tail of `out` uninitialized and reading it is
                // UB, not a panic. Count what was written and initialize any
                // remainder explicitly, so soundness does not depend on the
                // invariant at all.
                debug_assert_eq!(self.data.len(), pixels * 4);
                let mut out = Arc::<[u8]>::new_uninit_slice(pixels);
                let out_mut = unique_arc_mut(&mut out);
                let mut written = 0usize;
                for (dst, px) in out_mut.iter_mut().zip(self.data.chunks_exact(4)) {
                    dst.write(gray8_luma(px[0], px[1], px[2]));
                    written += 1;
                }
                for dst in &mut out_mut[written..] {
                    dst.write(0);
                }
                // SAFETY: the loop initialized `out[..written]` and the fill
                // above initialized `out[written..]`, so every element of
                // `out` is initialized regardless of the source length.
                let gray = unsafe { out.assume_init() };
                let width = self.width;
                // The RGBA buffer is not the returned page here, so it can be
                // parked outright — nothing else holds it.
                park(pool, self.data);
                (width, gray)
            }
        }
    }
}

/// The Gray8 output value of one premultiplied (composited-over-black) RGB
/// pixel: its Rec. 709 luma, rounded half away from zero.
///
/// Byte-identical to `y.round().clamp(0.0, 255.0) as u8` over all 2^24 inputs
/// (`gray8_luma_matches_round_on_every_input`), without the `roundf` libm call
/// that `round()` compiles to at the baseline x86-64 ISA, which made this
/// conversion cost more than rendering a whole text page. `y` lies in
/// `[0, 255.00002]`, so `y as i32` is `floor(y)`, `y - floor(y)` is exact,
/// and comparing it with 0.5 is exact round-half-away-from-zero. The sum is
/// kept in its original order; reassociating it changes results.
#[inline(always)]
fn gray8_luma(r: u8, g: u8, b: u8) -> u8 {
    let y = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
    let floor = y as i32;
    (floor + i32::from(y - floor as f32 >= 0.5)).min(255) as u8
}

/// Allocate the render buffer directly in the `Arc<[u8]>` layout returned by
/// `HostPage`. Going through a `Vec<u8>` forces `Arc::from(Vec<_>)` to allocate
/// and copy the entire page because the Arc refcounts need adjacent storage.
#[allow(
    unsafe_code,
    reason = "Arc::new_uninit_slice/new_zeroed_slice hand back MaybeUninit; see SAFETY comments"
)]
pub(crate) fn filled_arc(len: usize, byte: u8) -> Arc<[u8]> {
    if byte == 0 {
        let data = Arc::<[u8]>::new_zeroed_slice(len);
        // SAFETY: `new_zeroed_slice` initialized every `u8` to a valid zero.
        return unsafe { data.assume_init() };
    }
    let mut data = Arc::<[u8]>::new_uninit_slice(len);
    unique_arc_mut(&mut data).fill(MaybeUninit::new(byte));
    // SAFETY: `fill` initialized every element of the allocation.
    unsafe { data.assume_init() }
}

/// Allocate zeroed bytes directly in their final shared layout. Callers may
/// mutate the allocation through [`unique_arc_mut`] until publishing a clone.
pub(crate) fn zeroed_arc(len: usize) -> Arc<[u8]> {
    filled_arc(len, 0)
}

/// The premultiplied RGBA bytes of a `Background::Solid` color.
#[inline]
fn solid_rgba(c: pdf_page_ir::Color) -> [u8; 4] {
    let a = c.a.clamp(0.0, 1.0);
    [
        to_u8(c.r.clamp(0.0, 1.0) * a),
        to_u8(c.g.clamp(0.0, 1.0) * a),
        to_u8(c.b.clamp(0.0, 1.0) * a),
        to_u8(a),
    ]
}

/// Write `background` over an already-allocated surface buffer. Byte-for-byte
/// what the matching `filled_arc`/`repeated_rgba_arc` allocation would hold.
fn paint_background(buf: &mut [u8], background: Background) {
    match background {
        Background::Transparent => buf.fill(0),
        Background::White => buf.fill(0xFF),
        Background::Solid(c) => {
            let px = solid_rgba(c);
            for chunk in buf.chunks_exact_mut(4) {
                chunk.copy_from_slice(&px);
            }
        }
    }
}

/// Extra parked-surface slots kept per thread beside the worker context's own
/// slot (so three sizes in all). lege-process renders one page at an analysis
/// size, the base size and an OCR size on the same thread in turn; a single
/// slot missed on every alternation. `CpuWorkerContext::surface_buffer` stays
/// the most-recently-parked slot; displaced buffers of a different size move
/// here. Parked buffers are repainted in full before use, so which slot
/// supplies a buffer never changes output bytes.
const SPARE_SLOTS: usize = 2;
/// Larger buffers are not retained in the spare slots.
const SPARE_MAX_BYTES: usize = 128 << 20;

thread_local! {
    static SPARE: std::cell::RefCell<Vec<Arc<[u8]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

static SLOT_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SLOT_MISSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Process-wide `(hits, misses)` of parked-surface lookups (a hit found a
/// parked buffer of the wanted size, in any slot; ownership is checked later).
pub fn surface_slot_stats() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (SLOT_HITS.load(Relaxed), SLOT_MISSES.load(Relaxed))
}

/// Park `buf` in `pool` (the most-recent slot), moving a displaced buffer of a
/// different size into the thread's spare slots.
pub(crate) fn park(pool: &mut Option<Arc<[u8]>>, buf: Arc<[u8]>) {
    if let Some(old) = pool.replace(buf) {
        let new_len = pool.as_ref().map_or(0, |b| b.len());
        if old.len() != new_len && old.len() <= SPARE_MAX_BYTES {
            SPARE.with(|s| {
                let mut s = s.borrow_mut();
                // A spare of the displaced size is stale: replace it.
                s.retain(|b| b.len() != old.len());
                if s.len() >= SPARE_SLOTS {
                    s.remove(0);
                }
                s.push(old);
            });
        }
    }
}

/// Take a parked buffer for a `width x height` surface from `pool` or the
/// spare slots, if one of exactly that size is parked.
pub(crate) fn take_recycled(
    pool: &mut Option<Arc<[u8]>>,
    width: usize,
    height: usize,
) -> Option<Arc<[u8]>> {
    use std::sync::atomic::Ordering::Relaxed;
    let found = match Surface::checked_pixels(width, height).map(|p| p * 4) {
        None => None,
        Some(len) if pool.as_ref().is_some_and(|b| b.len() == len) => pool.take(),
        Some(len) => SPARE.with(|s| {
            let mut s = s.borrow_mut();
            let i = s.iter().position(|b| b.len() == len)?;
            Some(s.remove(i))
        }),
    };
    match found {
        Some(_) => SLOT_HITS.fetch_add(1, Relaxed),
        None => SLOT_MISSES.fetch_add(1, Relaxed),
    };
    found
}

/// Accept a recycled buffer only when it is exactly the size wanted and this
/// call can take sole ownership of it — a buffer the previous consumer still
/// holds is dropped here rather than retained, so the pool never pins memory
/// that is in use elsewhere.
fn reuse(recycled: Option<Arc<[u8]>>, len: usize) -> Option<Arc<[u8]>> {
    let mut buf = recycled?;
    if buf.len() != len {
        return None;
    }
    Arc::get_mut(&mut buf)?;
    Some(buf)
}

#[allow(
    unsafe_code,
    reason = "Arc::new_uninit_slice hands back MaybeUninit; see SAFETY comment"
)]
fn repeated_rgba_arc(pixels: usize, rgba: [u8; 4]) -> Arc<[u8]> {
    let mut data = Arc::<[u8]>::new_uninit_slice(pixels * 4);
    for chunk in unique_arc_mut(&mut data).chunks_exact_mut(4) {
        for (dst, src) in chunk.iter_mut().zip(rgba) {
            dst.write(src);
        }
    }
    // SAFETY: `chunks_exact_mut(4)` covers the complete pixels*4 allocation.
    unsafe { data.assume_init() }
}

#[inline]
fn to_u8(v: f32) -> u8 {
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

#[inline]
pub(crate) fn unique_arc_mut<T>(data: &mut Arc<[T]>) -> &mut [T] {
    let Some(data) = Arc::get_mut(data) else {
        unreachable!("a final buffer is never shared while it is mutable")
    };
    data
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: panics are the assertion mechanism
    use super::*;

    #[test]
    fn rgba_output_hands_off_the_surface_pixels() {
        let mut surface = Surface::new(2, 1, Background::White);
        surface.row_mut(0)[0] = 17;

        let (stride, output) = surface.into_output(OutputFormat::Rgba8PremultipliedSrgb);

        assert_eq!(stride, 8);
        assert_eq!(&*output, &[17, 255, 255, 255, 255, 255, 255, 255]);
        assert_eq!(Arc::strong_count(&output), 1);
    }

    #[test]
    fn a_recycled_surface_is_byte_identical_to_a_fresh_one() {
        let backgrounds = [
            Background::White,
            Background::Transparent,
            Background::Solid(pdf_page_ir::Color {
                r: 0.25,
                g: 0.5,
                b: 0.75,
                a: 0.5,
            }),
        ];
        for background in backgrounds {
            // A dirtied buffer of the right size, sole-owned: the shape a
            // previous render hands back.
            let recycled = filled_arc(5 * 3 * 4, 0x5A);
            let allocation = Arc::as_ptr(&recycled);
            let reused = Surface::new_recycled(5, 3, background, Some(recycled));
            let fresh = Surface::new(5, 3, background);
            assert_eq!(&*reused.data, &*fresh.data, "{background:?}");
            assert_eq!(
                Arc::as_ptr(&reused.data),
                allocation,
                "the recycled allocation should have been kept"
            );
        }
    }

    #[test]
    fn recycling_declines_a_shared_or_mismatched_buffer() {
        // Still held elsewhere: must not be written through.
        let shared = filled_arc(2 * 4, 0x11);
        let keep = Arc::clone(&shared);
        let surface = Surface::new_recycled(2, 1, Background::White, Some(shared));
        assert_ne!(Arc::as_ptr(&surface.data), Arc::as_ptr(&keep));
        assert_eq!(&*keep, &[0x11; 8]);
        assert_eq!(&*surface.data, &[255; 8]);

        // Wrong length: also declined.
        let wrong = filled_arc(4, 0x22);
        let ptr = Arc::as_ptr(&wrong);
        let surface = Surface::new_recycled(2, 1, Background::White, Some(wrong));
        assert_ne!(Arc::as_ptr(&surface.data), ptr);
        assert_eq!(&*surface.data, &[255; 8]);
    }

    /// The integer-rounded Gray8 conversion must equal the `round()` form it
    /// replaced on every (r, g, b). Over 2^24 inputs, 2,752 land on exact .5
    /// ties, which is why `(y + 0.5) as u8` and round-half-even are not
    /// substitutes.
    #[test]
    fn gray8_luma_matches_round_on_every_input() {
        for r in 0..=255u8 {
            for g in 0..=255u8 {
                for b in 0..=255u8 {
                    let y = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
                    let want = y.round().clamp(0.0, 255.0) as u8;
                    assert_eq!(gray8_luma(r, g, b), want, "rgb ({r}, {g}, {b})");
                }
            }
        }
    }

    #[test]
    fn into_output_parks_the_buffer_for_the_next_render() {
        let mut pool = None;
        let surface = Surface::new(3, 2, Background::White);
        let allocation = Arc::as_ptr(&surface.data);
        let (_, pixels) =
            surface.into_output_recycling(OutputFormat::Rgba8PremultipliedSrgb, &mut pool);
        // Parked, but the consumer still owns it, so it is not reusable yet.
        assert_eq!(Arc::as_ptr(pool.as_ref().unwrap()), allocation);
        assert!(reuse(pool.clone(), 3 * 2 * 4).is_none());
        drop(pixels);
        // Once the consumer drops it, the same allocation comes back.
        let recycled = reuse(pool, 3 * 2 * 4).expect("sole owner now");
        assert_eq!(Arc::as_ptr(&recycled), allocation);

        // Gray8 does not hand back the RGBA buffer, so it is parked outright.
        let mut pool = None;
        let surface = Surface::new(3, 2, Background::White);
        let allocation = Arc::as_ptr(&surface.data);
        let (_, gray) = surface.into_output_recycling(OutputFormat::Gray8, &mut pool);
        assert_eq!(gray.len(), 6);
        assert_eq!(Arc::as_ptr(pool.as_ref().unwrap()), allocation);
        assert!(reuse(pool, 3 * 2 * 4).is_some());
    }

    #[test]
    fn alternating_sizes_reuse_their_parked_buffers() {
        // Alternate three sizes the way analysis/base/OCR renders do.
        let sizes = [(5usize, 4usize), (9, 7), (3, 2)];
        let mut pool = None;
        let mut addrs = [None; 3];
        let (h0, m0) = surface_slot_stats();
        for round in 0..4 {
            for (i, &(w, h)) in sizes.iter().enumerate() {
                let recycled = take_recycled(&mut pool, w, h);
                if round == 0 {
                    assert!(recycled.is_none());
                } else {
                    let r = recycled.as_ref().expect("parked slot hit");
                    assert_eq!(Arc::as_ptr(r), addrs[i].unwrap(), "size {i} round {round}");
                }
                let surface = Surface::new_recycled(w, h, Background::White, recycled);
                addrs[i].get_or_insert(Arc::as_ptr(&surface.data));
                assert_eq!(Arc::as_ptr(&surface.data), addrs[i].unwrap());
                let (_, out) = surface.into_output_recycling(OutputFormat::Gray8, &mut pool);
                assert_eq!(out.len(), w * h);
            }
        }
        let (h1, m1) = surface_slot_stats();
        assert!(h1 - h0 >= 9 && m1 - m0 >= 3);
        // Recycled output is identical to a fresh surface.
        let fresh = Surface::new(9, 7, Background::White);
        let again = Surface::new_recycled(9, 7, Background::White, take_recycled(&mut pool, 9, 7));
        assert_eq!(fresh.data, again.data);
    }

    #[test]
    fn clone_remains_an_independent_pixel_snapshot() {
        let original = Surface::new(
            1,
            1,
            Background::Solid(pdf_page_ir::Color {
                r: 0.25,
                g: 0.5,
                b: 0.75,
                a: 1.0,
            }),
        );
        let mut clone = original.clone();
        clone.row_mut(0).fill(0);

        assert_ne!(original.local_row(0), clone.local_row(0));
        assert_eq!(original.local_row(0), &[64, 128, 191, 255]);
    }

    #[test]
    fn copying_a_snapshot_reuses_the_destination_allocation() {
        let source = Surface::new(4, 3, Background::White);
        let mut scratch = Surface::new(4, 3, Background::Transparent);
        let allocation = Arc::as_ptr(&scratch.data);

        scratch.copy_pixels_from(&source);

        assert_eq!(Arc::as_ptr(&scratch.data), allocation);
        assert_eq!(scratch.data, source.data);
    }

    #[test]
    fn gray_output_is_fully_initialized() {
        let surface = Surface::new(2, 1, Background::White);
        let (stride, output) = surface.into_output(OutputFormat::Gray8);

        assert_eq!(stride, 2);
        assert_eq!(&*output, &[255, 255]);
    }
}
