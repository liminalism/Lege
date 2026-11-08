# Changelog

The renderer workspace (`lege-pdf/render/crates/*`) is versioned as a unit.
Versions below 0.4.0 were workspace-internal and never published.

## 0.4.0 — Unreleased

First numbered release of the renderer workspace. The number reflects the
work since the native renderer was promoted on 2026-07-26: three performance
passes with byte-identical output, a hardened codec boundary, and the read,
print and agent surfaces built on it.

### Fixes, 2026-11-08

- Image cache: reduced decodes are keyed by the scale the codec decodes
  at. The DCT codec decodes a minified draw at 1/2, 1/4 or 1/8 scale, but
  the shared image cache kept the draw footprint in its key only for JPX,
  so a page's first render fixed its JPEG resolution for every later
  render through the same backend: a thumbnail, a zoomed-out view, or
  Lege's 800-pixel analysis render followed by a full-size render magnified
  the small decode, and output depended on render history. Codecs now
  report the level a draw selects (`ImageCodec::reduction_level`: the DCT
  scale from the frame header, the JPX wavelet level from the main header
  through jp2lam's own rule, 0 for JBIG2 and CCITT), and the cache keys by
  that level, so draw sizes that select one level still share a decode. An
  injected codec that reports no level is keyed by the raw footprint. A
  page rendered once is unchanged unless it draws one JPEG at two sizes
  that select different scales (it used the first draw's decode for both);
  a page rendered at two such sizes now decodes the JPEG once per scale.
- Image cache: the declared colour space is part of a decode's identity.
  The JPX codec reads its family (an `/Indexed` space bypasses a container
  palette) and component count (two keep a duotone's channels), so two
  images with byte-identical JPX streams under different `/ColorSpace`
  entries shared one entry and the second drawn got the first one's raster.
- Decode reference (`CpuRenderOptions::image_decode_reference`, through
  `CpuBackend::render_with_options`): a request can name another raster of
  the same page whose image footprints set the lowest resolution its images
  decode at. Renders that share a reference decode each JPEG or JPX image
  once and render from the same source pixels; a draw larger than the
  reference keeps its own resolution, tiling cells inherit the reference,
  and the decoded resolution follows from the request alone, with the
  cache on or off. `lege_pdf_read::RasterProduct::with_decode_reference`
  carries it; Lege's page plan names the base output raster for every
  product of a page, so the analysis render no longer decodes a smaller
  copy of each scan before the base render.

### Performance, 2026-11-04, round 4 (follow-ups to round 3)

Byte-identical to round 3 on the full private corpus except where noted;
the one output change is the glyph outline flattening, which is kept
reproducible through an override.

- Scheduler: the reorder window now scales with the worker counts
  (`SchedulerOptions::reorder_window_for`, four slots per worker, floor 12)
  instead of a fixed 12. A finished page waits in the reorder buffer holding
  a window slot, so a window near the worker count starved the render
  workers on short documents: a 22-page book at eight workers went from
  0.62× hayro to level with it (best of three: 45 → 38 ms against hayro's
  39 ms). Long documents are unaffected. The page feed is sized to the
  window too, and the window's unbudgeted reorder-buffer memory is
  documented on the option.
- Fonts: the document-scoped font resource cache resolves each font once
  even when several compile workers reach it at the same time (a per-font
  slot with an in-flight lock), where before every compile worker starting
  on a different page of the same book parsed the same programs in
  parallel. Type 3 fonts take no slot.
- Glyphs: outline curves are flattened adaptively — the fewest uniform
  segments whose chords stay within 1/16 px of the curve (FreeType's smooth
  rasterizer threshold) — instead of a fixed eight per curve. Body text at
  screen resolution rasterises one or two segments per curve, display sizes
  more than eight. The miss path of the glyph coverage cache, which was
  about 7 µs per glyph at 72 DPI with 200 edges for a 7-pixel glyph, drives
  the three-page beans document from 22.7 to 14.7 ms serially at 72 DPI
  (0.77× → 1.31× hayro). Output changes at anti-aliased glyph edges only:
  on the text documents 2–7% of pixels move by 4 levels on average, none by
  more than 28, and no image document changes.
  `PDF_RENDERER_OUTLINE_SEGMENTS=8` restores the previous fixed subdivision
  byte for byte (every other change in this pass verifies identical under
  it).
- Masked area minification: the alpha pass walks the mask union row's set
  texels and fills whole runs of clear boxes at once, a box known to reach
  a set texel takes only the all-set half of the uniform test, and the
  mixed-box filter reads prepared row offsets and weights. Structures'
  masked draw: 7.7 → 7.3 ms per page serially (the alpha pass 2.66 → 2.33 s
  over the 632 pages). Profiling showed the horizontal pass named in the
  round-3 hand-off at 2% of the draw, so it was left alone; the remaining
  cost is per visible pixel (mask coverage, box average, composite) and
  memory-bound.
- Low-bit-depth images: a 1-, 2- or 4-bit single-component image drawn
  axis-aligned at or above its own resolution — the bilevel JBIG2 or CCITT
  page of a scan, an MRC producer or Lege's own output — now reads its
  packed source row through the image's prepared lookup table
  (`paint_axis_aligned_lut_nearest_opaque`), where before it fell into the
  generic per-pixel loop at ~30 ns a pixel. Lege 1.5.1's own JBIG2 output
  (737×1200 text layers): 105 → 5.5 ms per page serially at 144 DPI (hayro
  8.3) and 27.5 → 1.9 ms at 72 DPI (hayro 3.9); its halftone-region pages
  175–205 → 9–10 ms (hayro 12.5–14.5). Byte-identical: a unit test holds the
  path to the generic sampler at 1:1, magnified, fractionally placed and
  partly off the surface, at 1, 2 and 4 bits, with and without an inverting
  `/Decode`, and the corpus hashes are unchanged.
- JBIG2 halftone regions, exercised rather than changed: a halftone-region
  PDF made with Lege 1.5.1 (`--halftone --text-format jbig2`; 4×4 cells, 16
  patterns, arithmetic gray planes) renders identically to hayro, and
  synthetic HENSKIP=1 streams (T.88 §6.6.5.1 skipped cells) decode
  identically to hayro too. The one defect found was in jbig2enc-rust's
  test-only stream writer, which fed the values of skipped cells into later
  contexts where the standard has 0 (fixed there; the decoder is
  unchanged). Lege's own encoder never sets HENSKIP and no corpus stream
  has a halftone region.
- Examined and left unchanged: the JPEG Huffman decoder already carries a
  10-bit lookahead, a combined code-and-magnitude AC table and batched
  refills, and the residual against zune-jpeg needs a stream-level harness
  to attribute; page composition already works a word at a time with
  full-cover regions swapped in without a copy.

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
