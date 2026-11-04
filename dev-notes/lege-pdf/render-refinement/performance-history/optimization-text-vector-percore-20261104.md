# Per-core text/vector gap vs hayro 0.9.0 — font-resource cache + sparse scanline rows — 2026-11-04

Follow-up to the 2026-11-04 heads-up (`variance-investigation.md` §8), where
hayro 0.9.0 led Lege per core on the Latin text book (xenosystems, 182 vs 63
pages/s at 144 DPI) and the vector/Type 1 drawing (vogtle, 187 vs 86). Brief:
profile first, take the two largest items, keep output byte-identical.

Machine: i7-13700H, serial runs pinned to the six P-cores (affinity `0xFFF`),
`PDF_RENDERER_JPX_CONCURRENCY=serial PDF_RENDERER_IMAGE_ROW_PAR=0
RAYON_NUM_THREADS=1`. Lege binary: the heads-up adapter
(`headsup-lege-final`, `sample`/`verify`/whole-document modes) rebuilt against
this worktree; hayro: the retained `headsup-rust-bench hayro` adapter. Scale 1
= 72 DPI, scale 2 = 144 DPI. "warm" = median of 20 re-renders of one page
after 6 warm-ups (document open excluded, page compile included, as hayro's
interpreter re-runs per render too).

## Where the time went (structured profile, before)

`pdfium-diff profile` (`--features bench-profiling`), `warm_document` mode =
compile + render per sample, medians of 20. Two profiling-only timers were
added for this: `render.path.glyph_runs` / `render.path.solid_paths` split
`render.path` by command class, and `render.path.solid_dispatch` is the span
classification + kernel share inside the coverage callback (the rest of
`solid_paths` is edge build + coverage accumulation in `RasterKernel`).

| page | total | compile.interpret | render.execute | of which glyph blits | of which 2 page-sized fills | surface + output |
|---|---:|---:|---:|---:|---:|---:|
| xenosystems p152 (2,525 glyphs), scale 1 | 4.93 ms | 1.00 | 2.71 | 0.36 | 1.91 (raster 1.49, dispatch 0.42) | 0.07 |
| xenosystems p152, scale 2 | 12.51 ms | 1.06 | 10.59 | 0.98 | 7.31 (raster 5.77, dispatch 1.54) | 0.25 |
| vogtle p3 (1,743 glyphs, Type 1), scale 1 | 2.80 ms | **2.02** | 0.19 | 0.19 | — | 0.03 |
| vogtle p3, scale 2 | 2.84 ms | **1.88** | 0.38 | 0.38 | — | 0.12 |
| vogtle p10 (841 glyphs, 6 fills), scale 1 | 2.97 ms | **2.39** | 0.12 | 0.11 | 0.01 | 0.02 |
| vogtle p10, scale 2 | 3.15 ms | **2.33** | 0.26 | 0.25 | 0.01 | 0.12 |

Neither suspected item from the July notes was it. The glyph cache path
(probe + `blend_mask` blit) costs ~0.47 µs per glyph at 144 DPI, about 1 ms on
the densest page; surface clear and output conversion are under 0.3 ms. The
two largest items were:

1. **Per-page font re-resolution in the content interpreter** (all of
   vogtle's 2.0–2.4 ms `compile.interpret`, 1.0 ms of xenosystems'). Every
   page compile builds a fresh `Interpreter` whose font cache is keyed by
   resource name and lives for that page only, so each `Tf` of an embedded
   font re-inflated the `/FontFile` stream, re-parsed the program (Type 1
   charstrings here), and re-walked all 256 encoding codes through name/cmap
   lookups. vogtle has two Type 1 fonts per page; that was ~1 ms each. The
   render side already had a document-scoped parsed-program cache
   (`SharedFontProgramCache`); the compile side had none, and every
   production caller (the scheduler's `make` closure, the CLI, the heads-up
   adapter) constructs compiler + `ParseContext` per page, so a
   context-scoped cache would not have helped.
