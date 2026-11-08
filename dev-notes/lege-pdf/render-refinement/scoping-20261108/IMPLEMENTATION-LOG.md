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

---

# Local round, 2026-11-08 (Windows host, branch `lege-pdf-agent-over-poppler`)

The second half of the plan was landed locally after merging the three cloud
branches. Plan and scout reports: [`LOCAL-CONTINUATION-PLAN.md`](LOCAL-CONTINUATION-PLAN.md),
[`scouting/`](scouting/). Three workers ran in their own worktrees
(`scoping-r1-correctness`, `scoping-r2-stencil`, `scoping-r2-small`); the parent
merged each with `--no-ff`, ran the suites and the corpus gate.

## Integration

| Merge | Contents | Gate |
|---|---|---|
| `05e1ded1` dazzling-planck | the first half above | cloud gate (2,364 renders) |
| `5763a86b` gifted-pascal | B9 Type 1 hardening | suites |
| `27debaee` jolly-ride | B1 decode-level cache keys, per-page decode reference | **layout-detection A/B passed**: detections identical on 5 of 6 documents, viaduct's two image boxes move ≤ 0.5 px ([measurements/layout-ab-jolly-20261108.md](measurements/layout-ab-jolly-20261108.md)) |

After the three merges: 78 suites, 825 tests, 0 failures; jp2lam 467 passed.

## Landed locally

| ID | Commit | What changed | Tests | Output |
|---|---|---|---|---|
| B6 | `0116b204` | `codec_target_size` hands codecs per-image-axis extents `(ceil(abs(a)+abs(b)), ceil(abs(c)+abs(d)))`; the B1 decode-level key follows. Correction to the review: the swapped box also *under*-decoded (1000x400 at device 100x300 got 1/2 scale), so rotated draws were losing sharpness. | `codec_target_size_reads_image_axes`; `quarter_turn_dct_draw_decodes_at_the_upright_level` (32 px differed before) | rotated / sheared image draws only |
| B11 | `7a21b4fb` | `CpuPreparedPage.sanity_extent` carries the page crop's device extent; `device_bounds_sane` and the stroke-width clamp (`max_output`, a second size-relative limit the review missed) use it instead of the request size. | `small_crop_window_keeps_page_spanning_paths` | renders smaller than the page (lege region crops, zoom windows) |
| B14 | `42e69c9a` | groups and soft masks rasterise against the device-plane width, so content crossing the offscreen's right edge keeps its coverage. Not last-column only: a shallow edge changes several columns of its row. | `group_content_crossing_the_offscreen_right_edge_keeps_its_coverage`; `mask_content_crossing_...` | edge pixels of affected rows |
| N1 / A8 | `7d3b73a8` | on a repaired `Parsed` whose declared `/Length` runs past the window, the resolver grows once to the declared end (capped at EOF) and retries. | `pdf-document/tests/stream_window.rs` (direct and indirect `/Length`, wrong-length control) | rare correct-length streams > 64 KiB |
| B15 | `ba929e33` | `lookup` at the eight read-only sites; inline images count as one op; inline-image names outside the spec set map to one placeholder. Three sites still intern by design (`cs`, `scn` pattern names, `Tf`): they keep the id. | `missing_resource_names_do_not_grow_the_name_table` (+120,009 names before); `inline_image_name_arrays_...` (+50,000 before); `inline_images_count_against_the_operator_budget` | none for valid PDFs |
| B12 | `6d050161` | `FileSpooler::submit_raster_stream` composes, writes and drops one sheet at a time; a failed job deletes its partial files. | `streamed_submission_holds_one_composed_sheet_at_a_time`, `streamed_and_slice_submissions_write_identical_files`, `a_failed_streamed_submission_leaves_no_sheets` | PNG bytes identical |
| B8 | `7bd9b9ee` | `ParseContext::with_cache_budget` (default 256 MiB per thread) enforced in `begin_job()`. | `begin_job_enforces_the_cache_budget`; `decrypted_stream_cache_stays_within_the_context_budget_across_jobs` | none |
| G1 level 2 | `e3f4a199`, `f6c80760` | `pdf-cli/tests/determinism.rs` (scheduler, {1,2,4} workers x two orders x two sizes vs cold hashes; catches B1 and B3 when put back) and `lege-pdf/read/tests/determinism.rs` (page-cache capacity 0, real threads, alternating fonts; catches B2). | both pass on the base | n/a |
| Tier B1 | `b08ec442` | `/ImageMask` stencil fast path: two-entry LUT through the bilevel box filter (minified) or a transparent-skip nearest path; the interior only, the generic body paints the border band, so output is identical. SAT build declines truncated buffers instead of indexing past them. | `stencil_fast_path_matches_the_generic_body`, `stencil_fast_path_eligibility_and_generic_fallbacks`; ring test extended to stencils and 1-bit gray | none (2118/2118) |
| Tier B2 | `e3922b30` | row-band weighted popcount replaces the per-draw summed-area table; Rayon over 16-row bands; the SAT stays as the `#[cfg(test)]` oracle. A 600-dpi letter page (33.7M bits) exceeded the old 32M-entry cap and took the serial loop. | `row_band_weighted_ones_match_the_sat_and_the_direct_count`, `row_band_paint_matches_the_per_pixel_loop`, `row_band_paints_a_600_dpi_page_and_declines_a_truncated_one` | none (2118/2118) |
| Bilinear alpha | `4a175132` | the generic bilinear sampler keeps an opaque image's alpha at 255 (tap weights summed to 254). | `bilinear_keeps_an_opaque_image_opaque` | ≤ 1/255 alpha on magnified bilinear generic draws |
| A21 | `09b11d29` | three parked-surface slots keyed by size, `surface_slot_stats()`. | `alternating_sizes_reuse_their_parked_buffers` | none (48/48) |
| A16 | `960ca095` | `cmyk_source_as_rgb8` calls `adobe_cmyk_to_srgb_u8` directly (both float round trips proven identities on bytes); `/Decode` byte tables (unreachable until the CMYK area path accepts `/Decode`). | `byte_components_round_trip_exactly`, `float_entry_matches_direct_byte_call`, `cmyk_rgb8_conversion_matches_pixel_with_and_without_decode` | none (60/60) |
| A18 | `5145ff46` | 16-bit Gray/RGB/CMYK promote through `pixel()` and the promotion cache; unmasked opaque axis-aligned draws with a fractional edge stay generic so the ring is unchanged. | `sixteen_bit_device_images_promote_exactly`, `..._only_where_the_ring_is_unchanged` | none (96/96) |
| Harness | `998851eb`, `3a3c9a25` | Windows bring-up: `ab.cmd` (P-core pinning), `sessionbench --alternate/--threads` and a `mimalloc` feature, `PROBE_GRAY8=1`. | n/a | n/a |

