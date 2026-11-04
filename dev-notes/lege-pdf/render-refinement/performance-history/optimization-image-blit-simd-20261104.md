# Axis-aligned image draws: SIMD bilinear, Indexed promotion, soft-masked area-min, AVX2 chroma upsampling — 2026-11-04

Follow-up to the hayro head-to-head: on a per-core basis Lege's axis-aligned
image draws were the one place hayro was clearly ahead. Three concrete pages
were the brief, each a different shape of the same problem:

| page | image | draw | baseline |
|---|---|---|---:|
| `sahib.pdf` p178 @1 | 1673×2734 RGB8 JPEG, drawn 1:1 | bilinear (DCT forces it) | 36.8 ms |
| `sahib.pdf` p178 @2 | same, 2× magnified | bilinear | 137.2 ms |
| `xenosystems.pdf` p0 @1 | 2297×3054 Indexed/Flate + 8-bit `/SMask` + `/Decode`, minified 3.9× | generic per-tap | 355.1 ms |

Targets were sahib @1 under 10 ms, sahib @2 under 50 ms, xenosystems @1 under
60 ms, byte-identical output. All three are met; every page hash in the
60-page verification corpus is unchanged.

## Measurement

Host i7-13700H (6 P-cores + 8 E-cores, 20 logical), Windows 11, rustc 1.97.1,
release profile. Both arms are the retained head-to-head adapter
(`headsup-lege-final.exe <pdf> <scale> 1 sample <outdir> <page>`) built from
the worktree: the **baseline binary is `5951f37a` unmodified**, the final
binary is this change. Env for every run:
`PDF_RENDERER_JPX_CONCURRENCY=serial PDF_RENDERER_IMAGE_ROW_PAR=0 RAYON_NUM_THREADS=1`,
and the process is **pinned to the P-cores** (`start /affinity FFF`) because
Windows parks a single-threaded process on an E-core otherwise, which halves
the numbers and makes them bimodal. Each invocation renders the page 26 times
(1 first, 5 warm-up, 20 warm); the arms were interleaved per configuration
over three rounds, so each cell below is 60 warm renders and 3 cold ones.

### Whole-page render, baseline binary vs final binary (ms)

| page | baseline first / warm min / warm med | final first / warm min / warm med | warm speedup | hash |
|---|---:|---:|---:|---|
| `sahib` p178 @1 | 75.4 / 36.8 / 41.2 | **39.4 / 9.1 / 9.9** | **4.0×** | `eabf1c38b6f7220f`, identical |
| `sahib` p178 @2 | 196.6 / 137.2 / 161.7 | **59.1 / 26.6 / 28.3** | **5.2×** | `cabb9a7ee0864668`, identical |
| `xenosystems` p0 @1 | 354.8 / 355.1 / 370.1 | **60.2 / 51.2 / 58.8** | **6.9×** | `1cd74d1730690951`, identical |

The "first" render includes the JPEG / Flate decode and the image-cache fill;
the warm numbers are the draw alone, which is what the brief measures.

### Verification corpus

`verify.py` renders eight documents (`Structures`, `beans`,
`centralparktest-repair`, `chinese-medicine`, `sahib`, `viaduct`, `vogtle`,
`xenosystems`) at pages 0 / 1 / mid / last (plus sahib p178) at scales 1 and 2
and hashes every surface: 60 rows. Final vs baseline: **60/60 identical** with
row fan-out off, and **60/60 identical** with `PDF_RENDERER_IMAGE_ROW_PAR=1`
(the new bilinear kernel runs in 32-row bands when fan-out is on, so the
banded and serial results had to be checked separately).

## Change 1 — separable bilinear with AVX2 kernels (`exec.rs`)

### What the baseline did

`paint_axis_aligned_rgb8_bilinear_opaque` evaluated the full two-axis lerp per
destination pixel (four texel fetches, eight multiplies, float round-trip) in a
hand-written SSE2 kernel that still walked the source one pixel at a time, with
the row/column tap tables as arrays of structs. At 1:1 it did exactly the same
work as at 2× — nothing recognised that the draw was a copy.

Two facts about the 1:1 case drove the design. The device→image inverse for
sahib is `a = 1/1673, e = -0`: the sample positions `u·w − 0.5` land on
`k + ε` or `k − ε`, so the fractional weight `t` is `0.0` on about half the
columns and `1 − 2⁻²⁴` on the rest, and **floor steps by 0 or 2 texels across
the row rather than by 1**. A true unit blit therefore only fires when every
`t` is exactly `0.0` (integer translations), which this page does not satisfy —
the fast path for it has to be the general kernel being fast on this input.

