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

## Round 2 — AVX2 `blend_mask` for the glyph blit — 2026-11-04 (later the same day)

Follow-up requested after round 1: vectorize `kernels::blend_mask`, the
source-over of a coverage mask with a constant colour, which is the glyph
blit (`paint_glyph_run` calls it once per glyph row) and the partial-coverage
spans of `dispatch_row`. Same ground rules: byte-identical output, scalar
fallback selected at runtime, same crates.

### What changed (`pdf-render-cpu/src/kernels.rs` only)

- `blend_mask_avx2` — eight RGBA8 pixels per step in two 16-lane `u16`
  registers. Coverage bytes widen to `u16`, are scaled by the constant alpha
  with the same `mul_div_255` op sequence as the scalar kernel when
  `alpha != 255`, and are spread to four channels each with an in-lane byte
  shuffle (`_mm256_shuffle_epi8` on a broadcast 128-bit alpha vector). The
  source vector is `[r g b 255]×4`; since `mul_div_255(255, a) == a`, the
  alpha channel takes the same `src·a + dst·(255−a)` formula as the scalar
  `a + mul_div_255(px[3], ia)`. The `u16` sum is masked with `0x00FF` before
  `packus`, reproducing the scalar `as u8` truncation exactly, and a 64-bit
  permute undoes `packus`'s per-lane interleave. The scalar early-outs for
  `c == 0` / `a == 0` are identities in this arithmetic, so there is no
  per-pixel branching.
- Tail: four to seven remaining pixels take one half-width step (128-bit
  pixel load/store, same arithmetic in the low register); zero to three run
  the scalar reference. A first version staged the remainder through a
  zero-padded stack buffer so every row took the full vector step; the
  microbenchmark below showed that at **twice** the scalar cost for rows
  under eight pixels (two variable-length copies plus a store-forwarding
  stall), so it was dropped before measurement on pages.
- `KernelSet::select()` installs it when `is_x86_feature_detected!("avx2")`;
  `PDF_RENDERER_KERNELS=scalar` (or `0`/`off`, read once) keeps the scalar
  table for same-binary A/B. The AVX2 entry re-probes on call (a cached
  atomic load) so a direct call on a CPU without AVX2 still runs the scalar
  kernel. Non-x86_64 builds are unchanged.
- New gate `kernels::tests::blend_mask_avx2_matches_scalar_bit_exact`:
  every (alpha, coverage) pair over 8-pixel spans with random colour and
  backdrop, plus 40 random spans at each length 0..=41 (vector body, half
  step and scalar tail in every combination) against the scalar kernel,
  byte for byte.

### Kernel microbenchmark (one P-core, min of 15 × 200k calls, alpha 255)

| row width (px) | scalar ns | AVX2 ns | ns/px scalar → AVX2 |
|---:|---:|---:|---:|
| 2 | 10.4 | 13.2 | 5.2 → 6.6 |
| 3 | 13.2 | 14.6 | 4.4 → 4.9 |
| 4 | 14.3 | 10.7 | 3.6 → 2.7 |
| 6 | 17.7 | 15.6 | 3.0 → 2.6 |
| 8 | 23.7 | 12.2 | 3.0 → 1.5 |
| 12 | 33.2 | 13.2 | 2.8 → 1.1 |
| 16 | 47.9 | 12.9 | 3.0 → 0.8 |
| 24 | 71.2 | 16.8 | 3.0 → 0.7 |
| 48 | 139.2 | 25.6 | 2.9 → 0.5 |

Per call the floor is ~10 ns (fn-pointer call, feature probe, constant
setup, loop control), which is why a 2–3 px row is a few ns slower and the
gain is bounded by row width: glyph rows at 72 DPI are mostly 3–8 px, at
144 DPI 6–16 px.

### Structured profile, `render.path.glyph_runs` (ms, median of 20, min of two interleaved rounds)

Same `pdfium-diff` binary, `PDF_RENDERER_KERNELS=scalar` vs default, arms
alternated per case so load drift hits both equally. Output hashes
unchanged in every row (xenosystems p152 `c75087d5dfd4d328` /
`0ba3beb2b069a54c`, vogtle p3 `de73ba9be3303db2` / `0baf559928e01705`,
vogtle p10 `23f7c526fe495569` / `b5b20f7ce7485b04`).

