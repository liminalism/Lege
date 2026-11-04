# jp2lam serial decode — 2026-11-04

`variance-investigation.md` (sections 3 and 8, `.agent/scratch/pdf-render-headsup-20261104/`)
placed the remaining Lege gap on the MRC scans in the codec: the two full-page JPX layers per
page of *Structures* and *chinese-medicine* cost 1.5–1.8× hayro-jpeg2000 0.5.0 per layer,
serially, and whole-document renders decode every image serially (`Budgeted(1)` under load),
so per-core decode throughput is the whole story. Goal: jp2lam's single-threaded decode of
those layers at or under hayro at full resolution and at one reduction level, bit-identical
output, and the chinese-medicine 100-page serial run (adapter, 144 DPI) from ~191 ms/page
toward hayro's 164.

Branch `claude/peaceful-montalcini-878c4d`, parent `5951f37a`. All changes are inside
`lege-codecs/jp2lam/src/`; the renderer is untouched.

## Result

Paired micro-benchmark (`lege-misc/dev-misc/jpxbench`, raw `/JPXDecode` streams decoded to
packed RGB8, serial, P-core pinned, `RAYON_NUM_THREADS=1`, min of 7, ms per layer). `lvl 0` is
full resolution, `lvl 1` one reduction level (what the renderer asks for at 144 DPI after the
headroom fix, `d39b8f1a`). hayro numbers are from the same runs.

| stream (layer) | lvl | output | jp2lam before | jp2lam after | hayro | after/hayro |
|---|---:|---|---:|---:|---:|---:|
| cm p0 layer 0 (2929×4424) | 0 | 2929×4424 | 384.6 | 166.5 | 246.5 | 0.68 |
| | 1 | 1465×2212 | 158.3 | 82.4 | 91.5 | 0.90 |
| cm p0 layer 1 | 0 | 2929×4424 | 345.1 | 151.6 | 235.8 | 0.65 |
| | 1 | 1465×2212 | 149.7 | 75.4 | 87.8 | 0.86 |
| cm p1 layer 0 (2364×4162) | 0 | 2364×4162 | 172.5 | 72.4 | 142.0 | 0.51 |
| | 1 | 1182×2081 | 47.2 | 19.5 | 33.7 | 0.58 |
| structures p0 layer 0 (2222×3191) | 0 | 2222×3191 | 199.5 | 92.3 | 133.3 | 0.69 |
| | 1 | 1111×1596 | 104.7 | 52.5 | 56.2 | 0.93 |
| structures p0 layer 1 | 0 | 2222×3191 | 124.7 | 50.4 | 99.8 | 0.51 |
| | 1 | 1111×1596 | 33.9 | 14.1 | 23.6 | 0.60 |
| structures p1 layer 0 (2157×3189) | 0 | 2157×3189 | 131.5 | 52.0 | 98.7 | 0.53 |
| | 1 | 1079×1595 | 39.9 | 16.4 | 25.0 | 0.66 |

Across all 20 streams of the corpus (pages 0, 1, 2, 100, 300 of both books, both layers) at
both levels, jp2lam is under hayro on every one of the 40 measurements; the thinnest margins are
the dense text layers at one reduction level (cm p2 layer 1: 82.3 vs 84.1 ms, ratio 0.98;
structures p0 layer 0: 0.92). The full before/after tables are `lege-misc/dev-misc/jpxbench/results/serial-before-20261104.txt`
and `serial-20261104.txt`, the raster hashes `raster-hashes-20261104.txt`.

Whole-document, through the retained adapter (`headsup-lege-final`, serial, P-core pinned,
`PDF_RENDERER_JPX_CONCURRENCY=serial PDF_RENDERER_IMAGE_ROW_PAR=0 RAYON_NUM_THREADS=1`,
144 DPI, two runs each, median ms/page over pages 1–99, first page cold in brackets):

| document, 100 pages @144 DPI | Lege before | Lege after | hayro 0.9.0 |
|---|---:|---:|---:|
| chinese-medicine | 207–212 (430–457) | 124–125 (294–311) | 144–145 (284–288) |
| Structures | 115 (211–226) | 88–89 (146–157) | 62 (120–126) |

Whole documents, same settings, one run each (median ms/page; wall seconds): chinese-medicine
392 pages Lege 210 → 118 (82.4 → 45.9 s) against hayro's retained 164 (64.4 s, section 8 of the
report); Structures 632 pages Lege 128 → 88 (83.8 → 57.0 s) against hayro's 68 (43.2 s). On the
JPX-dominated book Lege is now ahead of hayro serially (4.8 → 8.5 pages/s against hayro's 6.1 on
the whole document; 8.0 against 6.9 on the first 100 pages); on Structures the decode no longer dominates and the remaining 26 ms/page are outside the codec.