## Measured (Windows, debug-fast, pinned to P-cores, one Rayon thread, 9 interleaved rounds; other builds were running, so ratios and wins are the reliable figures)

| Change | Document | Before → after | Rounds won |
|---|---|---|---|
| Tier B1 | stencil_jbig2 (MRC, JBIG2 `/ImageMask`), Gray8 150 dpi | 663 → 167 ms (x0.25) | 9/9 |
| Tier B1 | stencil_flate | 935 → 476 ms (x0.51) | 9/9 |
| Tier B1 | bilevel_600 (600-dpi 1-bit + stencil) | 622 → 194 ms | 9/9 |
| Tier B2 (after B1) | stencil_jbig2 | 177 → 130 ms (x0.71) | 9/9 |
| Tier B2 (after B1) | bilevel_600 | 184 → 82 ms (x0.43) | 9/9 |
| B1 + B2 | stencil_jbig2 / bilevel_600 / stencil_flate | x0.16 / x0.13 / x0.43 | 9/9 |
| A21 | vogtle, `--alternate 100` | x0.79–0.80 | 9/9 |
| A21 | beans / viaduct, `--alternate 100` | x0.82–0.89 / x0.86–0.87 | — |
| A16 | synthetic 2000x2800 CMYK cover | x0.50 | 9/9 |
| A18 | 1200x1600 16-bit pages, warm 150 dpi | x0.065–0.10 | 7/7 |
| mimalloc (A/B only, not adopted) | beans / vogtle / viaduct / sahib, pinned | x0.88–0.92; +13–30% peak working set | 5–7/7 |
| mimalloc (A/B only) | same, 4 threads unpinned | x0.76–0.90; +20–65% peak working set | 5–7/7 |

beans, viaduct and sahib (no `/ImageMask`; its JBIG2 layers are 1-bit DeviceGray images and go through B2 and the LUT-nearest path) stayed within ±5% on B1/B2.

## The border ring: decided