| page | glyph blits | scalar | AVX2 | change | share of page before |
|---|---:|---:|---:|---:|---:|
| xenosystems p152, scale 1 | 2,096 | 0.493 | 0.396 | −20% | 24% |
| xenosystems p152, scale 2 | 2,096 | 1.229 | 0.860 | −30% | 34% |
| vogtle p3, scale 1 | 1,497 | 0.192 | 0.183 | −5% | 21% |
| vogtle p3, scale 2 | 1,497 | 0.443 | 0.347 | −22% | 33% |
| vogtle p10, scale 1 | 738 | 0.103 | 0.085 | −17% | 13% |
| vogtle p10, scale 2 | 738 | 0.247 | 0.191 | −23% | 22% |

So the blit is 0.1–0.37 ms per page cheaper at 144 DPI, 3–10% of the page.
What remains in `glyph_runs` is per-glyph/per-row overhead rather than
pixel math: the cache probe and placement clamp per glyph, one indirect
call per row, and the per-row `covered` statistic (`src.iter().filter(c !=
0).count()`), which is a second pass over every coverage byte and is not
profiling-gated. Folding that count into the kernel or sampling it, and
blitting a whole glyph (all rows) per call so the constants are set up once,
are the next steps if the glyph path is revisited; neither was in scope.

### Target pages, warm, serial, P-cores (ms) — noise-bounded today

The machine was **not idle** for this round: a qemu VM and two JVMs kept
15–45% of the CPUs busy throughout, and hayro itself read 40% slower than
in round 1 (3.9 vs 2.7 ms on xenosystems p152 scale 1). Absolute numbers are
therefore not comparable with the round-1 table, only arms within one
interleaved round with each other. Arms were alternated per case
(hayro → 751de415 → this change → this change forced scalar), three rounds,
each arm's value the median of 20 warm re-renders; median of the three
rounds, with the min in brackets.

| page | hayro 0.9.0 | 751de415 | AVX2 | AVX2 forced scalar |
|---|---:|---:|---:|---:|
| xenosystems p152, scale 1 | 3.92 [3.71] | 2.42 [2.16] | 2.42 [2.11] | 2.52 [2.35] |
| xenosystems p152, scale 2 | 7.43 [6.92] | 5.44 [4.91] | **4.80 [4.51]** | 5.27 [5.14] |
| vogtle p3, scale 1 | 2.14 [1.94] | 1.22 [1.11] | 1.44 [1.18] | 1.23 [1.16] |
| vogtle p3, scale 2 | 3.54 [3.18] | 2.35 [1.94] | 2.40 [1.77] | 2.21 [1.85] |
| vogtle p10, scale 1 | 1.59 [1.38] | 1.17 [1.08] | 1.57 [0.93] | 1.09 [1.06] |
| vogtle p10, scale 2 | 2.43 [2.32] | 2.14 [1.67] | **1.75 [1.53]** | 2.48 [1.79] |

The only whole-page differences that clear the noise are the 144 DPI text
pages (xenosystems p152 −12%, vogtle p10 −18% median), consistent with the
profile timer; the 72 DPI rows are flat within noise. Every target page
stays ahead of hayro per core, as after round 1. The round-1 conclusions
stand: the per-core text page is now bounded by per-glyph overhead and
compile, not by pixel work.

### Output identity

`verify` mode (per-page pixel hash, 8 workers) with the new adapter build
against the 751de415 build on the same 390 page renders as round 1
(vogtle 22 pages × scale 1 and 2, xenosystems 305 × scale 1 and 17 × scale
2, beans 3 × 2 scales, viaduct 3, Structures 5, centralpark 4, sahib 3,
chinese-medicine 3): all identical. `cargo test -p pdf-render-cpu` (86 unit
tests + integration suites) passes; `cargo clippy -p pdf-render-cpu
--all-targets` reports nothing in `kernels.rs`.

### Reproduce (additions)

```sh
PDF_RENDERER_KERNELS=scalar pdfium-diff profile <scale> <pdf> <page> 20 scalar.jsonl
pdfium-diff profile <scale> <pdf> <page> 20 avx2.jsonl        # compare render.path.glyph_runs
cargo test -p pdf-render-cpu --release blend_mask_avx2_matches_scalar_bit_exact
```

## Round 3 (2026-11-04) — whole-glyph blit, run-aware page fills, viaduct attribution