Output is bit-identical: the 20 stream rasters hash equal to the pre-change decoder at both
levels after every step, and the equivalence tests below pin each rewritten stage to the code
it replaced.

## Where the time went, and what changed

Stage times for cm p0 layer 0 (full resolution, 13 Mpx, serial, `--stats`; totals in stats mode
are a few ms above the timed runs). hayro's breakdown comes from a patched copy with per-stage
timers (`lege-misc/dev-misc/jpxbench/hayro-trace.patch`).

| ms | Tier-1 | inverse DWT | colour + pack | total |
|---|---:|---:|---:|---:|
| jp2lam after | 87 (sig 23, ref 13, cln 33, out 17) | 32 (levels 1–4) | 43 (level 5 fused with ICT and pack) | ~170 |
| hayro 0.5.0 | 137 | 98 | 51 (store 34, mct 8, shift 9) | ~288 |

The stage counters (`Jp2Decoder::decode_with_stats`) were added on this branch, so there is no
per-stage "before" row; the pre-change total for this layer was 385–395 ms, and the per-step
totals below show where each stage's time went.

At one reduction level (1465×2212) the same layer is Tier-1 64, DWT 6, fused final 10 ms for
jp2lam against hayro's 81 / 20 / 12: Tier-1 is three quarters of the reduced-resolution time
and the margin there is the smallest.

The steps, in the order they landed, with the cm p0 layer 0 full-resolution min after each
(395 → 166 ms):

1. **SIMD packed output** (`decode/reconstruct.rs`, `pack_row_f32`): the final
   ICT + level shift + clamp + byte interleave ran scalar per sample over three planes;
   it is now one `f32x8` row kernel over the Y/Cb/Cr rows with the same operation order
   (bit-identical). 395 → 349.

2. **Streaming inverse 9/7** (`dwt/stream97.rs`, new): the in-place transform did, per level,
   a horizontal sweep over every row and then a vertical sweep over every column, each lift
   touching the whole plane (ten plane passes per level, cache-hostile column strides). The
   streaming version synthesises each level in one pass: for output row *y* it pulls the
   needed LL/HL (even) or LH/HH (odd) rows, scales (K / 1/K), runs the four horizontal lifts
   in a padded scratch row, and applies the four vertical lifts inside an 8-row ring with a
   lag of 4 rows, mirroring at the top and bottom for whole-sample symmetric extension. Each
   level reads its inputs once and writes its output once; the ring stays in L1.
   `LevelSynth::next_row` exposes the final level row by row (used in step 4), and bands can
   start at any row (`first_row`) so the final level can still be split across threads when
   the decoder is allowed a pool. The old two-sweep code is kept under `#[cfg(test)]` as the
   reference. 349 → 279.

3. **Tier-1 on packed stripe-column flags** (`decode/t1.rs`, `tier1/stripe.rs`, `mq/mod.rs`):
   the EBCOT passes kept three separate byte grids (significance, visited, refined) plus a
   sign plane and a final sign-application loop over the block, with per-sample neighbour
   gathers of eight loads. They now use OpenJPEG's one-`u32`-per-column layout (four rows of
   a stripe per word: σ of the 3×6 neighbourhood, χ, μ, π bits), so a column's context for
   all four rows is one load and significance updates write the neighbours' words directly.
   Coefficients are built signed in place (sign folded in when a sample becomes significant,
   magnitude refinement adjusts the signed value), the dequantisation stays fused into the
   block output, and the full and partial stripes run unrolled four-row column helpers. The MQ
   decoder's `decode`, `bytein` and the exchange paths are `#[inline(always)]` and `renormd`
   shifts by `leading_zeros` in one step instead of a bit at a time. The old grid-based passes
   are kept verbatim as `decode/t1_reference.rs` (test only). 279 → 218.

4. **Fused final level and pack** (`reconstruct_irreversible_97_fused_pack`): for the common
   case (three real 8-bit colour planes, one tile, no subsampling, at least one level) the
   decoder runs levels 1..n−1 per component into quarter-size buffers, then synthesises the
   finest level of Y, Cb and Cr in lockstep and feeds each row triple straight from the
   synthesis rings through the ICT and the byte packer into the caller's buffer. The three
   full-resolution `f32` planes are never written or read back, which removes six plane-sized
   memory passes (about 150 MB per layer). Banded over the Rayon pool when the target is
   contiguous and a pool exists; serial otherwise. 218 → 182.