Option A (soften the opaque unmasked fast paths' ring) was implemented as
`107f7b8b` and then **reverted** (`bc8ffffc`) after the PDFium probe showed
PDFium snaps an axis-aligned image's destination rectangle to whole pixels and
paints it solid, which is the fast paths' pixel-centre rule
([measurements/image-edge-ring-pdfium-probe-20261108.md](measurements/image-edge-ring-pdfium-probe-20261108.md)).
Rotated and sheared draws stay on the generic body with A10 coverage. What
remains open is the opposite direction: masked images (including Lege's own MRC
output) paint a soft ring on axis-aligned fractional placements where PDFium
would paint a hard one.

## Final gate

`probe run --hash --fresh-backend`, scales 1.0 / 1.37 / 2.0, RGBA and Gray8, pre-worker base binary (5763a86b) against the final HEAD: beans, viaduct, vogtle, xenosystems (305 pages), sahib_processed_1794115972, the stencil, bilevel, ring and 8/16-bit fixtures. **2,148 renders: every real document identical at every scale and format.** The only differences are stencil_flate page 2 (the `/Interpolate` stencils) in all six combinations, which is the bilinear alpha fix (≤ 1/255). B6 and B14 changed no corpus page (no rotated images or right-edge-crossing groups in it); their effect is pinned by their tests.

## Not landed

- A4 hoist (subsumed by Tier B1), A5 caching half, A11c, A14: declined, reasons in the plan.
- A7, A9 remainder, A12, A20 remainder: deferred, minor.
- mimalloc: measured only; adoption is a product decision (peak working set grows 13–65%).
- B8 per-thread contexts still linger for a closed document until the thread serves another; the budget bounds them.
- B14 was not A/B'd for speed on group-heavy pages.

---

# Local round 3, 2026-11-08: the deferred items

The items the local round left as "deferred, minor" were landed on the same
branch after the round's log was written, each with a failing-then-passing or
oracle test, the corpus hash gate (HEAD `7ea2f836` probe against the working
tree) and a paired timing where the change is meant to be felt.

| ID | Commit | What changed | Tests | Output |
|---|---|---|---|---|
| B14 speed | n/a | A/B of `42e69c9a` against its parent on `corpus/mkgroups.py` (400 right-edge-crossing groups, 400 soft masks, 2000 tiny groups) and vogtle: B/A 1.005 and 1.007, speed-neutral ([measurements/b14-group-speed-ab-20261108.md](measurements/b14-group-speed-ab-20261108.md)). | n/a | n/a |
| A20 | `fb4d313f` | O(log n) eviction: every shard of the font, glyph and image caches keeps a `BTreeMap<clock, key>` recency index; hits and inserts maintain it, eviction pops the first key (the image cache takes the minimum over the shards' first keys under its global clock, and re-checks the clock under the shard lock so a hit between peek and removal is not evicted). | `evicts_the_least_recently_used_entry_first`; the three `evicts_by_byte_budget` | none |
| A7 | `7bcbc820` | per-compile image XObject cache keyed by stream object: a page that draws one image many times inflates, converts and lowers it once. Not cached when the colour space came out of the page's `/ColorSpace` resources (`/CS0`), which another resource context could resolve differently. | `repeated_image_xobject_is_decoded_once_per_compile` (six draws charge the decode budget once; the `/CS0` variant six times) | none |
| A12 | `721b3e76` | gray irreversible 9/7 to packed bytes takes the fused finest level the colour path already had: coarse levels into quarter-size buffers, then rows out of the synthesis ring straight to level shift, round, clamp and `u8`. Same kernel and per-sample conversion as the planar path. | `fused_gray_97_matches_the_planar_path` (whole plane and a strided window, odd sizes, 1-3 levels) | none |
| A9 remainder | `9302d20b` | `ProcessingPipeline` opens one `RenderSession` and hands it to the page source (`PdfPageSource::from_session`, `PdfRenderer::from_session`), so the document is parsed once per job instead of twice; `read_pdf_file` reads the input straight into its `Arc<[u8]>` (one allocation, no `Vec` to `Arc` copy). The `Arc<[u8]>` to `Vec` copies at the RGB consumers stay: the seam's shared type has no owned form and the page cache may hold a clone. | `cargo test -p lege`; an end-to-end `lege` run on beans | none |

## Measured

Cold first pass through `RenderSession` (fresh session, pinned, 9 interleaved rounds; [measurements/round3-deferred-items-ab-20261108.md](measurements/round3-deferred-items-ab-20261108.md)):

| Change | Document | Before → after | B/A | Rounds won |
|---|---|---|---|---|
| A7 | `repeat_image.pdf` (one 1200x1600 image drawn 12 times per page), Gray8 150 dpi | 429 → 270 ms | 0.615 | 9/9 |
| A12 | `gray_jpx.pdf` (2000x2800 gray 9/7 JPX), Gray8 150 dpi | 214 → 207 ms | 0.984 | 7/9 |
| A12 | same, 300 dpi | 244 → 237 ms | 0.977 | 8/9 |
| controls | beans Gray8 / vogtle RGB8 | — | 1.025 / 1.001 | 3/9 / 4/9 |

The warm pass is flat on every document (the page and image caches serve it). A20 and A9 do not register in this harness: eviction rarely runs within one document, and `sessionbench` already opened a single session.

## Gate

`gate.sh`, probe at `7ea2f836` against the working tree, 16 documents (beans, viaduct, vogtle, xenosystems, sahib_processed, the stencil, bilevel, ring, 8/16-bit fixtures, and the new `groups`, `gray_jpx`, `repeat_image` fixtures), scales 1.0 / 1.37 / 2.0, RGBA and Gray8: **2,202 of 2,202 renders identical.** Workspace suites: 175 suites, 2,153 passed, one failure (`lege-document-ocr-cli` `list_file_expands_urls_and_skips_comments`, a Windows `file://` path bug from September that fails identically at `7ea2f836`); jp2lam 468 passed. Clippy on the changed crates adds no new diagnostics (the `unwrap()` denials it reports are in `lege-pdf/write`, untouched). End-to-end `lege` runs of beans to CCITT4 PDF, JBIG2 PDF and DjVu succeed on the rebuilt CLI.

## Still open

- A5 caching half, A11c, A14, A4 hoist: declined, as in the plan.
- Snapping masked-image edges to match PDFium (the ring follow-up).
- mimalloc adoption (measured; product decision).
- The three `cs`/`scn`/`Tf` intern sites keep interning by design.
