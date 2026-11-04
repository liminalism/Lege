# JBIG2 generic-region decode — 2026-11-04

The round-2 Structures profile (`optimization-jp2lam-serial-20261104.md`, section 4)
put the JBIG2 foreground mask at 36–41 ms per page at 144 DPI, serial: about 6 ns
per pixel of generic-region decode, as much as both JPX layers together. Structures
is the document where Lege trailed hayro most (0.74 serial). Goal: the page-0 mask
decode at or under 15 ms serially, bit-identical output.

## Result

Micro-benchmark, `jbig2enc-rust/examples/generic_decode_timing.rs`: one reused
`DecoderContext`, `decode_embedded_with_context` (the call the renderer adapter
makes), release, P-core pinned (`start /affinity FFF`), old and new binaries
interleaved over 3 rounds of 20 decodes, min / median over all rounds. Host load
3–8% during this run. Streams are the raw `/JBIG2Decode` bytes extracted from the
PDFs (`.agent/scratch/jbig2-round3/extract.py`); output hashes identical for every
stream.

| stream | size, % set | before ms (ns/px) | after ms (ns/px) | min ratio |
|---|---|---:|---:|---:|
| Structures p0 mask | 2222×3191, 85% | 28.8 / 32.2 (4.06 / 4.55) | 7.75 / 9.31 (1.09 / 1.31) | 3.7× |
| Structures p1 mask | 2157×3189, 95% | 24.1 / 28.6 (3.50 / 4.15) | 4.60 / 5.36 (0.67 / 0.78) | 5.2× |
| Structures 3rd mask | 2202×3082, 98% | 22.7 / 26.6 (3.35 / 3.93) | 2.73 / 3.37 (0.40 / 0.50) | 8.3× |
| Structures 4th mask | 2136×3048, 99% | 22.6 / 25.7 (3.48 / 3.94) | 2.02 / 2.46 (0.31 / 0.38) | 11× |
| centralpark first stream (symbol dict + text region) | 3403×3380, 8% | 16.1 / 19.1 | 9.32 / 10.2 | 1.7× |
| chinese-medicine 2nd stream (symbol dict + text region) | 2364×4162, 2% | 2.40 / 2.72 | 1.96 / 2.13 | 1.2× |

(The first loaded-host run of the old binary measured 47.9 ms for the p0 mask, which
is where the 36–41 ms of the profile sits; the quiet-host baseline is 28.8 ms.)

Page level, timing adapter (`headsup-lege-final`, built from this branch in
`.agent/scratch/headsup-round3/`) against the retained 035660da adapter, `sample`
mode with `PDF_RENDERER_IMAGECACHE=off` so every render decodes the mask and both
JPX layers, harness env (`RAYON_NUM_THREADS=1 PDF_RENDERER_JPX_CONCURRENCY=serial
PDF_RENDERER_IMAGE_ROW_PAR=0`), pinned, 3 interleaved rounds × 25 warm samples,
host load ~5%:

| Structures @144 DPI | before min / median ms | after min / median ms | saved (min) |
|---|---:|---:|---:|
| page 0 | 150.0 / 162.6 | 123.2 / 138.3 | 26.8 |
| page 1 | 107.7 / 118.5 | 82.6 / 89.6 | 25.1 |
| page 100 | 81.8 / 91.8 | 57.0 / 67.5 | 24.8 |

The whole-document `verify` runs (below) were taken on a host loaded to ~100% by
other builds and are only correctness evidence; their timings are not comparable.

## What changed

### 1. One windowed decoder for templates 0–3 (`decode/generic.rs`)

The old decoders formed every context per pixel: the nominal template-0 path rolled
three small windows but read each incoming pixel with a bounds-checked `sample()`,
and templates 1–3 and non-nominal template 0 gathered all 10–16 bits per pixel.

Every template's context (in the T.88 / jbig2dec numbering the decoder already used)
is three contiguous windows: row `y-2` above row `y-1` above the current row, MSB =
leftmost pixel, and with the nominal AT positions the AT pixels are window members
(T0 bits 4/10/11/15, T1 bit 3, T2 bit 2, T3 bit 4). So moving one pixel right is
`ctx = ((ctx << 1) & keep) | in1 << p1_lsb | in2 << p2_lsb | bit`, the encoder's
form, generalised by a `Geometry` table per template. The incoming bits come from
two 64-bit look-ahead registers loaded once per 32-pixel block (padding masked), so
the pixel loop has no row indexing or bounds checks. `decode_windowed::<T, GENERAL>`
is instantiated per template; `GENERAL` (non-nominal AT) masks the AT slots out of
the window context and reads each AT pixel from its own per-block window (rows above)
or a 128-bit current-row history (`Ay == 0`), with the old semantics for non-causal
positions (0).