### What changed

- **Structure-of-arrays tap tables.** `bilinear_columns` and `bilinear_rows`
  produce `first_x` plus parallel `off0 / off1 / t / 1−t` vectors (column
  offsets are byte offsets into the row, ready for a gather).
- **Separable evaluation.** For each source row touched, `hlerp_row` writes a
  planar `f32` row (R plane, G plane, B plane, each `n` floats) holding the
  horizontal lerp of every destination column; `vlerp_row` then blends two
  cached planar rows with the vertical weight and packs premultiplied RGBA.
  Consecutive destination rows share one of their two source rows, so the
  two-slot `HLerpCache` halves the horizontal work; at 2× it quarters it.
- **AVX2 `hlerp_row`.** Eight columns per iteration. When the eight `off0`
  values are exactly `o, o+3, …, o+21` (checked lane-exact with
  `cmpeq_epi32` against a ramp — the "span equals 21" shortcut is wrong for
  the 0/2 step pattern above) the two texel rows are two unaligned loads and a
  byte shuffle per plane; otherwise two `i32gather_epi32` per plane. Either way
  the arithmetic is `mul, mul, add` in that order with no FMA, so it matches
  the scalar reference bit for bit.
- **AVX2 `vlerp_row`.** Eight pixels per iteration: `p0·(1−t) + p1·t`,
  `cvttps` (truncation, as the scalar path), clamp, pack `r | g<<8 | b<<16 | α`.
- **Unit blit.** When every column *and* row weight is exactly `0.0` the rows
  are copied through the 3→4 widen below. Fires for integer placements only.
- **Rayon bands.** With fan-out enabled the destination rows are split into
  32-row bands, each with its own cache; the serial path is one band.

Scalar versions of every kernel remain and are the fallback without AVX2; the
kernel is chosen once per draw by `image_kernel()`.

### 3→4 widen for the nearest / blit paths

`paint_axis_aligned_rgb8_nearest_opaque` now resolves the present columns to
one interval and a texel-index list, splits it into consecutive runs, and
widens each run RGB8→premultiplied-RGBA8 with `widen_rgb8_to_rgba8_opaque`
(AVX2: two 16-byte loads at `p` and `p+12`, one `shuffle_epi8`, OR the alpha
byte; scalar tail). The same widen serves the bilinear unit blit.

## Change 2 — Indexed images are promoted to RGB8 once (`image.rs`, `exec.rs`)

The generic path resolved every tap of a minified Indexed draw through
`pixel()` (bit-unpack, `/Decode`, palette, base colour space) — for
xenosystems that is 15 taps per output pixel over 500k output pixels.

`PreparedImage::indexed_promoted_to_rgb8` expands the palette once (bpc
2/4/8, MSB-first unpack, `/Decode` applied in index units through the same
`indexed_palette_index` that `pixel()` now uses, 64 Mpx cap) into an RGB8
`PreparedImage` with `decode = None` and no sample LUT. `paint_image` performs
the promotion for every Indexed image except colour-keyed ones, so all RGB8
fast paths (area-min, bilinear, nearest) and the generic path alike see RGB8.

## Change 3 — soft-masked area-min (`exec.rs`, `image.rs`)

The planned "unmasked Indexed" gate never fired: xenosystems' image carries a
full-resolution 8-bit `/SMask`, so even promoted it fell to the masked
generic path. `area_min_bilevel_mask` is now `area_min_mask` with a third
variant, `AreaMinMask::Soft8`, for an 8-bit smask of the image's own size.
The painter box-averages the colour as before and computes the coverage with
`gray8_smask_coverage`, which mirrors `sample_smask`'s multi-bit branch
exactly (row-major `f64` accumulation of `wy · wx · texel/255`, divided by the
weight sum, cast to `f32`, then `apply_smask_decode`), so the result is
byte-identical to the generic path.

Progression on xenosystems p0 @1: 355 ms → 110 ms (promotion alone) →
51 ms (promotion + `Soft8` area-min).

## Change 4 — AVX2 4:2:0 chroma upsampling (`pdf-image/src/jpeg/mod.rs`)

The brief asked for Lege's JPEG decode on the sahib stream to be measured
against zune-jpeg and optimised only if it was a larger share than the draw.
After changes 1–3 the draw is ~9 ms and the decode was ~24 ms, so it was.

`jpegbench` decodes the page's DCT stream (SOF0 baseline, 4:2:0, Y 2×2 /
Cb Cr 1×1, 524,123 bytes, no restart interval) 40× with each decoder, pinned,
idle machine, min / median:

| decoder | before | after |
|---|---:|---:|
| Lege `pdf_image::jpeg::decode_jpeg` | 23.95 / 26.33 ms | **15.14 / 17.24 ms** |
| zune-jpeg 0.5 | 10.91 / 11.94 ms | 11.25 / 12.57 ms |

Temporary stage timers (since removed; they inflate the total by ~7 ms)
attributed the baseline as entropy 7.1 ms, IDCT + store 4.6 ms,
upsample + colour 16.1 ms. The IDCT and the YCbCr→RGB conversion were already
AVX2; the 4:2:0 "fancy" triangle upsampling (`Upsample::FancyH2V2`) was scalar
and ran over two chroma planes at the full output width — the single largest
stage.

`upsample_h2v2_row` now dispatches to an AVX2 kernel that handles sixteen
source texels per iteration in `u16` lanes: the vertical blend `near·3 + far`
for the texel and both neighbours, then `(this·3 + prev + 8) >> 4` and
`(this·3 + next + 7) >> 4`, interleaved as `even | odd << 8` into 32 output
bytes. All intermediates fit `u16`, so it is the scalar expression exactly;
the left edge and the ragged tail run the scalar reference. After the change
the same timers read upsample + colour 6.7–7.4 ms; the remaining gap to zune
is the entropy decoder (7.5–8 ms), which is a separate piece of work.

`upsample_tests::h2v2_avx2_matches_scalar_bit_exact` fuzzes widths 1–39 and
63/64/65/100/127/128/129/837 at both even and odd output widths.

## Tests

- `pdf-render-cpu` (89 lib tests, all pass) gained in `rgb8_area_min_tests`:
  `indexed_fast_path_matches_generic_shade` (bpc 2/4/8 × Gray/RGB/CMYK
  palettes × with/without `/Decode`, unmasked and bilevel-masked),
  `bilinear_kernels_match_generic_shade_and_each_other` (nine placements:
  integer and fractional 1:1 offsets, non-integer magnification, 2×, partly
  off-surface, narrower than one vector group, 1×1 source; every kernel the
  host has), `nearest_fast_path_matches_generic_shade`,
  `widen_kernels_match_scalar_bit_exact`, `masked8_fast_path_matches_generic_shade`.
- `pdf-image` (51 lib tests + fixture suites, all pass) gained the H2V2 fuzz
  above.

## Not done / follow-ups

- The JPEG entropy decoder is now the largest decode stage on 4:2:0 scans
  (~8 of ~15 ms on sahib) and the whole remaining gap to zune-jpeg.
- `FancyH2V1` and `FancyH1V2` (4:2:2 / 4:4:0 scans) are still scalar; the
  same `u16` kernel shape applies.
- The Indexed promotion caps at 64 Mpx; larger Indexed images keep the old
  per-tap path.
- AKR: `akr start` fails on this worktree with AKR-C031 (neither working-tree
  nor HEAD knowledge validates, 10,996 errors), so no planning read or record
  update was possible for this change. Logged here as a papercut.

---

# Round 2 — 2026-11-04 (later the same day)

Follow-ups from the first round, in the order the orchestrating session set:
the JPEG entropy decoder, AVX2 for the two remaining chroma upsampling modes,
and the vogtle scanned-image pages the text/vector session flagged. Same
ground rules: byte-identical output, paired P-core-pinned warm timings, the
corpus hashes, both fan-out modes.

**Previous binary is `183f0501`** (the first-round result above); the new
binary is this round's tree. The machine carried other sessions' builds
during these runs, so absolute numbers sit ~15 % above the first-round
table (sahib @1 reads 10.5 ms for the *same* previous binary that read 9.1
earlier); the pairing is what counts.

### Whole-page render, previous binary vs new binary (warm min / median ms, 60 renders each, 3 interleaved rounds)

| page | previous | new | speedup | hash |
|---|---:|---:|---:|---|
| `vogtle` p14 @1 | 24.2 / 27.8 | **4.9 / 5.6** | **4.9×** | identical |
| `vogtle` p14 @2 (144 DPI) | 95.5 / 104.2 | **11.3 / 13.5** | **8.5×** | identical |
| `vogtle` p16 @1 | 9.3 / 11.7 | **4.0 / 4.8** | **2.3×** | identical |
| `vogtle` p16 @2 | 28.1 / 31.9 | **6.3 / 7.4** | **4.5×** | identical |
| `vogtle` p6 @2 | 8.9 / 10.8 | 9.1 / 10.9 | 1.0× | identical |
| `vogtle` p0 @2 | 18.2 / 20.9 | 18.3 / 21.0 | 1.0× | identical |
| `sahib` p178 @1 | 10.5 / 12.1 | 10.9 / 12.1 | 1.0× | identical |
| `sahib` p178 @2 | 31.7 / 36.3 | 32.3 / 38.1 | 1.0× | identical |
| `xenosystems` p0 @1 | 62.1 / 71.1 | 62.8 / 71.3 | 1.0× | identical |

