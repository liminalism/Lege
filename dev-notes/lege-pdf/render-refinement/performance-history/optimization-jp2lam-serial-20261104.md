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

## Round 2 (2026-11-04, later): segmentation symbols, Tier-1 indexing, `simd`, Structures profile

Same branch, second commit. The host ran an Android Studio index at ~40 % load for the whole
round, so the absolute numbers here are 20–25 % above the quiet tables; every comparison is
interleaved (A, B, C, A, B, C, …) and judged on minima and on arms measured in the same minutes.
Raw numbers: `lege-misc/dev-misc/jpxbench/results/tier1-bounds-and-simd-20261104.txt`.

### 1. Segmentation symbols: pass lengths now cover the coder's pending bytes

`encode/backend/native/t1.rs` (`encode_codeblock_styled`): at a pass boundary inside an open
codeword segment the MQ coder still holds the last symbols in its registers and may still carry
into the byte at the write position, so a pass's byte count taken as `numbytes()` is up to three
bytes short of what a decoder needs to reproduce that pass. Without segmentation symbols this
only blurs the last few symbols of a truncated layer; with them the decoder *checks* the marker
after every cleanup pass that is not the stream's last, so a layer ending one or two passes
after a cleanup pass failed with "segmentation symbol mismatch". Non-terminated passes now
snapshot `numbytes() + 3` (MQ) or `+ 2` (raw/bypass), capped at the segment length by
`close_segment` (OpenJPEG applies the same correction). The segment's final pass still takes
the exact remainder, so totals and terminated-pass lengths are unchanged.

Test `decode::t1::stripe_equivalence_tests::segmentation_symbol_prefixes_decode_at_every_pass`:
five styles with segmentation symbols (plain, +reset, +vertical causal, +bypass, bypass+VSC+
reset), four coefficient flavours, 64×64 / 128×128 / 61×59 / 5×7 blocks, every pass prefix
(over a thousand prefixes) must decode without error and the full prefix must reproduce the
source coefficients. It failed on the first prefix before the fix.

Not changed: the default single-segment path (`encode_codeblock_single_term`) keeps its exact
snapshots. It carries no marker to trip over, and widening its lengths would change every
multi-layer stream's packet headers; whether layer-truncated decodes gain from the same +3 is a
separate (rate-control) question.

### 2. Tier-1 indexing: slices of visible length instead of a padded index

There was no unchecked indexing to replace: the round-1 kernels index the padded flag grid and
the sample plane with plain `[]` (checked). The question was what those checks cost. Three
arms, interleaved on the two reference layers, min ms over 2×15 and 4×9 iterations:

| layer | lvl | A: round-1 kernels | B: `get_unchecked` everywhere (experiment) | C: `StripeRows` (committed) |
|---|---:|---:|---:|---:|
| cm p0 layer 0 | 0 | 191.6 / 214.7 | 196.9 / 205.6 | 205.7 / 206.0 |
| | 1 | 102.3 / 106.1 | 104.7 / 100.7 | 103.5 / 104.4 |
| structures p0 layer 0 | 0 | 126.8 / 107.2 | 110.5 / 115.4 | 106.7 / 120.8 |
| | 1 | 72.9 / 67.6 | 69.6 / 70.2 | 65.3 / 67.3 |

No arm is consistently ahead; the spread between arms (±5 %) is smaller than the spread between
rounds of the same arm. Bounds checks are not a measurable cost in this kernel: the MQ decoder's
serial dependency chain is, and the checks sit in its shadow. The committed form is C
(`decode/t1.rs`, `StripeRows`): each stripe hands the kernels three flag rows of exactly
`width + 2` words (the stripe above, the stripe, the stripe below, from `split_at_mut`) and four
sample rows of exactly `width`, so column `x < width` and its neighbours index slices whose
length the compiler can see, and the manual `idx`/`stride` arithmetic of round 1 is gone. Hashes
20/20 identical; the packed-vs-reference equivalence tests cover it.

### 3. `simd` feature for the renderer build: neutral, left off

Root `Cargo.toml` line 148 (`jp2lam = { path = …, default-features = false }`) was given
`features = ["simd"]` and the adapter rebuilt; the micro-benchmark compared the bench crate's
jp2lam with and without the feature (3×9 iterations), the adapter the two renderer builds on
pages 0–99 (two interleaved rounds each):

| | without `simd` | with `simd` |
|---|---:|---:|
| cm p0 layer 0, full / one level (min ms) | 186.2 / 93.6 | 200.2 / 101.7 |
| structures p0 layer 0, full / one level | 99.7 / 62.8 | 109.1 / 66.8 |
| adapter chinese-medicine, median ms/page | 158.4, 157.4 | 164.3, 154.1 |
| adapter Structures | 115.2, 116.5 | 117.9, 115.0 |

Expected: with the AVX2 kernels selected independently of the feature, the only `wide` code the
decode could still reach is the 5/3 path and the encoder, neither of which these streams use.
The feature adds the `wide` dependency to the renderer build for no measured gain (the
micro-benchmark minima are nominally worse with it, inside the noise), so the line is reverted
to its committed form and the renderer keeps building jp2lam without `simd`.

### 4. Structures: where a page goes now (profile, not fixed)

Adapter built with `pdf-render-cpu/profiling` (`render_profiled_with`), serial, P-cores, 144 DPI,
loaded host; the first visit decodes, later visits draw with the decoded images cached
(`codec.*` keys are per decode, `render.image` is the image draw time, `jpx-trace` the two
layers):

| Structures page, ms | JBIG2 decode | JPX decode (2 layers) | lower (decode + prepare) | image draw | first visit | cached visit |
|---|---:|---:|---:|---:|---:|---:|
| p1 (2157×3189 layers → 1079×1595, mask 2157×3189) | 41.4 | 48.4 (24.8 + 23.0) | 89.9 | 52.8–62.5 | 145 | 52–63 |
| p100 (698×1018 + 2096×3056 layers, mask 2096×3056) | 36.0 | 23.2 | 59.3 | 41.9–54.5 | 106 | 42–55 |

Scaled to the quiet host (×0.8) that is roughly JPX 20–40, JBIG2 30–33, draw 35–45 ms of the
88 ms/page. Two things stand out:

- **The JBIG2 mask decode costs as much as both JPX layers.** 36–41 ms for a 2157×3189 bilevel
  generic region is ~6 ns per pixel; hayro renders the whole page in 62 ms. That is the next
  codec target (`pdf-image/src/jbig2.rs` / `jbig2enc-rust` decode).
- **The masked foreground draw misses the fast path.** Each page has two image draws: the
  background layer (`image.fast_rgb8_area_min_pixels` ≈ 1.1 M, the unmasked area-minification
  fast path) and the foreground layer through the JBIG2 stencil
  (`image.resource_masked_draws = 1`), of which only 80–140 k pixels went through
  `fast_rgb8_area_min_masked`. The two draws together take 43–62 ms for 2.2 M destination
  pixels (~25 ns/px), so the masked draw is running the generic per-tap sampler for most of
  the page. The image-blit branch (`optimization-image-blit-simd-20261104.md`) added a soft-mask
  area-min variant; a stencil-mask variant of the same shape is the draw-side fix.

### Verification (round 2)

`cargo test --manifest-path lege-codecs/jp2lam/Cargo.toml` in release and debug, and
`--no-default-features` in release; `check.sh`: 20/20 raster hashes identical to the round-1
baseline; clippy adds no new lint classes (the usual `too_many_arguments`).