2. **Page-sized rectangle fills through the per-cell scanline sweep** (7.3 of
   xenosystems' 10.6 ms execute at 144 DPI). Page 152 paints a dark
   full-page background rect (fractional edges, so not `OpaqueRect`) and
   strokes a page border. A page-sized window (2 M cells, 2–8 edges) is
   correctly routed to the sparse-window AET path, but that path's pass 2
   walked every cell of each row (`running += acc[x]; acc[x] = 0;
   coverage_byte(running)`) — ~2.9 ns per cell over 2 M cells per fill —
   although only two to eight cells per row had ever been written. Then
   `dispatch_row` scanned the resulting 1,191-byte row one byte at a time
   for its 255-run, and `fill_opaque` wrote four byte stores per pixel.

## What changed

### `pdf-document`: document-scoped extension slots

`DocumentSnapshot::extension::<T>()` — a type-erased, lazily created
`Arc<T>` per snapshot, keyed by `TypeId`, shared by every worker and page of
the document for as long as the snapshot lives. The document crate learns
nothing about what the slot holds; a private newtype gives a crate a slot
nothing else can reach.

### `pdf-content`: `FontResourceCache` in that slot

`Interpreter::resolve_font` keys on the font dictionary's `ObjectId` plus the
identity of the system-font provider (a substituted font depends on who
answered). A hit clones the cached `SemFont` (shared `Arc`s and small vectors)
and re-labels its resource name. Type 3 fonts are not cached (their CharProcs
are interpreted inline and tracked per page); a font reached through a
direct dictionary has no object identity and resolves as before. Bounded at
64 MiB of retained program bytes (+16 KiB per entry); overflow clears the map,
costing the next page one re-resolution per font.

### `pdf-render-cpu`: sparse scanline rows, chunked span scan, splat fill

- `RasterKernel::fill_scanline_aet` now records, per row, the `[start, end)`
  cell interval each contributing edge's deposits touched (one push per
  edge per row), and `convert_row_sparse` sorts/merges those intervals and
  writes the gaps between them as a constant run (`fill`) instead of
  summing, converting and resetting each cell. Untouched cells hold exactly
  `0.0`, so skipping them changes the running sum only in the sign of zero,
  which `coverage_byte` does not see — the output is byte-identical to the
  dense sweep. A row with more than 48 contributing edges or a span under 64
  cells takes the dense sweep unchanged (edge-dense map pages do not pay the
  sort). The compact-window fast path (glyphs, ordinary vector fills) is
  untouched.
- `exec::dispatch_row` walks runs of `0` and `255` coverage eight bytes at
  a time before the per-byte tail.
- `kernels::fill_opaque` copies the whole pixel per step so the loop lowers
  to a 32-bit splat the compiler vectorizes.

New gate: `raster::tests::aet_sparse_rows_match_dense` — page-wide rects
(integer, fractional, overhanging, sub-pixel wide), a stroked-border ring,
two rects on one row, a long diagonal, and 60 random wide-device paths, each
checked byte-for-byte against the naive reference via both the AET driver and
the production `fill` entry point, both fill rules. The pre-existing
`aet_matches_reference_bit_exact` / `fast_path_matches_reference_bit_exact`
gates still pass.

## Results

### Target pages, warm, serial, P-cores (ms, median of 20)

Paired run on an idle machine, hayro → before → after back to back.

| page | hayro 0.9.0 | before | after | before / hayro | after / hayro | speedup |
|---|---:|---:|---:|---:|---:|---:|
| xenosystems p152, scale 1 | 2.67 | 3.92 | **1.66** | 1.47× | **0.62×** | 2.36× |
| xenosystems p152, scale 2 | 5.00 | 10.71 | **3.76** | 2.14× | **0.75×** | 2.85× |
| vogtle p3, scale 1 | 1.43 | 2.47 | **0.90** | 1.73× | **0.63×** | 2.74× |
| vogtle p3, scale 2 | 2.43 | 3.18 | **1.50** | 1.31× | **0.62×** | 2.12× |
| vogtle p10, scale 1 | 1.01 | 2.75 | **0.82** | 2.72× | **0.81×** | 3.35× |
| vogtle p10, scale 2 | 1.79 | 3.29 | **1.36** | 1.84× | **0.76×** | 2.42× |

Goal was ≤1.2× hayro on xenosystems text pages and ≤1.5× on vogtle; every
target page is now faster than hayro per core.