vogtle p14 at 144 DPI was 68 ms of image draw against hayro's 11.2 ms in
the text session's measurement; it is now at parity. The pages whose image is
a plain RGB JPEG (p6, p0) and the first-round targets are unchanged on the
warm draw, as expected — their change is in the decode, which the "first"
render carries: sahib @1 first render 51.9 → 46.5 ms.

Corpus: 60/60 hashes equal to the `183f0501` binary with row fan-out off,
and 60/60 with `PDF_RENDERER_IMAGE_ROW_PAR=1`.

## Change 5 — vogtle's scans are `/ICCBased` RGB: resolve once, cache by content (`image.rs`, `prepared.rs`)

The flagged images are not an exotic draw path but an exotic *colour space*:
`ImageColorSpace::IccRgb` (a non-sRGB profile with a 256-point TRC per
channel and a profile→XYZ matrix). None of the RGB8 fast paths accept it, so
every tap of every draw went through `pixel()` → `to_srgb_with` — three curve
samples, two 3×3 matrices and the sampled sRGB transfer — ~60 ns per source
texel per draw, 1317×882 texels, every render.

Each texel's result is a pure function of its own samples, so the image is
now resolved to RGB8 **once** and the executor's existing RGB8 paths (area-min,
bilinear, nearest, the soft-masked variants) take it from there:

- `PreparedImage::generic_promotion_eligible` names the family: `IccRgb`,
  `IccCmyk`, `Lab`, `TintLut`, `TintLut2`; 8- or 16-bit samples; not a
  stencil; no colour-key mask (it tests the *raw* samples); no prepared LUT;
  complete sample data; ≤ 64 Mpx.
- `generic_source_as_rgb8` resolves every texel. The 8-bit ICC-RGB shape
  without `/Decode` calls `to_srgb_with` on the byte triple directly — the
  identical computation `pixel()` performs once its generic bit reads and the
  absent `/Decode` reduce to `byte / 255` — and every other shape goes through
  `pixel()` itself, so identity is by construction.
- `prepared.rs::promote_through_cache` runs after `lower_image` builds the
  `PreparedImage` and keeps the resolved RGB8 in the production
  `SharedImageCache`, keyed by the 128-bit content hash of the *source
  samples* plus a hash of everything that shapes a texel's result (the
  profile's tables and matrix, the CMYK transform's tables and CLUT, Lab's
  white point and range, the tint tables, the `/Decode` array), under a
  sentinel `codec` byte so it can never alias a real decode. The first render
  pays the conversion (p14 @2 first render 119 → 37 ms, since the one-shot
  loop is also cheaper than per-tap resolution); every later draw of the same
  bytes — this render, the next, the other scale — is a cache hit and an RGB8
  draw. Hashing the 3.5 MB of p14's samples costs ~0.5 ms per draw; that is
  the price of an exact key with no reliance on object identity across
  documents.

Why this is byte-identical: the generic box average sums `pixel()`'s RGB per
tap with integer weights; the RGB8 area-min path sums the stored bytes with
the same weights; the bilinear and nearest samplers call `pixel()`, which on
an RGB8 image returns the stored bytes. `exec::rgb8_area_min_tests::
generic_promotion_matches_generic_shade` checks all four spaces (IccRgb with
random curves and matrix, Lab, both tint tables), with and without `/Decode`,
promoted-through-generic and promoted-through-fast against the original
through generic.

## Change 6 — JPEG decoder: colour stores, lookahead tables, refill (`pdf-image/src/jpeg/mod.rs`)

`jpegbench` on the sahib p178 stream, 40 decodes each, four interleaved
rounds of the `183f0501` decoder (B) against this round's (N), both pinned,
with zune-jpeg 0.5 on the same stream in the same process for reference
(min / median ms):

| round | B: Lege | N: Lege | zune (same run) |
|---|---:|---:|---:|
| 1 | 18.39 / 20.77 | **14.95 / 17.09** | 13.1 / 15.3 |
| 2 | 17.95 / 20.84 | **14.66 / 16.90** | 12.1–13.3 / 14.2–15.0 |
| 3 | 17.16 / 19.68 | **13.47 / 15.82** | 11.9–12.2 / 14.0–14.5 |
| 4 | 18.28 / 21.56 | **14.37 / 16.54** | 11.8–12.6 / 13.7–15.4 |

