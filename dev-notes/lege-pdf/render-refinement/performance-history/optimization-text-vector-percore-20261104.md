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