Brief: (1) remove the glyph blit's per-row overhead (one indirect kernel call
per coverage row, and the non-profiling `covered` count that walked every
coverage byte a second time); (2) profile viaduct ("pure vector", 33 vs
hayro's 22 ms/page at 144 DPI) and fix its largest item; (3) a further lever
in `kernels.rs` / `raster.rs` / the text and path-fill parts of `exec.rs`.
Same ground rules: byte-identical output, scalar fallback and
`PDF_RENDERER_KERNELS=scalar` kept, no dependency or version changes.

### Method

- Timing: the heads-up adapter (`headsup-lege-final`, `sample` / `time` /
  `verify` modes) rebuilt against the round-3 branch, against the retained
  main-checkout build at `035660da` ("base", same code as this round's
  starting point `9c93b580` apart from a test-only cfg gate) and the
  retained `headsup-rust-bench hayro` adapter. Serial, process affinity
  `0xFFF` (P-cores), `RAYON_NUM_THREADS=1 PDF_RENDERER_JPX_CONCURRENCY=serial
  PDF_RENDERER_IMAGE_ROW_PAR=0`; arms interleaved per case with the order
  reversed every round, three rounds; each value per round is the median of
  20 warm re-renders (sample mode) or one whole-document pass (time mode);
  tables give median [min] over the rounds.
- Structured profile: a small profiling adapter (`compile_profiled` +
  `render_profiled_with`, `pdf-render-cpu/profiling`), the same
  `warm_document` shape as `pdfium-diff profile` (compile + render per
  sample, 6 warm-ups, medians of 20), plus a whole-document mode (every page
  once, summed report) and a cold mode (fresh backend per sample).
- **Load.** Two other workers were building and benchmarking throughout,
  plus two JVMs: total CPU sampled at 15–98% (mostly 40–80%) during the
  runs. hayro read 1.3–1.7× slower than in round 1 on the same pages, as in
  round 2. Absolute numbers are therefore not comparable with the round-1
  table; arms inside one interleaved run are, and the minima are the
  steadier figure.

### 1. Glyph blit: one kernel call per glyph, coverage counted in-kernel

`KernelSet::blend_mask_rect(dst, dst_stride, cov, cov_stride, width,
height, rgb, alpha) -> u64` blends a whole clamped glyph bitmap and returns
its non-zero coverage count. `paint_glyph_run`'s simple path (Normal blend,
no clip mask, no soft mask, solid colour — the only path the target pages
take) now takes the surface rows of the glyph's clamped box once
(`Surface::rows_mut_abs`) and makes one call per glyph; the separate
`src.iter().filter(c != 0).count()` pass is gone. Masked, soft-masked,
non-Normal and shaded runs keep the per-row path unchanged.

- Scalar `blend_mask_rect`: the per-pixel body of `blend_mask` (factored
  into a shared `blend_mask_px` so the two references cannot drift) with
  the count fused into the loop.
- `blend_mask_rect_avx2`: the same lane arithmetic as round 2's
  `blend_mask_avx2` (eight pixels in two 16-lane `u16` registers), with the
  constants set up once per glyph rather than once per row, and the one to
  seven pixel row remainder done as **one masked eight-pixel step**
  (`vpmaskmovd` load/store, so pixels past the row are neither read nor
  written) instead of the half step plus scalar tail — glyph rows are 3–16
  px, so the remainder is most of the work. Remainder coverage is one 8-byte
  load when the bitmap has eight bytes from there (the extra bytes feed only
  masked-off lanes), else the eight bytes ending at the bitmap end shifted
  down, else (a bitmap under eight bytes) a byte loop. The count is
  `cmpeq` + `movemask` + `popcnt` on the coverage already in a register. The
  extent of the rectangle is checked once with slicing before the unsafe
  body. Installed by `KernelSet::select()` with AVX2 (+POPCNT, both probed);
  `PDF_RENDERER_KERNELS=scalar` keeps the scalar table.
- Gate: `kernels::tests::blend_mask_rect_avx2_matches_scalar_bit_exact` —
  widths 0..=41 × heights {0,1,2,3,5,9} × padded and tight strides ×
  coverage buffers ending exactly at the last row (all three remainder
  sources), random colour/alpha/backdrop: scalar and AVX2 both equal the old
  per-row reference (`blend_mask` per row + separate count) byte for byte
  and count for count, and pixels outside the rectangle stay untouched.

`render.path.glyph_runs` (ms, profile adapter, three interleaved rounds,
median [min]; load ~40%):

