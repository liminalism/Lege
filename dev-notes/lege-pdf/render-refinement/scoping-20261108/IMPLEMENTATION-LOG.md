# Implementation log for the 2026-11-08 scoping review

This log records which fixes from
[`../plans/optimization-scoping-20261108.md`](../plans/optimization-scoping-20261108.md)
have landed on branch `claude/dazzling-planck-z44poy`, how each was gated, and what was deliberately left out. The review's IDs (B*, A*, FE*) are used throughout. A Fable advisor reviewed the fix batch before implementation. Its order was followed:
1. G1 → G2.
2. B2 → B3 → B5, B10, B13 → B7 → FE1 → B4.
3. A3 → A2 → A5 → A4 → A10 → A11.

## How every change was gated

**Hash gate.** The `probe` harness ran in `run --hash` mode over 23 inputs:
- the 15 synthetic corpus classes;
- the bug fixtures (B2, B3, B4/Tz, B5/colour key, B1/two-page gray);
- three real documents (beans, viaduct, vogtle), which are not committed.

Each input was rendered at scales 1.0, 1.37 and 2.0, in RGBA and Gray8, with a fresh and a shared backend: 2,364 renders per binary. Each change was compared with the binary before it, and every difference had to be on that change's intended list.

**Cache telemetry.** Shared-glyph-cache statistics were recorded before and after B3.

**Timing.** Timings use `harness/sessionbench`, which renders through `lege_pdf_read::RenderSession` (the seam lege uses), and `harness/ab.py`, which runs paired, interleaved rounds. Build one binary per commit, each against a detached worktree.

## Landed

| ID | Commit | What changed | Gate result |
|---|---|---|---|
| B2, B3 | `57f6704` | The worker-local font-program cache is keyed by a `Weak` to the page's font table, so an ABA match on a reused address is impossible. The glyph-cache key is the exact `f64` bits of the rasterised scale, plus the hinter's ppem. | No fresh render changed. Only the B2/B3 fixtures' shared renders changed, and they now equal fresh. Glyph-cache hits and misses on beans and vogtle are identical. |
| B5 | `ed77427` | Gray8→RGB8 promotion skips colour-keyed images. | Only `ckey.pdf` changed: keyed texels are transparent again. |
| B13 | `182c76b` | `RasterKernel` is marked dirty during a fill. A fill that finds the mark clears the scratch first. | No change. |
| B10 | `1e2bbfc` | An uncoloured tiling used with a new underlying colour gets its own tiling entry, sharing the compiled cell. | No corpus change (the corpus has no such page). On the hatched-chart fixture, red/blue/red/green replaces all red. |
| FE1 | `791927c` | `sc`/`scn` read the array head before building an Indexed space, and memoise per name. | No change. 200 `scn` now charge the decode budget exactly what one does; before, it was about 100×. |
| B4 | `b0ca048` | Tz is folded into the run matrix, `scale(Th, 1) × Tm`, and placements are in the pre-Th frame. A non-normal Th is not folded. | Only `tz.pdf` changed. Its Tz 50 and Tz 200 rows now match the `cm` control rows. |
| G2 | `3b355f5` | `lege-render-demo` times the real font scan and reports PNG-queue stalls apart from render time. It also gains `--dpi`, `--compile-workers` and `--render-workers`. | n/a |
| Ring test | `a726a24` | Image fast paths at fractional placements are compared with the full generic body. | See "Open decision" below. |
| B7 | `57a2676` | jp2lam threads the request's worker budget to every post-Tier-1 parallel site. At budget 1 no Rayon job is spawned. | No change. JPX and MRC-JPX pages hash the same at serial, auto and budgeted:3, with 1 and 4 Rayon threads. |
| A3 | `d32b18f` | Gray8 output luma uses integer floor/compare rounding instead of `roundf`. An exhaustive test covers all 2²⁴ inputs. | No change. Microbenchmark: 4.19 → 1.99 ns/px. |
| A2 | `3d69c3a` | Glyph-cache shard maps use foldhash, with each map seeded from OS randomness through std's `RandomState`. The hit and miss counters move under the shard lock. | No change. |
| A5 | `f7c62b8` | Gray8 promotion writes straight into its `Arc`. | No change. |
| A4 (libm half) | `b8f20fd` | Exact casts replace `floor()`/`trunc()` at three ICC sites and in the PostScript real-to-integer coercion. | No change. |
| A10 | `932bd57` | Fills drop an `all_opaque` clip mask they lie inside (containment checked). Tiling fills share the nearest path-clip ancestor's mask. | No change. A new test checks that an opaque five-point path clip paints like its rectangle. |
| A11 a, b | `a5c30f9` | A seeded group at full opacity with no mask is copied back by rows. An opaque Normal layer pixel is copied; an exhaustive 2²⁴ test shows the float path returns exactly the source. | No change. |
| RGB8 packing (new) | `cfdf5f1` | `RenderSession`'s RGBA→RGB8 packing is allocated once in its final `Arc` and filled three bytes per pixel. The old path extended a `Vec` byte by byte through `flat_map`, then copied the whole plane into an `Arc`. | Same bytes (sessionbench checksums). |