Lege's decode drops by ~3.5 ms (≈20 %); the gap to zune-jpeg goes from
~5.5 ms to ~1.8 ms, about two thirds closed. (The machine was loaded; on the
idle first-round host the same B binary read 15.1 ms and zune 11.3.)

What the stages actually cost was established with two temporary env-gated
skips (removed before commit) that left only the entropy loop running, or
entropy + IDCT: **entropy ≈ 7.3 ms, IDCT ≈ 3 ms, upsample + colour + assemble
≈ 7–8 ms** on the B decoder. The first-round attribution (entropy 7, colour
16) had been inflated by per-block timers; the colour stage, not entropy,
was the larger of the two after the H2V2 kernel, and it was the easier win:

1. **Colour stores** (−2 ms). `rgb_ycc_row_avx2` computed eight pixels in
   AVX2 and then wrote the 24 output bytes with 24 bounds-checked scalar
   stores. The three eight-byte channel runs are now interleaved with two
   `pshufb` pairs and written with one 16-byte and one 8-byte store. The lane
   values are untouched, and `rgb_simd_tests::avx2_ycc_rgb_is_exhaustively_
   byte_exact` still proves every (Y, Cb, Cr) triple against the scalar path.
2. **Lookahead tables** (−0.3 ms). `HuffTable` now peeks 10 bits instead of
   8 (`HUFF_LOOKAHEAD`), and carries a combined `fast_ac` table: for every
   prefix whose code *and* magnitude bits fit the lookahead, the fully
   decoded `(run, coefficient, total length)`, built with the same `extend`
   the symbol path applies. Most AC coefficients of a photographic scan are
   resolved from one peek with no second bit extraction. `extend` is
   branch-free.
3. **Refill** (−0.5 ms). The per-symbol `fill()` asked "are ≤ 56 bits
   buffered?" — a branch the entropy stream makes unpredictable. `top_up()`
   reads the next eight bytes and, when they hold no `0xFF`, inserts as many
   whole bytes as fit unconditionally (zero when the buffer is full); only
   stuffing, markers and the input tail reach `fill`, the unchanged exact
   arbiter. After a coefficient resolved through `fast_ac` (≤ 10 bits), the
   next coefficient is decoded without another top-up (≥ 47 bits remain,
   enough for the worst case of 16 + 15).

The remaining entropy cost (~6 ms) is the serial dependency of a table-driven
decoder — peek, load, shift — at ~5 ns per symbol; zune-jpeg's is of the same
order, and the residual gap to it is spread over the IDCT and the colour
stage rather than in any one place.

`entropy_tests::lookahead_decoder_matches_bit_serial_reference` decodes
random streams (stuffed bytes, embedded markers, truncation) with 400
random Kraft-valid table pairs through the production decoder and through a
bit-serial T.81 F.2.2 reference written without any lookahead, and requires
identical coefficients, DC predictions, `any_ac` flags and bit positions.
`fast_ac_covers_exactly_the_short_pairs` pins the table's coverage.

## Change 7 — AVX2 for the 4:2:2 and 4:4:0 chroma modes (`jpeg/mod.rs`)

`FancyH2V1` (horizontal triangle, biases 1/2, shift 2) and `FancyH1V2`
(vertical blend with the row-parity bias) now dispatch to AVX2 kernels of the
same shape as the first round's H2V2 one — sixteen interior texels per
iteration in `u16` lanes for H2V1, thirty-two texels per iteration for H1V2
(two `u16` halves packed back with the saturating pack, exact since every
value is ≤ 255) — with the scalar references kept for the edges, the tails
and non-AVX2 hosts. `upsample_tests::h2v1_avx2_matches_scalar_bit_exact` and
`h1v2_avx2_matches_scalar_bit_exact` fuzz every width class on both row
parities. No corpus page uses either mode at a size that shows in a table,
so no timing is claimed for this change.

## Not done / follow-ups

- **Indexed promotion cap (item 4, not changed).** The 64 Mpx cap bounds the
  per-draw transient at 192 MiB of RGB8, the same ceiling the Gray and CMYK
  promotions use; above it the generic per-tap path, whose work is bounded
  by the *device* footprint rather than the source area, is the right
  choice for a minified draw. A budgeted decision would need the
  renderer's memory budget plumbed into `paint_image`, which neither
  promotion has today. What would pay off sooner is moving the Indexed
  expansion behind the same content-keyed cache as Change 5 — xenosystems
  re-expands its 7 Mpx palette image on every draw (≈ 10 of its 62 ms).