### 2. Uniform-run batching (`decode/arith.rs` `decode_mps_run`)

Structures' masks are 85–99% set: the background is JBIG2 1. In a uniform area the
context is all-1 (or all-0), its MPS is that value with a tiny `Qe`, and DECODE takes
the fast path: `A -= Qe; C -= Qe << 16`, no context write. `n` such steps fold into
one subtraction of `n * Qe`, valid while `C_high >= i*Qe` and `A - i*Qe >= 0x8000`
for every step. When the context is all-`v` and the next `k` incoming window bits
are all `v` (leading zeros of the window XOR the fill), the decoder runs up to `k`
pixels this way, finishes a blocked step with the normal DECODE, and continues while
the pixels stay `v`. Commit 1 entered runs only at block starts for whole uniform
blocks; commit 2 enters them at any pixel, bounded by the window (p0: 11.2 → 9.1 ms
on a loaded host).

### 3. MQ DECODE fast path first (`decode/arith.rs`)

`decode_bit` tests the MPS-without-renormalisation case first and returns; the
LPS/MPS-exchange and RENORMD flow is a separate function with exactly the T.88
logic. The generic decoder keeps the decoder registers in a local copy for the
whole region.

### 4. Renderer adapter (`pdf-image/src/jbig2.rs`)

`as_msb_bytes().to_vec()` (per-word pushes), a second inversion pass and
`Arc::from(Vec)` (a third copy) became one pass writing each row's inverted
big-endian word bytes into a zeroed `Arc<[u8]>`. Same bytes, stride and inverted
padding; cancellation polled before row 0 and every 256 rows. Not measured on its
own (under 1 ms of an 860 KB plane either way); it is inside the page numbers.

## Tried and dropped

- **Preloading both candidate next context states** (`ctxs[base]`, `ctxs[base|1]`)
  while the current pixel decodes, selecting by the decoded bit, to take the
  context-bank load off the per-pixel dependency chain: slower (p0 10.8 → 11.5 ms
  min, cp 13.0 → 13.9, 6 rounds, loaded host). Reverted.
- **`#[inline(never)]` on the exchange/renormalisation path** (the encoder's
  choice): 0–9% slower on the three streams compared (loaded host). Kept inline.
- A probability-estimate cache next to the context state (the encoder's last
  lever) was not tried: the public API passes `&mut [MqContext]` (one byte per
  context, shared with symbol dictionaries across calls), so a cached bank would
  need an import/export of 65 536 entries per call; with most Structures pixels in
  batched runs the per-pixel path is no longer the dominant cost.

## Correctness checks

- `decode::generic::tests::windowed_decoder_matches_reference`: 480 cases ×
  (encoded stream + arbitrary bytes), every template, nominal / valid / arbitrary
  (non-causal) AT pixels, TPGDON on/off, widths 1–200 including 31/32/33/63/64/65,
  1–3 bitmaps sharing one context bank, fresh / trained / out-of-table initial
  context states. Compares bitmaps, decoder registers `(BP, C, A, CT)` and the whole
  context bank against the previous decoders and DECODE, kept verbatim under
  `cfg(all(test, feature = "encode"))` in `generic::reference` and
  `ArithmeticDecoder::decode_bit_reference`. Two mutations (run length off by one,
  current-row AT shift off by one) both fail it.
- `windowed_decoder_roundtrips_any_template`: test-side encoder (contexts from the
  reference `pixel_context`) round-trips through the new decoder.
- `windowed_decoder_matches_reference_on_real_streams` with
  `JBIG2_EQUIV_STREAMS=<dir>`: all 632 Structures masks identical (bitmaps,
  registers, contexts). Vacuous without the variable; the streams are not
  distributed.
- Renderer `verify` mode, serial, adapter vs the 035660da adapter: Structures scale 1
  and 2 632/632 pages identical each; centralpark (JBIG2 symbol dictionaries + text
  regions) scale 1 and 2 136/136; chinese-medicine (likewise) scale 1 and 2 392/392.
  The other corpus files carry no `/JBIG2Decode`.
- `pdf-image` `jbig2::tests` (7) pass.

## Follow-ups

- The skip-aware path (`decode_generic_bitmap_skip`, halftone gray planes) still
  forms contexts per pixel; it only gained the DECODE fast path.
- Text-dense pages now spend most of the decode in the per-pixel path (2.5–3 ns/px
  there, measured with runs disabled on a loaded host); `page.rs` composites a full-page region into the page bitmap with a
  word-level combine that a full-page Replace/Or could skip.