Structured profile after (same modes as above): xenosystems p152 scale 2
`warm_document` 12.51 → **2.95 ms** (`compile.total` 1.25 → 0.50,
`render.path.solid_paths` 7.31 → 0.87 of which dispatch 0.50,
`render.path.glyph_runs` 0.98 → 1.03); vogtle p3 scale 1 2.80 → **0.79 ms**
(`compile.interpret` 2.02 → 0.20). The largest remaining render item on the
text page is the 2,096 glyph blits at ~1 ms.

### Whole document, serial, P-cores (pages/s, best of 2, first visit per page)

| document | hayro | before | after | after / hayro |
|---|---:|---:|---:|---:|
| xenosystems, 144 DPI | 191.4 | 73.1 | **169.6** | 1.13× slower |
| xenosystems, 72 DPI | 376.7 | 196.3 | **356.9** | 1.06× slower |
| vogtle, 144 DPI | 202.1 | 98.3 | 113.7 | 1.78× slower |
| vogtle, 72 DPI | 297.9 | 166.9 | 228.7 | 1.30× slower |
| beans, 144 DPI | 80.7 | 48.4 | **98.1** | faster |

xenosystems still carries its pathological page 0 cover image (handled in a
separate session). vogtle's remaining whole-document gap is **not text or
vector**: per-page, 18 of its 22 pages are now 0.6–1.1× hayro, and the gap
is pages 14 (76.7 vs 11.2 ms), 16 (27.1 vs 8.3), 6 (12.0 vs 8.5) and 0 (30.6
vs 26.4), which are scanned-image pages — `render.image` is 68.1 of page 14's
68.8 ms (a 1317×882 8-bpc image minified ~2× at 144 DPI, ~59 ns per source
pixel) and 19.5 of page 16's 20.3 ms. That is the image-draw kernel work
tracked in its own session; it is out of scope here and noted in the shared
session notes.

### Output identity

Byte-identical before vs after on every page rendered: output hashes of the
six profiled page/scale cases are unchanged (xenosystems p152 scale 1
`c75087d5dfd4d328`, scale 2 `0ba3beb2b069a54c`; vogtle p3 scale 1
`de73ba9be3303db2`, scale 2 `0baf559928e01705`; vogtle p10 scale 1
`23f7c526fe495569`, scale 2 `b5b20f7ce7485b04`), and the adapter's `verify`
mode (per-page pixel hash, 8 workers) agrees on all 390 page renders of:
vogtle (22 pages) at scale 1 and 2, xenosystems all 305 pages at scale 1 and
17 pages at scale 2, beans (3) at scale 1 and 2, viaduct (3), Structures (5),
centralpark (4), sahib (3), chinese-medicine (3) at scale 1.

`cargo test -p pdf-render-cpu`, `cargo test -p pdf-content -p pdf-document`
pass.

## Not architectural

The sparse-strip vs full-page-coverage question from the brief does not
arise at this level: with the two items above removed, the text page's
remaining render cost is the per-glyph blit (~0.5 µs per glyph; `blend_mask`
is a scalar per-pixel source-over) and the vector pages are compile-bound at
~0.3 ms. Candidates if the per-core gap is revisited: a SIMD `blend_mask`
for the glyph blit; extending the sparse-row conversion to the compact 2-D
window's pass 2 for large-but-dense windows; and the cold-page costs visible
in the whole-document numbers (glyph-cache misses extract and rasterize
~200 outlines per vogtle page, ~1 ms).

## Reproduce

```sh
# Lege adapter against this tree (copy of .agent/scratch/pdf-render-headsup-20261104/lege
# with the crate paths pointed at the worktree), hayro adapter as retained there.
cargo build --release --locked --manifest-path <adapter>/Cargo.toml
headsup-lege-final <pdf> <scale> 1 sample <outdir> <page>     # 26 samples, rows[6..] warm
headsup-lege-final <pdf> <scale> 8 verify <outdir> [pages]    # per-page pixel hash
headsup-lege-final <pdf> <scale> 1 time <outdir>              # whole document serial
# Structured profile (adds render.path.glyph_runs / .solid_paths / .solid_dispatch):
cargo build --release --features bench-profiling --manifest-path lege-pdf/render/tools/pdfium-diff/Cargo.toml
pdfium-diff profile <scale> <pdf> <page> 20 out.jsonl
```