| page | glyph blits | round 2 note | start (`9c93b580`) | round 3 | change vs start (min) |
|---|---:|---:|---:|---:|---:|
| xenosystems p152, scale 2 | 2,096 | 0.860 | 1.255 [0.911] | 0.475 [0.377] | −59% |
| vogtle p3, scale 2 | 1,497 | 0.347 | 0.371 [0.358] | 0.173 [0.162] | −55% |
| vogtle p10, scale 2 | 738 | 0.191 | 0.197 [0.170] | 0.086 [0.086] | −49% |
| xenosystems p152, scale 1 | 2,096 | 0.396 | 0.424 [0.407] | 0.167 [0.160] | −61% |
| vogtle p3, scale 1 | 1,497 | 0.183 | 0.212 [0.184] | 0.079 [0.078] | −58% |
| vogtle p10, scale 1 | 738 | 0.085 | 0.098 [0.097] | 0.062 [0.039] | −60% |

The 144 DPI target (≥20% below the round-2 numbers 0.860 / 0.347 / 0.191)
is met by a wide margin even against the round-2 figures measured on an
idle machine: 0.377 / 0.162 / 0.086 ms. Forced scalar
(`PDF_RENDERER_KERNELS=scalar`), the fused scalar kernel measures about
where the old per-row AVX2 path did (xenosystems p152 scale 2 1.14 [1.04],
vogtle p3 scale 2 0.40 [0.35]). The glyph blit is now ~0.18 µs per glyph at
144 DPI on a loaded machine.

### 2. viaduct is not a vector document: it is three JPEG decodes

The brief's premise did not hold. `viaduct.pdf`'s three pages each paint one
`DCTDecode` RGB image through a Form XObject (1000×599, 1274×1024 and
2067×2880 px, baseline 4:2:0, 0.32 / 0.36 / 1.45 MB of entropy data); there
is no path fill or stroke on any page (`render.solid_paths` = 0,
`render.commands` = 1). Per page at 144 DPI, cold (fresh backend per sample,
which is what the whole-document run sees on its single visit):

| page | total | `codec.dct.decode` | image draw (`render.image`) | surface | compile |
|---|---:|---:|---:|---:|---:|
| 0 | 11.6 ms | 6.7 (58%) | 2.4 | 1.8 | 0.4 |
| 1 | 17.4 ms | 11.6 (66%) | 3.4 | 1.9 | 0.4 |
| 2 | 47.9 ms | 41.2 (86%) | 4.6 | 1.8 | 1.5 |

Warm (decoded image cached) the same pages take 3.1 / 3.4 / 7.8 ms. The
whole-document gap to hayro (30–32 vs 37–40 pages/s at 144 DPI in this
round's runs; per page 14.5–15.6 / 24–27 / 52–56 ms against hayro's 11.1 /
16–22 / 45–46) is therefore the JPEG decode in `pdf-image` (the remaining
JPEG Huffman residual named in `variance-investigation.md` §10) plus the
area-average minification draw — both owned by the image-path session, so
nothing was changed for it here. Lege's own share of the page outside those
two is the 1.8 ms cold surface allocation and the 0.4 ms compile. Nothing in
the text/vector area can close it; the gap is not structural, it is in the
codec.

### 3. Page-sized solid fills: run-aware rows and row repetition

With the glyph blit fixed, the largest text/vector item left in the corpus
is xenosystems' per-page background: every page paints a near-black
page-sized rectangle (fractional right/bottom edges, so not `OpaqueRect`)
and a border stroke clipped to the same rectangle. In the whole-document
profile at 144 DPI these two fills were `render.path.solid_paths` 528–606 ms
of 1,526–1,701 ms (~35%), more than the 2,096-glyph pages' blits.

Both go through the scanline (AET) path. Per row, the sparse conversion
from round 1 wrote the rectangle's constant interior into the coverage row
(a 1,190-byte memset), `dispatch_row` scanned the row again for its 255-run,
`fill_opaque` painted it, and the driver zeroed the row; the stroked ring's
rows were scanned and zeroed across the page width for a few partial cells
at each end. And every interior row re-ran the edge deposits and the
conversion, although the rows are identical.

