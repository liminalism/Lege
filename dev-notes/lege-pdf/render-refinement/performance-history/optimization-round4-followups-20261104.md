# Round 4: the round-3 follow-ups (2026-11-04)

Done sequentially in the main checkout on top of `58e7d7de`; no version
change. The measurements, scripts and diffs are under
`.agent/scratch/pdf-render-headsup-20261104/variance-investigation/`
(`round3/adapter-probe`, `round3/*flatten*`, `round4/`, the halftone PDFs
and HENSKIP streams under `round4/halftone/`), summarised in section 12 of
`variance-investigation.md`.

## Scheduler reorder window scales with the workers

`SchedulerOptions::reorder_window_depth` defaulted to 12 whatever the worker
count. A page that finishes out of order holds a window slot until every
earlier page is emitted, so with a window near the worker count a short
document never kept the render workers busy. vogtle (22 pages) at 2 + 6
workers: window 12 → 35 ms, 20 → 31, 32 → 23–25, 48 → no further gain;
the compiled-queue depth made no difference; long documents were flat.
Default now `reorder_window_for(compile, render)` = 4 × workers, floor 12;
the page feed is sized to the window; the option's doc comment names the
unbudgeted reorder-buffer memory the window bounds. Structures at windows
12/32/48/80: no penalty.

## Font resolution deduplicated across compile workers

Per-page compile rows showed every compile worker spending ~3 ms on its
first page: each resolved the same fonts in parallel before the document
cache held them. `FontResourceCache` now hands out a per-font slot with an
in-flight lock, so later compilers wait for the first result. Type 3 fonts
take no slot. A scratch mimalloc build also halved the median compile
(allocator contention) — a binary-level decision, not adopted here.

## Adaptive glyph outline flattening (output change)

The glyph-cache miss path cost ~7 µs per glyph at 72 DPI, almost all in the
rasteriser, because every curve was cut into a fixed 8 segments (~200 edges
for a 7-pixel glyph). Curves are now flattened to the fewest uniform
segments whose chords stay within 1/16 px (FreeType's smooth-rasteriser
threshold), identically on the cached, outline and hinted paths, capped at
64. beans serial 72 DPI 22.7 → 14.7 ms (0.77× → 1.31× hayro); same-binary
A/B beans 1.29–1.40×, vogtle 0.99–1.10×, xenosystems within noise. Pixel
effect on the text documents: 2–7% of pixels move by 4 levels on average,
max 28, none above 32; image documents unchanged.
`PDF_RENDERER_OUTLINE_SEGMENTS=8` reproduces the old output byte for byte
and is how the corpus hash check is kept meaningful.

## Masked area-min alpha pass skips clear runs

Profile over Structures' 632 pages: masked draw 4.89 s = alpha pass 2.66 s
(54%), per-visible-pixel colour pass 1.79 s (37%), dense box filter 0.14 s,
horizontal pass 0.09 s (2%) — the AVX2 horizontal pass from the hand-off
was not worth building. The alpha pass now walks the union row's set texels
and fills clear runs at once, skips the known-nonzero half of the uniform
test, and filters mixed boxes over prepared rows: 2.66 → 2.33 s; draw
7.7 → 7.3 ms per page. A branch-free per-pixel box average measured neutral
(memory-bound) and was dropped. Exact; 2658/2658 hashes identical on the
three MRC/cover documents.

## JBIG2 halftone: tested, not changed

"Halftone skip" is T.88's HENSKIP (§6.6.5.1): a halftone region whose grid
cells can fall outside the region, decoded by a per-pixel generic path
that leaves those cells out — a decoder matter unrelated to the GUI's
halftone checkbox, which selects halftone *encoding*. To exercise it, Lege
1.5.1 (`lege.exe viaduct.pdf all --text-format jbig2 --halftone`) produced
a PDF with two halftone regions (722×595 and 722×1005; 4×4 patterns, 16
levels, MMR pattern dictionary, arithmetic gray planes, HENSKIP=0 — Lege's
encoder never sets it). Those pages render pixel-identical to hayro 0.9.0.
Synthetic HENSKIP=1 streams (grids with a skipped column, 4/8/40-px
patterns, up to 10×6 cells) decode identically to hayro as well. The one
defect was in jbig2enc-rust's test-only stream writer
(`tests/common/writer.rs::halftone_skip_page`): it fed the values of
skipped cells into later contexts, where §6.2.5.7 sets a skipped pixel to
0 first, so its `halftone_enable_skip` test passed only because its 5×4
stream was too short for the contexts to diverge. The writer is fixed;
the decoder is unchanged. (hayro renders Lege's cover page — a DCT image
under a blank JBIG2 layer — as green stripes; Lege's renderer shows the
painting. Not pursued here.)

## Low-bit-depth images drawn at or above their resolution (found by the halftone test)

The halftone pages took 175–205 ms each at 144 DPI against hayro's 12–15,
and Lege's own JBIG2 text layers (737×1200, 1-bit DeviceGray) 105 ms
against 8.3. Not the decoder: a 1-, 2- or 4-bit single-component image
drawn axis-aligned at footprint ≤ 1 had no fast path (RGB8 nearest and
bilinear, area-min and the summed-area bilevel box path all needed either
8-bit RGB or minification), so it fell into the generic per-pixel loop —
an inverse map, an edge test and a `shade()` per destination pixel, ~30 ns
each. `paint_axis_aligned_lut_nearest_opaque` maps device columns and rows
to source texels once and reads the packed row through the prepared
lookup table, byte-identical to `shade()` (unit test at 1:1, magnified,
fractional placement, partly off-surface, 1/2/4 bpc, inverting
`/Decode`). Lege's output: 105 → 5.5 ms per page serially at 144 DPI
(hayro 8.3), 27.5 → 1.9 ms at 72 DPI (hayro 3.9), whole 10-page file 985 →
73 ms (hayro 99); halftone pages 175/205 → 8.8/9.6 ms (hayro 12.5/14.5).
Corpus hashes unchanged (no corpus document has such an image at ≤ 1
footprint; its bilevel layers are stencil masks).

## Examined, unchanged

- JPEG Huffman: already a 10-bit lookahead, combined AC code+magnitude
  table and batched refills; the residual against zune-jpeg needs a
  stream-level harness.
- Page composition already works a word at a time and swaps full-cover
  regions in without a copy.
