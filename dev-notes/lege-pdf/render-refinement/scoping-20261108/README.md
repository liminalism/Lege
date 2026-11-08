# Artefacts of the 2026-11-08 renderer scoping review

These are the measurement tools, corpus generators and tested fix candidates used by
[`../plans/optimization-scoping-20261108.md`](../plans/optimization-scoping-20261108.md).

They are kept so that the numbers in that note can be reproduced, and so that the next optimisation round can reuse them as a starting point. The 11-04 evidence lived in a prunable scratch directory and was lost. Nothing in the build reads this directory.

## Layout

| Path | What it is |
|---|---|
| `harness/probe/` | `lege-render-probe`: an out-of-workspace measurement harness. It has path dependencies on the renderer crates and its own `[workspace]`. Its modes are `run` (sequential compile and render through one backend, with hashing and PPM dumps), `doc` (whole document through `RenderScheduler::render_range`, with per-page hashes), `seq` (one page rendered at a sequence of sizes through one `SharedImageCache`, which reproduces B1), and `fontscan`. The full CLI is in the header of `src/main.rs`. |
| `harness/readprobe/` | `lege-read-probe`: renders through `lege_pdf_read::RenderSession` the way lege-process does: a shared session and thread-local worker context, or a fresh session per page. It reproduces B2. An optional `mimalloc` feature and an allocation tracer find the `CompiledPage` address. |
| `harness/gray8bench/` | Standalone microbenchmark with no repository dependencies. It contains a verbatim copy of the Gray8 output loop (`pdf-render-cpu/src/surface.rs:256-289` at `5b24f8e`) and exact alternatives (tables, floor/compare rounding, SSE2), checked exhaustively over all 2²⁴ inputs. |
| `harness/sessionbench/` | `lege-session-bench`: production-path timing. Opens one `lege_pdf_read::RenderSession` and makes N sequential compile+render passes over every page (`<pdf> <dpi> <rgb8\|gray8> [passes]`). Pass 0 is cold; later passes are warm. Prints per-pass milliseconds and a checksum. |
| `harness/ab.py` | Paired A/B of two `sessionbench` binaries: interleaved, order-swapped rounds; reports median times, the median per-round ratio, wins, and whether the checksums match. `--pass 0` times the cold pass. |
| `harness/mkjbig2/` | Encodes 1-bit PBM pages into a JBIG2 `/ImageMask` MRC PDF with shared `/JBIG2Globals`, using jbig2enc-rust. Used for the `mrc_jpx_jbig2` corpus class. |
| `corpus/mkcorpus.py` | Generates the 13-file synthetic perf corpus (text with TrueType, standard-14 and Type 1 fonts; vector; JPEG scans; G4 bilevel; transparency; MRC; JPX). Usage: `python3 -I mkcorpus.py <outdir> [class ...]`. Needs reportlab, Pillow, numpy and system fonts. |
| `corpus/mk_determinism_pdfs.py` | PDFs for B3 (near-equal glyph sizes, float-noise matrices) and B2 (pages alternating Type 1 fonts under one resource name). |
| `corpus/mk2page.py`, `corpus/mkdpi.py`, `corpus/metrics.py` | B1 inputs: one shared DCT XObject drawn as a thumbnail and full page; DPI sweeps. Quality metrics: PSNR against an area-averaged ideal, and gradient/Laplacian sharpness. |
| `corpus/make_tz.py`, `corpus/make_ckey.py`, `corpus/make_fontorder.py` | B4 (Tz rows plus `cm` controls), B5 (colour-keyed gray images at several placements), and B2 again (font order). |
| `patches/b1-dct-scale-cache-key.patch` | Probe-quality fix for B1. It keys DCT cache entries by the DCT scale class derived from the declared `/Width`/`/Height`. A production fix should derive the class from the SOF; the sibling branch `claude/jolly-ride-cyjbsm` does this through `ImageCodec::reduction_level`. |
| `patches/b4-tz-glyph-shapes-b5-gray8-colour-key.patch` | Probe-quality fixes for B4 and B5. Lege then matches Poppler within ~1 px on Tz rows and masks every colour-key placement. All 312 corpus page hashes are unchanged. Other consumers (text boxes, the wgpu glyph key, hinting) were not reviewed. |
| `patches/glyph-cache-foldhash.patch` | Glyph-cache shard map keyed with seeded foldhash 0.2.0, which is already in `Cargo.lock`. Warm text render −7 to −13%; output identical on 348 renders. For production, seed from OS randomness (see the note §3.4). |
| `patches/glyph-cache-foldhash-plus-shard-counters.patch` | The above, plus the hit and miss counters moved under the shard lock. A further −2 to −3.5% on warm render. |

All patches apply with `git apply` to `main` at `5b24f8e`.

[`IMPLEMENTATION-LOG.md`](IMPLEMENTATION-LOG.md) records which fixes have since landed, how each was gated and timed, and what was left out.

## Building and running

Each harness is its own Cargo workspace, outside the Lege workspace. Copy the root `Cargo.lock` into the crate directory first so that it builds against the same versions, then build into a separate target directory:

```sh
cp ../../../../Cargo.lock harness/probe/
cargo build --profile debug-fast --manifest-path harness/probe/Cargo.toml --target-dir /tmp/probe-target
```

Single-thread timing environment used throughout the note:

```sh
RAYON_NUM_THREADS=1 PDF_RENDERER_IMAGE_ROW_PAR=0 PDF_RENDERER_JPX_CONCURRENCY=serial taskset -c 3 <binary> ...
```

### Windows recipe

Build (PowerShell or Git Bash; copy `Cargo.lock` as above, one target directory per harness):

```sh
cargo build --profile debug-fast --manifest-path harness/sessionbench/Cargo.toml --target-dir <worktree>/target-sessionbench
```

Both `sessionbench` and `probe` build unchanged on Windows. Pinned, single-thread A/B (logical CPUs 0-11 are the P-cores on an i7-13700H; the E-cores otherwise get single-thread renders):

```bat
harnessb.cmd C:\path	o\python.exe 7 A.exe B.exe -- D:\pdf-testogtle.pdf 150 rgb8 3
```

`ab.cmd` sets the single-thread environment, runs `start "" /affinity FFF /b /wait python -I ab.py ...`, and the children inherit the affinity. Use full `.exe` paths. `sessionbench` extras: `--alternate <dpi2>` renders each page at `dpi2` then `dpi` on one thread (lege's analysis/base alternation, for surface-slot and cache-key effects; the checksum covers the base renders), `--threads N` shares one session across N threads, and the `mimalloc` Cargo feature swaps the global allocator.
Note: with `--alternate`, some documents (vogtle) produce a different checksum than the plain run, because per-size image-cache effects (B1) change the output.

**Comparing variants:**
- Run interleaved A/B rounds (at least 7), and read paired per-round ratios rather than absolute times.
- Compare `--hash` output across variants.
- Use `--fresh-backend` as the determinism reference. Shared-backend runs are affected by B2 and B3 until those are fixed.

## Caveats

- Everything was measured with the `debug-fast` profile on a 4-vCPU Linux VM. The shipped fat-LTO profile was spot-checked on the base harness (≤8% faster).
- Some original driver scripts, not included here, hard-coded session scratch paths. The commands above are the parts that matter.
- The patches are probe-quality. Treat them as starting points that are known to be byte-identical (or deliberately changed) on the synthetic corpus, not as reviewed changes.