- **Run-aware hand-over.** `RasterKernel::fill_runs_cancellable` gives the
  row callback the row as `CovRun`s: cell runs (bytes in the buffer) and
  constant runs that were never written into it; zero gaps are omitted.
  `paint_command`'s solid Normal fills without clip mask, soft mask,
  stencil or shading use `dispatch_runs`: a constant 255 run is one
  `fill_opaque` (or `blend_const` at the fill alpha), a partial constant run
  `c` one `blend_const` at `a = c·alpha` (exactly `blend_mask`'s arithmetic
  at constant coverage — gated by
  `kernels::tests::blend_const_matches_blend_mask_at_constant_coverage`,
  every `c` × every alpha), and only cell runs go through the unchanged span
  classifier (factored out of `dispatch_row` as `classify_spans`). The
  driver then zeroes only the cell runs. Every other fill writes the
  constant runs into the buffer, takes `dispatch_row` as before and reports
  the buffer dirty. `fill` / `fill_cancellable` (clip masks, glyph
  rasterization, attribution) are unchanged and still materialize in
  `convert_row_sparse` exactly as before.
- **Row repetition** (run-aware mode only). If every active edge of a row
  is vertical and spans the whole row, each deposit is `accumulate(x, yf, x,
  yf + 1, dir)`: same columns, `cover = ((yf + 1) − yf)·dir = dir` (exact
  below 2^24 rows), `x = x_at_top + (yf − y_top)·0.0` (a zero of fixed sign,
  or NaN alike). The next row with the same active set — no activation, no
  retirement, every edge still reaching `yf + 1` — accumulates bit-identical
  values in the same order and converts to the same cells, runs and span, so
  those cells are kept in the buffer and the row is re-emitted without
  deposits or conversion. Anything else (an edge starting or ending inside
  the row, a non-vertical edge, a consumer that wrote the buffer) releases
  the held cells and computes the row normally.
- Gates: `raster::tests::fill_runs_match_fill` — the run hand-over, rebuilt
  into a grid, equals `fill`'s coverage and row spans on page-wide rects
  (integral, fractional, overhanging, sub-pixel), a stroked ring, overlapping
  rects giving partial constant runs under even-odd, a half-covered band,
  nested rects activating and retiring mid-band, a diagonal interrupting the
  vertical rows, a compact-window fill and 40 random wide paths; both rules;
  both consumer contracts (untouched buffer / materialized and dirty); one
  kernel reused across all fills so a missed reset surfaces in the next
  fill; constant runs asserted unwritten. A deliberately wrong repeat
  emission makes it fail. `aet_sparse_rows_match_dense`,
  `aet_matches_reference_bit_exact` and `aet_matches_reference_on_edge_cases`
  still pass.

Profile timers, xenosystems page 152 (ms, three interleaved rounds, median
[min]; `solid_dispatch` includes two `Instant::now()` per row, so it reads
high in absolute terms):

| scale | timer | start | run-aware only | + row repetition |
|---|---|---:|---:|---:|
| 2 | `solid_paths` | 1.788 [1.323] | 1.360 [1.162] | 0.986 [0.835] |
| 2 | `solid_dispatch` | 1.064 [0.723] | 0.677 [0.556] | 0.718 [0.614] |
| 1 | `solid_paths` | 0.711 [0.433] | 0.570 [0.559] | 0.303 [0.224] |

(The "run-aware only" column is from the run that paired it with row
repetition: 1.360 [1.162] → 0.879 [0.804] at scale 2, 0.570 → 0.307 at
scale 1.) The edge/conversion share of the two fills (`solid_paths` −
`solid_dispatch`) went from ~0.6–0.7 to ~0.25 ms at 144 DPI; what remains in
dispatch is mostly `fill_opaque` writing 8 MB, which a stand-alone
microbenchmark puts at 0.24–0.27 ms per page-sized fill whether written as
32-bit splats, 64-bit words or explicit 32-byte AVX2 stores — it is
store-bandwidth bound, so no SIMD fill kernel was added.

Whole-document profile, xenosystems at 144 DPI (two passes each, summed
report): `solid_paths` 528–606 → 260–272 ms, `benchmark.total` 1,526–1,701
→ 1,164–1,248 ms.

### Results

Target pages, warm, serial, P-cores (ms, median [min] of three interleaved
rounds; final measurement run, load 40–98%):

