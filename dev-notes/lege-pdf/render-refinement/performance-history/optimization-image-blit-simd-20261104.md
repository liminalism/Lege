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
