# Changelog

The renderer workspace (`lege-pdf/render/crates/*`) is versioned as a unit.
Versions below 0.4.0 were workspace-internal and never published.

## 0.4.0 — Unreleased

First numbered release of the renderer workspace. The number reflects the
work since the native renderer was promoted on 2026-07-26: three performance
passes with byte-identical output, a hardened codec boundary, and the read,
print and agent surfaces built on it.

### Performance, 2026-11-04, round 3 (per-core, byte-identical)

- Glyph blit: one `blend_mask_rect` call per glyph (AVX2 with a scalar
  fallback) that also returns the coverage count, removing the second pass
  over every coverage byte; the glyph-run timer on the text pages drops
  49–60%.
- Solid fills: scanline rows reach solid fills as coverage runs, constant
  runs are painted with one fill or blend, and repeated all-vertical rows
  are re-emitted without recomputation; page-sized fills on xenosystems
  fall from about 35% of the document profile to under half of that.
- Indexed images are expanded once at lowering and kept in the content-keyed
  image cache (key: sample hash, palette, `/Decode`, bits per component).
- Masked and opaque area minification: per-row mask summaries paint
  all-clear and all-set boxes without per-tap work, mixed boxes read one
  64-bit window per mask row, and an exact whole-row box filter (AVX2 finish
  with a scalar reference) replaces the per-pixel averaging. Structures'
  image-draw stage falls from 47–49 to 12–17 ms per page; xenosystems p0
  from 88 to 46 ms warm.
- JBIG2: jbig2enc-rust 0.6.1's windowed generic-region decoder (3.7–11×
  faster on page-sized masks), and the renderer packs and inverts the decoded
  rows in one pass straight into the output buffer. Structures' mask decode
  falls from 36–41 to about 8 ms per page.
- Correction: viaduct, previously described as a vector document, is three
  JPEG pages; its remaining gap is the JPEG decoder.

### Performance, 2026-11-04 (per-core, against hayro 0.9.0)

Measured per page, serial, P-core pinned, with dated notes under
`dev-notes/lege-pdf/render-refinement/performance-history/`. Every change in
this pass is byte-identical to the previous build on the full private corpus
(3698 page renders at 72 and 144 DPI).

- Text and vector: a document-scoped font resource cache (fonts were
  re-resolved per page); sparse scanline rows that write untouched cells as
  constant runs; an AVX2 `blend_mask` for the glyph blit with a runtime scalar
  fallback (`PDF_RENDERER_KERNELS=scalar` for A/B).
- Image draws: separable SIMD bilinear with a shared horizontal-lerp cache
  and a unit blit for integer placements; Indexed, ICC-RGB, ICC-CMYK, Lab and
  tint-table images resolved to RGB8 once and kept in the content-keyed image
  cache; a soft-masked area-minification painter; AVX2 JPEG chroma upsampling
  for 4:2:0, 4:2:2 and 4:4:0, a vectorised colour store, and a lookahead
  Huffman decoder.
- JPX: decode headroom dropped from 2× to 1× (`PDF_RENDERER_JPX_HEADROOM`),
  so 144 DPI takes the one-level decode; jp2lam 0.3.1's serial decoder.
- Fonts: the system font index is built lazily from sparse reads of the
  `name` and `OS/2` tables instead of loading every installed font file.
- Whole-document throughput at 144 DPI, serial, against the 2026-11-04
  morning build: 1.0× (a three-page JPEG document) to 3.9× (JPEG scans, text
  with Indexed images); Lege now leads hayro 0.9.0 on four of eight corpus documents.

### Performance, 2026-09-04

- Page-surface reuse across renders and load-aware image row fan-out
  (`PDF_RENDERER_IMAGE_ROW_PAR`).

### Performance and hardening, 2026-08

- MRC masked-draw fast path for area minification; final image buffer copies
  eliminated; JPX decode into caller buffers.
- Panic isolation restored per page; hot-path allocations cut; the audited
  memory defects in the renderer and jp2lam closed.
- Gray scans take the RGB8 fast paths; coverage by area average.

### Surfaces

- `lege-pdf-read` session crate: header, identity, signature coverage, XMP
  lineage; compiled-artifact LRU shared with printing.
- Office printing end to end (imposition, sheet composition, CUPS and
  winspool spooling).
- The `lege-pdf` agent CLI; feature-gated Android port with a JNI cdylib.
- Windows DX12 validation of the wgpu backend.

### Performance, 2026-07-20

- Signed-area raster kernel; glyph coverage-bitmap cache with sub-pixel
  bucketing; opaque axis-aligned area-minification fast path (RGB8, CMYK);
  CCITT row-band compositing; JPEG decode passes 1–3; JPX decoder integration
  with a load-aware budget.