- JPEG: the IDCT (f32 AVX2, ~3 ms) and the colour stage's f32 arithmetic
  are the next decode costs; an integer AVX2 IDCT would not be byte-identical
  to the current one and needs the oracle comparison rather than a hash.
- The in-data (`/SMaskInData`) and colour-keyed images stay on the generic
  path by design.
- AKR remains unusable on this worktree (AKR-C031); no record was written.

---

# Round 3 (2026-11-04)

Two levers from the round-2 follow-ups: the Indexed expansion behind the
content-keyed image cache (xenosystems), and the masked area-minification
draw on the Structures MRC pages, which the jp2lam note's round-2 profile
named as a missed fast path. The JPEG Huffman residual (item 3 of the brief)
was not started.

**Previous binary** is the retained `headsup-lege-final.exe` built at
`035660da` (identical renderer code to `9c93b580`). The new binaries are the
same adapter rebuilt from this branch: `c1` = commit 1 alone (Indexed
promotion), `new` = both commits.

## Method

Same as round 2: `headsup-lege-final.exe <pdf> <scale> 1 sample <out> <pages>`
(1 first render, 5 warm-up, 20 warm per invocation), process pinned to the
P-cores (logical CPUs 0–11, psutil affinity inherited by the child),
`RAYON_NUM_THREADS=1 PDF_RENDERER_JPX_CONCURRENCY=serial
PDF_RENDERER_IMAGE_ROW_PAR=0`, arms interleaved over three rounds, so each
cell is 60 warm renders. Two other workers were building and benchmarking
on the host throughout; the CPU load sampled after each configuration was
10–42 % for the table below and up to ~100 % during development, so steps
were judged on paired minima only. Stage splits come from the same adapter
built with `pdf-render-cpu/profiling` and a `profile` mode that renders a
page N times through `render_profiled_with`; this round adds
`render.image.area_min`, `render.image.area_min_masked` (per-path draw
time), `image.area_min_masked.{soft1,soft8,stencil}_draws` and
`render.image.indexed_promotion` to the profile.

## Paired results (warm min / median ms, whole page)

| page | previous | c1 (Indexed cache only) | new (both) | speedup | hashes |
|---|---:|---:|---:|---:|---|
| `xenosystems` p0 @2 (144 DPI) | 88.2 / 98.4 | 82.7 / 110.5 | **46.4 / 53.4** | **1.90×** | identical |
| `xenosystems` p0 @1 | 54.2 / 60.6 | 48.9 / 55.0 | **32.6 / 36.1** | **1.66×** | identical |
| `Structures` p0 @2 | 56.1 / 64.1 | 54.8 / 63.5 | **20.8 / 25.9** | **2.70×** | identical |
| `Structures` p1 @2 | 52.5 / 61.3 | 52.5 / 59.6 | **15.0 / 19.4** | **3.50×** | identical |
| `Structures` p0 @1 | 17.9 / 20.4 | 18.1 / 21.6 | **6.3 / 7.2** | **2.86×** | identical |
| `Structures` p1 @1 | 17.2 / 19.8 | 17.9 / 20.1 | **4.4 / 5.2** | **3.94×** | identical |
| `sahib` p178 @1 (control) | 10.4 / 12.3 | 10.2 / 12.0 | 10.2 / 11.6 | 1.0× | identical |
| `sahib` p178 @2 (control) | 31.0 / 35.8 | 31.3 / 36.2 | 31.0 / 35.9 | 1.0× | identical |

Structures' "first" render (JBIG2 + two JPX decodes) is unchanged in
substance (p0 @2 210 → 186 ms, of which ~35 ms is the draw).

Profiled image-draw stage, `render.image`, 27 warm renders, load 11–14 %:

| page @2 | previous | new | of which opaque area-min / masked area-min (new) |
|---|---:|---:|---|
| `Structures` p0 | 48.9 / 57.2 | **17.0 / 20.9** | 6.8 / 9.9 (was 14.6 / 34.0) |
| `Structures` p1 | 47.0 / 51.5 | **12.2 / 14.1** | 6.4 / 5.3 (was 14.7 / 30.6) |
| `Structures` p100 | 31.0 / 36.9 | **9.7 / 10.9** | — / 8.9 (was 30.1) |
| `xenosystems` p0 | 56.9 / 62.4 | **16.4 / 22.1** | — / 16.4 (incl. 8–9 ms Indexed expansion before) |

