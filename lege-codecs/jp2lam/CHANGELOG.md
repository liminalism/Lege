# Changelog

## 0.3.1 — Unreleased

Changes since 0.3.0 (published 2026-11-03). The decoder is bit-identical to
0.3.0 on every stream in the renderer's corpus (20 streams, two reduction
levels each) and on the crate's own test suite; the public API gains one
function.

### Decoding performance (serial)

- Streaming inverse 9/7 wavelet: each resolution level is synthesised in one
  pass, so full-resolution subband planes are never materialised.
- Tier-1 on OpenJPEG-style packed stripe-column flags with an inlined MQ
  decoder; the three passes run on exact-length stripe slices so the compiler
  can elide bounds checks (measured neutral against unchecked indexing).
- The finest wavelet level is fused with the inverse ICT and byte packing.
- AVX2 row kernels for the lifting steps and the packer, selected at runtime
  on x86-64 regardless of the `simd` cargo feature (the renderer builds
  jp2lam with default features off). The `simd` feature's `wide` kernels
  were measured neutral on the decode path and stay off by default.
- Result: single-threaded decode is 2.0–2.3× faster at full resolution and
  1.9–2.0× at one reduction level on the two reference layers (chinese-
  medicine p0: 385 → 167 ms; Structures p0: 200 → 92 ms), under
  hayro-jpeg2000 0.5.0 on all 40 corpus measurements. Numbers and method in
  `lege-misc/dev-misc/jpxbench/`.

### API

- `decode_with_stats` returns per-stage timings beside the decoded image.

### Encoder fix

- Non-terminated coding passes in segmentation-symbol styled streams reported
  the arithmetic coder's byte count at the pass boundary, up to three bytes
  short of what a decoder needs for that pass; any layer ending one or two
  passes after a cleanup pass tripped the marker check. Lengths now carry
  OpenJPEG's +3 (MQ) / +2 (raw) correction, capped at the segment. A test
  decodes every pass prefix of segmark-styled blocks across five coding styles.
  The default single-segment path is unchanged.

### Decoder fix

- A `Serial` or `Budgeted(1)` decode now stays on the calling thread after
  Tier-1 too. The per-plane 9/7 synthesis and the fused RGB(A) packers used
  to fan out over whatever Rayon pool was ambient, so a decode on a worker
  of the PDF renderer's pool spread across that whole pool. They now follow
  the request's worker budget. Output bytes are unchanged for every budget.
  A test decodes beside a work-stealing thief worker and checks it gets no job.

## 0.3.0 — 2026-11-03

First crates.io release with multiple quality layers, the six code-block
coding switches, an alpha channel, and the exposed coding parameters.