5. **AVX2 row kernels** (`Kernel::Avx2` in `simd/mod.rs`, `simd/x86`): the lift, scale and
   interleave rows of the streaming DWT and the packer were compute-bound on `wide`'s two
   128-bit halves (a baseline x86-64 build does not enable AVX). They now have
   `#[target_feature(enable = "avx2")]` variants, selected at runtime after feature detection
   through a `Kernel { Scalar, Wide, Avx2 }` flavour carried in `PRIMITIVES`; the packer's
   variant is written with intrinsics (eight pixels per step, byte shuffle for packed RGB).
   No FMA, same operation order, bit-identical. The selection does not depend on the `simd`
   cargo feature, because the renderer builds jp2lam with `default-features = false`; the
   AVX2 kernels only need `std::arch`. 182 → 166. (`JP2LAM_PRIMITIVES=wide|scalar` forces
   the other flavours for A/B runs.)

Not pursued: a decoder-held plane pool (measured at 1–3 ms per plane, since step 4 removed the
planes), a plain row-parallel final level for single images (whole-document runs are serial
by policy), and target-cpu flags in `.cargo/config.toml` (none exist in the workspace; the
runtime dispatch keeps the baseline build portable).

## Correctness checks

- `dwt::stream97` tests: every size 1..19 × 1..19 at 0–4 levels, larger sizes, and every
  band start row, compared bit-for-bit with the retained two-sweep reference, for every
  kernel flavour available on the host (`Kernel::available()`).
- `decode::t1` stripe-equivalence tests: blocks encoded with the in-tree encoder under eight
  code-block styles (default, vertical causal, reset, terminate-each-pass, bypass,
  segmentation symbols, all on, bypass+VSC+segmark), four coefficient flavours, 64×64 and
  128×128 plus eleven odd sizes, decoded at every pass prefix; the packed passes must return
  the same `Result` (coefficients or error) as the verbatim old implementation, and full
  decodes must equal the source coefficients.
- `decode::reconstruct::pack_kernel_tests`: scalar, `wide` and AVX2 packers over widths
  1–40, 300 and 1001, every layout (RGB, RGBA, RGBX, BGRA), with and without an alpha row,
  with and without the ICT.
- `cargo test --manifest-path lege-codecs/jp2lam/Cargo.toml`: 395 lib tests and the 12
  integration suites pass in release and debug, with default features and with
  `--no-default-features`.
- `jpxbench --check` / `check.sh`: all 20 corpus rasters at both levels hash identical to the
  pre-change decoder.

## Method

- Serial jobs pinned to the six P-cores (`cmd /c start /affinity FFF /b /wait …`), Rayon
  pinned to one thread; min and median over 7 iterations; sibling builds on the host add
  10–20 % noise to medians, so steps were judged on minima and on the jp2lam/hayro ratio of
  the same run, and the final tables were taken on an idle host.
- Adapter: `.agent/scratch/pdf-render-headsup-20261104/lege/` (main checkout; untouched)
  copied to the session scratchpad with its path dependencies pointed at this worktree's
  `lege-pdf/render/crates/*`, which pulls this worktree's jp2lam through the workspace
  `[patch]`. The "before" binary is the retained `headsup-lege-final.exe` built from the same
  renderer commit with the old jp2lam. `summarize_timing.py`-style medians over pages 1–99.
- hayro-jpeg2000 0.5.0 from crates.io (`simd` feature, `fearless_simd` runtime AVX2),
  decoded through `Image::new` + `DecoderContext` to interleaved RGB8 with the same
  `resolution_reduction`.

## Follow-ups

- **Segmentation symbols with truncated pass prefixes**: with `segmentation_symbols` on, both
  the old and the new Tier-1 fail identically on prefixes that end between cleanup passes,
  because the encoder's per-pass length bookkeeping does not cover the segmark bytes
  (`encode/backend/native/t1.rs`). Pre-existing, encoder side; the equivalence test compares
  the error too.
- **Tier-1 is the remaining margin** at reduced resolution (64 of 84 ms on cm p0 layer 0 at
  one level, ratio 0.86–0.98 against hayro on the dense text layers). The next levers are
  unchecked indexing in the pass kernels and a bypass-aware raw-pass fast path; both would
  need the same equivalence harness.
- **Structures end-to-end** is still behind hayro's 68 ms/page (88 after this work): the
  remaining per-page cost is outside JPX (JBIG2 mask, draw), see the sibling notes
  `optimization-image-blit-simd-20261104.md` and `optimization-text-vector-percore-20261104.md`.
- The renderer still builds jp2lam without the `simd` feature; the AVX2 kernels now land
  regardless, but the remaining `wide` kernels (5/3 reversible path, encoder) do not. Worth
  enabling `simd` in the root manifest once measured.
- Clippy on the new code adds two `manual_is_multiple_of` and one loop-counter lint, in line
  with the crate's existing style lints (62 before this change); none were fixed here.