The Structures target (image draw from 42–63 ms to at most 20 ms per page)
is met on minima on all three pages and on medians for p1 and p100; p0's
median under load is 20.9 ms. The xenosystems target (p0 warm at 144 DPI at
least 15 % faster) is met by the pair (1.90×); commit 1 on its own gives
6 % (min) at 144 DPI and 10 % at 72 DPI — see below.

## Diagnosis: the Structures mask is a one-bit `/SMask`, not a stencil

The brief (and the jp2lam note) read `image.fast_rgb8_area_min_masked_pixels`
≈ 80–210 k of 1.1 M as "the masked draw misses the fast path". That counter
is incremented by the number of *painted* pixels; the draw itself was on
the fast path all along (`image.area_min_masked.soft1_draws = 1` per page:
the JBIG2 layer is a one-bit `/SMask`, `AreaMinMask::Soft`). It was simply
slow: 31–34 ms for 1.1 M destination pixels, ~30 ns each, almost all of it
in the per-pixel `bilevel_smask_coverage` (bit-by-bit edge reads, a
popcount loop and an f32 division for every destination pixel, transparent
or not), and the unmasked background draw took another ~15 ms (~13 ns per
pixel in the per-tap box filter). No stencil-specific painter was needed;
the `/Mask` stencil variant shares the rewritten body and gets the same row
filter.

## Change 1 — Indexed expansion behind the image cache (`prepared.rs`, `image.rs`)