## Measured

All numbers come from `sessionbench` (one `RenderSession`, 150 dpi, warm second pass) with `ab.py` (9 paired, order-swapped rounds). Each binary was built against a detached worktree of its commit. The environment line is the README's (one Rayon thread, pinned to core 3). Every pair produced identical checksums.

| Change | Document (format) | Before → after | Rounds won |
|---|---|---|---|
| A3 | vogtle (Gray8) | 179.5 → 138.9 ms (−26%) | 9/9 |
| A3 | beans (Gray8) | 51.3 → 36.6 ms (−27%) | 9/9 |
| A3 | viaduct (Gray8) | 59.6 → 45.5 ms (−26%) | 9/9 |
| A3 | text_ttf (Gray8) | 407 → 259 ms (−36%) | 9/9 |
| A3 | jpeg_gray_scan (Gray8) | 796 → 768 ms (−5%) | 5/9 |
| A5 | jpeg_gray_scan (RGB8) | 847 → 581 ms (−34%) | 9/9 |
| RGB8 packing | vogtle (RGB8) | 289 → 163 ms (−44%) | 9/9 |
| RGB8 packing | beans (RGB8) | 79 → 41 ms (−47%) | 9/9 |
| RGB8 packing | viaduct (RGB8) | 89 → 48 ms (−45%) | 9/9 |
| RGB8 packing | jpeg_color_scan (RGB8) | 780 → 709 ms (−2%) | 6/9 |

**Noise floor.** A5 cannot affect viaduct, which has only DeviceRGB images, yet viaduct measured +10% between those two binaries (2 of 9 rounds). Whole-binary code-layout variance on this VM therefore reaches about ±10% on some documents, and a single A/B inside that band means nothing.

**A2** sits inside that band in wall time: vogtle +6%, beans −2%, the synthetic text files −7% to +4%. Callgrind instruction counts, which layout does not affect, show it is a small, real gain on whole documents:

| Document | Instructions before | Instructions after | Change |
|---|---|---|---|
| beans | 1.534 G | 1.529 G | −0.3% |
| vogtle | 6.260 G | 6.222 G | −0.6% |
| text_type1 | 12.94 G | 12.80 G | −1.0% |

The scoping review's −7 to −13% was measured on the render stage alone, at 72 dpi, on glyph-dense pages, where the hash is a much larger share. The padded-shard A/B was not run: A2's whole-document share is too small for single-thread timing to resolve it, and the padding matters under multi-worker contention, which needs its own setup.

## Not landed, and why

- **A4, hoist half.** Precomputing `axis_box_taps` per column and per row means threading taps through `shade()` in the generic sampler. That is a larger change than the libm half and needs its own A/B.
- **A5, caching half.** It goes through `promote_through_cache`, which the B1 branch (`claude/jolly-ride-cyjbsm`) is changing.
- **A11c** (skip the zero-fill of a seeded offscreen that the backdrop fully covers). The offscreen comes from `calloc`. For page-sized groups those are fresh zero pages, so the fill costs nothing that writing the copy would not. Doing it for smaller groups needs new `unsafe` code for little gain.
- **A11d** (knockout restore-and-diff per element): deferred by the advisor.
- **A7, A8, A9, A15, B8:** deferred as the advisor ruled.
- **B1, B6** belong to `claude/jolly-ride-cyjbsm`; **B9** belongs to `claude/gifted-pascal-eyn4qw`.
- **G1 level 2** (a worker-count × render-order determinism harness in pdf-cli, plus a read-seam test with page-cache capacity 0) was not done. Level 1, the `tests/render_determinism.rs` fixtures, landed with B2/B3.

## Open decision: the opaque image border ring

`fractional_placements_match_the_generic_body_including_the_border_ring` (exec.rs) found the following:
- Every *masked* image path matches the generic body byte for byte, ring included.
- The opaque *unmasked* fast paths draw the edge ring by pixel centre: `fast_rgb8`, `fast_rgb8_bilinear`, `fast_rgb8_area`, the CMYK area path, and Gray8 and Indexed through their RGB8 promotions. The generic body instead paints A10 fractional edge coverage there.

So the same image can get a soft or a hard edge depending on which path it takes, for example once a clip mask or soft mask is added.

Bringing the fast paths into line would change every opaque image placed on fractional device coordinates. Changing the generic body would undo A10's PDFium-matching edges. That choice is for the project, not a refactor; the test pins the interior and documents the ring.

The test also showed that the generic bilinear sampler can return alpha 254 for an opaque image, because the tap weights sum to 254/255. The magnified bilinear fast path keeps the image opaque.