| page | hayro 0.9.0 | base (`035660da`) | round 3 | round 3 / base (median) |
|---|---:|---:|---:|---:|
| xenosystems p152, scale 2 | 8.57 [7.83] | 6.05 [5.92] | **4.38 [4.27]** | 0.72× |
| xenosystems p152, scale 1 | 4.31 [4.11] | 2.70 [2.66] | **2.41 [1.92]** | 0.89× |
| vogtle p3, scale 2 | 3.83 [3.64] | 2.50 [1.96] | 2.04 [1.95] | 0.82× |
| vogtle p3, scale 1 | 3.02 [2.02] | 1.39 [1.22] | 1.44 [1.16] | noise |
| vogtle p10, scale 2 | 3.31 [2.82] | 2.02 [1.79] | 2.00 [1.86] | noise |
| vogtle p10, scale 1 | 1.46 [1.42] | 1.23 [1.12] | 1.56 [1.12] | noise |

An earlier run under lighter load (run-aware fills, before row repetition)
gave xenosystems p152 scale 2 hayro 7.06 [6.80], base 4.73 [4.44], round 3
4.02 [3.87]. As in round 2, only the xenosystems page clears the noise in
whole-page timing; the vogtle pages are compile-bound (~0.5–0.9 ms of their
~1–2 ms) and their glyph saving (0.1–0.2 ms) is within the run-to-run
spread under this load. Every target page stays ahead of hayro.

Whole document, serial, P-cores (pages/s, best of three interleaved):

| document | hayro | base | round 3 | round 3 / base |
|---|---:|---:|---:|---:|
| xenosystems, 144 DPI | 119.1 (150.8) | 198.5 | **242.0** | 1.22× |
| xenosystems, 72 DPI | 277.3 (283.5) | 404.7 | **468.2** | 1.16× |
| vogtle, 144 DPI | 116.9 (169.5) | 126.6 | 138.4 | 1.09× |
| vogtle, 72 DPI | 204.9 (207.8) | 229.7 | 218.3 | noise |
| beans, 144 DPI | 59.7 (64.0) | 76.0 | 83.7 | 1.10× |
| viaduct, 144 DPI | 36.6 (40.4) | 32.2 | 30.4 | untouched (JPEG) |
| viaduct, 72 DPI | 52.0 (53.9) | 52.1 | 50.7 | untouched (JPEG) |

hayro in brackets: the same cases from the lighter-load run earlier in the
session; hayro was hit harder by the load of the final run than Lege was,
so the hayro ratios of the final run flatter Lege. vogtle's remaining
whole-document gap to hayro under light load (144 vs 170 pages/s) is its
scanned-image pages (`codec.dct.decode` 44–78 ms of the 22-page document),
as round 1 found.

### Output identity

`verify` mode (per-page pixel hash, 8 workers), final build against the base
build, each document at scale 1 and 2, default kernels and
`PDF_RENDERER_KERNELS=scalar`: vogtle 22 + 22, viaduct 3 + 3, beans 3 + 3,
xenosystems 305 + 305 pages — 1,332 page renders, all identical. The two
intermediate builds were verified the same way (1,320 and 1,332 renders,
all identical).

### Tried, measured, not pursued

- Precomputing the non-zero count per cached `GlyphBitmap` (zero per-blit
  work) would touch `prepared.rs`, which another session owns; the in-kernel
  count costs one `popcnt` per eight pixels.
- An AVX2 `fill_opaque`: 0.237 vs 0.274 ms per page-sized fill in isolation,
  bandwidth bound; not worth a kernel-table entry.
- Glyph cache misses (temporary, uncommitted timers in the lowering): the
  outline rasterization itself (`RasterKernel::fill`, compact window) is
  7.3 ms of vogtle's 142 ms, 15.8 of xenosystems' 955 ms, but 8.6 of beans'
  36 ms at 144 DPI (892 misses, ~10 µs each — large display glyphs through
  the dense compact-window sweep). That is the next text-side lever on short
  documents: a sparse/row-repeating pass 2 for the compact window, the same
  idea as above applied to glyph-sized windows.

### Reproduce (additions)

```sh
cargo test -p pdf-render-cpu --lib blend_mask_rect_avx2_matches_scalar_bit_exact
cargo test -p pdf-render-cpu --lib fill_runs_match_fill
cargo test -p pdf-render-cpu --lib blend_const_matches_blend_mask_at_constant_coverage
# profile adapter: warm per page, or `all` (whole document), or `cold`
headsup-lege-prof <pdf> <scale> <page|all> [runs] [cold]
```