`promote_through_cache` (round 2's ICC/Lab/tint promotion) now also takes
Indexed images: lowering expands them with the unchanged
`PreparedImage::indexed_source_as_rgb8` and keeps the RGB8 payload in the
shared image cache. The key is the 128-bit hash of the source samples plus
the palette identity (base space with its tables, recursively; `hival`; the
lookup bytes) and the `/Decode` array, under the promotion sentinel codec
byte; `bpc` is in the key, so 2/4/8-bit packings of the same bytes cannot
alias. The lowered image is exactly what `paint_image` produced at draw
time (RGB8, `decode = None`, no LUT), so the draw is unchanged;
`paint_image`'s own expansion stays as the path for images not built by
lowering (tests). Eligibility moved into `indexed_promotion_eligible`
(2/4/8 bpc, not a stencil, complete samples, ≤ 64 Mpx, no colour key) so
the hash is paid only for images that will be expanded.

What it buys is smaller than the round-2 estimate: the expansion was 8–9 ms
of xenosystems p0's draw at 144 DPI, and the warm path now pays a 7 MB
content hash instead (`render.lower` 0.06 → 0.97 ms), so the net is ~6 %
of the page at 144 DPI and ~10 % at 72 DPI. The rest of the xenosystems
gain is change 2: its cover is drawn through the eight-bit soft-mask
path, which was 57 ms of the 66 ms draw.

**The 64 Mpx cap stays.** With the cache in front, residency is already a
budgeted decision: `SharedImageCache::insert` refuses an entry larger than
its 96 MiB budget, so an expansion above 32 Mpx is computed per draw and
never retained, and one between ~10 and 32 Mpx can evict most of the
working set — the same policy every cached decode has. The cap now only
bounds the per-draw transient (192 MiB) for images the cache would not
keep anyway; above it the per-tap path's work is bounded by the device
footprint, not the source. Lifting it would need the renderer's memory
budget plumbed into lowering, which no promotion has.

## Change 2 — masked and opaque area-min (`exec.rs`)

Three exact accelerations, shared by `paint_axis_aligned_rgb8_area_min_masked`
(all three mask kinds) and `area_min_box_average_opaque` (RGB8 and the
converted CMYK):

1. **Uniform mask boxes (`MaskRowSummary`).** Per destination row, the OR
   and AND of the soft-mask rows its boxes span (packed bits or bytes,
   padded for 64-bit window reads). A box whose texels are all clear has
   coverage exactly `0.0` in both filters; one whose texels are all set has
   exactly `1.0` — at one bit the weighted ones equal `tx.total · ty.total`
   (every tap weighs ≥ 1), at eight bits the f64 accumulation sums integral
   products `wy · wx · 1.0` exactly (< 2⁵³) and divides the total by
   itself. So `alpha_clear` / `alpha_set` are computed once per draw with
   the generic `apply_smask_decode` and served without the filter. When
   every body box of a row is clear and clear is transparent (no inverting
   `/Decode`), the row paints only its border band.
2. **Windowed one-bit coverage.** A mixed box reads each mask row's taps as
   one 64-bit big-endian window: `w_lo·first + w_hi·last + S·popcount(interior)`,
   the same saturating row-weighted sum and the same f32 division as
   `bilevel_smask_coverage` (which still serves boxes wider than 57 bits).
3. **Row box filter (`RowBoxFilter`).** Every opaque row, and every masked
   row with at least half its body visible, is box-averaged a whole row at
   once: each source row's horizontal sums `Σ wx·texel` are computed once
   per band (straight-line code for boxes of ≤ 4 texels, the tap loop
   otherwise) and cached in a 17-slot LRU; a destination row is the
   row-weighted sum of its cached rows and one rounding division per
   channel over planar arrays. The arithmetic is f64 but every value is an
   integer below 2⁴⁹ (the constructor requires `tx.total · ty.total < 2⁴⁰`),
   so sums are exact and `trunc((acc + w/2) / w)` is the integer
   `(acc + ⌊w/2⌋) / w`: a correctly rounded quotient below 256 is within
   2⁻⁴⁶ of the true one, and a non-integer quotient is at least
   `1/w > 2⁻⁴⁰` from the next integer (for odd `w` the half-integer
   numerator cannot reach a multiple of `w` either). The finish (multiply,
   add, divide, truncate, pack four RGBA pixels) has an AVX2 kernel behind
   `image_kernel()` and a scalar reference; the accumulate loop is the same
   safe code compiled for AVX2. Sparse masked rows keep the per-pixel
   `rgb8_box_average` (the original tap loop, factored out).

Masked rows are now two passes — alphas for the body, then colour and
composite for the visible pixels — and both painters work in 32-row bands
under row fan-out (one filter cache per band). The body is the single run
of interior columns (`u` is monotonic in `x`), kept as compact arrays
rather than the 80-byte column tuples.

## Tried and dropped

- **Reciprocal division in the per-pixel filter.** One f64 reciprocal per
  pixel with a ±1 correction instead of three u64 divisions: slower
  (Structures background 24 → 32 ms) — first because `u64 ↔ f64` has no
  single x86-64 instruction (fixed with `i64` casts, still slower), then
  three true `divsd` measured the same as three `div`. The division was
  not the bottleneck of the per-pixel loop; the tap loop and its overhead
  were.
- **Regrouping the per-pixel tap loop** as `w_lo·first + w_hi·last +
  S·Σ interior` per row: exact, but ~10 % slower than the original zip
  loop, so the original was kept for the per-pixel path; the regrouping
  survives only as the fixed-tap horizontal pass of the row filter.
- **Lower dense threshold** (⅛ of the body instead of ½) and a
  prepared-offset per-pixel path for sparse rows: no measurable change
  under the load at the time; dropped to keep one per-pixel path.

## Verification

- Hashes (`verify` mode, 6 workers, `RAYON_NUM_THREADS=6`), this branch vs
  the previous binary, xenosystems (305 p), Structures (632 p) and sahib
  (356 p) at scale 1 and 2, with `PDF_RENDERER_IMAGE_ROW_PAR=0` and `=1`
  (both binaries run in each mode): **5172 / 5172 pages identical**.
- New unit tests, all passing: `exec::rgb8_area_min_tests::
  masked_uniform_boxes_match_generic_shade` (blocky and blank-line masks at
  1 and 8 bits, both `/Decode` polarities, masks up to 8× wider than the
  base, boxes wider than one window, against the generic per-pixel body),
  `windowed_mask_and_box_sum_match_per_tap_reference` (4000 random boxes,
  every bit phase, truncated mask buffers), `row_box_filter_matches_per_pixel_box_average`
  (both kernels, boxes of 1 to ~25 texels, rows revisited out of order,
  boxes taller than the filter serves, near-white sums), and
  `prepared::image_cache_tests::indexed_promotion_is_cached_by_content_and_renders_identically`
  (cold/warm/uncached lowering, hit sharing, palette and `/Decode` in the
  key, equality with the draw-time expansion). The existing masked,
  masked8, opaque, CMYK, Indexed and generic-promotion fast-path tests
  pass unchanged.

## Not done / follow-ups

- The JPEG Huffman residual (brief item 3) was not started.
- Structures p0 @2 median (20.9 ms under load) sits on the 20 ms line; what
  remains is pass 1 of the masked draw (~5–6 ms of per-pixel alpha
  classification on rows with ink) and the row filter's horizontal pass
  (~4 ms for the 1.1 Mpx background). An AVX2 horizontal pass (three
  gathers per eight columns) and a run-skipping pass 1 are the next levers.
- The warm Indexed path hashes its source samples every draw (~1 ms for
  7 MB); a cheaper identity would need the IR to carry one.
- AKR was not used (worktree; the ledger does not validate here).
