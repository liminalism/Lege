# Renderer speed and memory scoping review (2026-11-08)

**Scope:**
- the native PDF renderer (`lege-pdf/render/crates/*`);
- the two decoders it links: `lege-codecs/jp2lam` and `lege-codecs/jbig2enc-rust`;
- the code that drives it: `lege-pdf/read`, `lege-process`, `lege-pdf/print`, and the scheduler tools.

**Baseline:** `main` at `5b24f8e`. Line numbers refer to that commit.

**Status:** this is a scoping document. No renderer code was changed on this branch.
- Every claim was checked against the code by an adversarial verifier.
- Every bug claim, and the cheapest levers, were also reproduced or measured at runtime (§3.3).
- Still in progress, to be added in a follow-up commit: two timing probes, the allocator and the glyph-cache hash. They are marked *pending* below.

Contents:
1. Summary
2. Past optimisation runs: what the ledger and notes show
3. How this review was done
4. Bugs found in passing (fix first)
5. Ranked opportunities
6. Memory plan
7. Fix the measurement setup before the next round
8. Not recommended
9. Suggested execution plan

Appendices: A. Artefacts · B. Verification record · C. Tried and rejected · D. Suggested AKR records

---

## 1. Summary

**Where Lege stands.** On 2026-11-06 Lege was faster than all six renderers tested, by geometric mean:

- 1.40× PDFium
- 1.36× Hayro
- 2.26× MuPDF

Four things remain:

- **Text-heavy pages.** On the text-heavy "beans" document Lege runs at 0.65–0.83× PDFium's speed.
- **Short books at 8 workers.** On the 22-page "vogtle" book Lege runs at 0.95× Hayro's speed.
- **Peak memory.** Lege uses more than Hayro: doc-median 306 vs 216 MiB, max 1,548 vs 1,090 MiB, at 8 workers and 144 DPI.
- **Rare page types.** Several page types that the private corpus barely contains turn out to be slow (§3.3).

**Findings:**

1. **Fix a set of correctness and determinism bugs first** (§4). Six were reproduced at runtime.
   - **B1 is the most serious.** JPEG images decoded at reduced scale are cached under a key that omits the scale.
     - Lege's default PDF→PDF pipeline first renders a small layout-analysis image of each page.
     - On gray-suitable JPEG pages, the base, OCR and region renders then reuse that quarter-area decode.
     - Measured on a 300-dpi scan: base-render sharpness falls to 0.74× of a correct render.
     - With two or more workers the output is nondeterministic.
   - **B9 crashes the process.** A crafted embedded Type 1 font makes any Lege binary abort on an allocation failure, which `catch_unwind` cannot contain.
   - **The others:**
     - fonts drawn with another page's font program;
     - text bytes that depend on page order and thread count;
     - Tz (horizontal scaling) never applied to glyph shapes;
     - colour-key masks lost on minified gray images;
     - uncoloured patterns painted in the wrong colour;
     - rotated scans decoded at 4–16× the needed pixels.
   - **A fix for B1 is in progress** on branch `claude/jolly-ride-cyjbsm` (not merged). It also implements the per-page decode-level sharing recommended here. B6 (rotated placements) is not fixed there and has been reported to that session.
2. **The largest measured speed costs are a few slow paths, not the core rasteriser.** Measured on a synthetic corpus in this container:
   - `/ImageMask` stencils never take a fast path. On MRC pages about 90% of render time is spent in the generic per-pixel sampler, 3.4–3.6× slower than Poppler. Lege's own MRC output uses exactly this shape: a JBIG2 `/ImageMask` over a background image.
   - Bilevel images rebuild a 33.7 MB summed-area table on every draw.
   - 8-bit gray images are re-expanded to RGB on every draw. A gray JPEG page is about 1.8× slower than a colour one of the same size.
   - The glyph cache hashes its key with SipHash, about 10–15% of the instructions on a text page. This bears directly on the beans gap.
   - Builds target baseline x86-64, so `floor`, `ceil` and `round` compile to out-of-line library calls: 290 call sites. These run per edge or per pixel in the rasteriser, the generic sampler and shadings, and per pixel in the Gray8 output conversion.
     - Building for x86-64-v2 removes them all, with bit-identical output. It saves 8–15% on vector, text and MRC pages, provided the build also disables one LLVM tuning flag (§3.4).
     - The Gray8 output conversion alone costs ~7 ms per 1224×1584 page, about 5× the whole RGBA render of a text page. `lege` uses Gray8 for every gray page. An exact rewrite verified over all 2²⁴ inputs makes it 2.3–3× faster.
3. **`lege-process` wastes renderer work:**
   - each page is rendered as 2–4 separate products, and JPX layers are re-decoded for each one;
   - the read seam copies every RGB raster three times;
   - encrypted PDFs grow per-thread parse caches without bound;
   - the input file is copied into memory and opened twice.
4. **Memory has several large, cheap levers** (§6):
   - **Image cache:** in one-pass runs the 96 MiB decoded-image cache holds only dead entries. Measured peak drops from 84 to 29 MiB with it off, at no speed cost.
   - **Summed-area tables:** 34–135 MB per concurrent bilevel draw.
   - **JPX coefficient planes:** 39–155 MB per in-flight decode.
   - **Parked surfaces:** every thread keeps a full-page surface after rendering.
5. **The benchmarks and the product exercise different code** (§7):
   - The scheduler path, used by `lege-render-demo`, `pdfium-diff` and the 2026-11-06 comparison, builds a fresh worker context for every page. Production `lege-pdf-read` keeps one per thread.
   - "Serial" JPX decodes still fan out across threads.
   - `lege-render-demo` counts PNG back-pressure as render time.
   - `pdfium-diff` builds a different profile from the shipped one.
   - The fast-path equivalence tests cannot see the 1-px border ring.
   - Every image-cache test uses one render size, which is how B1 shipped.
6. **Not worth doing now** (§8):
   - a GPU renderer for Lege's product;
   - full GPU rasterisation;
   - a surface-format change;
   - JPX region-of-interest decode for whole pages;
   - multi-process rendering.

---

## 2. Past optimisation runs: what the ledger and notes show

**Sources:**
- the 19 dated notes in `../performance-history/`;
- the AKR records under `.akr/records/lege-ecosystem-perf/` and `.akr/records/lege-ecosystem/`;
- `../handoffs/DEFERRED.md` and the plans in this directory;
- `docs/measurements/pdf-renderer-comparison-2026-11-06.md`;
- git history, which in this checkout starts on 2026-10-07, so earlier commits are cited from the notes.

### 2.1 Timeline

| Date | Run | How it was run | What it achieved | Output |
|---|---|---|---|---|
| 07-19 | Baseline and attribution | One agent, 13-page perf manifest | Lege 4–6× slower than PDFium per page. It won whole documents only through 20 threads against single-threaded PDFium. | — |
| 07-20 | Image sampling, CCITT box filter, Type 1 cache, JPEG passes 1–3, JPX integration, raster kernel, glyph cache, RGB8/CMYK area-min | Sequential passes, then two rounds of three parallel agents in one shared, uncommitted tree | JPEG sweep 1588→338 ms. Latin page 20.5→3.2 ms. Latin document 131→316 pages/s, against PDFium's 213. | Identical, except gated JPX reduction and glyph sub-pixel buckets |
| 07-21/24 | JPEG pass 4, SharedImageCache, Flate on zlib-rs (1.65×), clip masks | Parallel stream branches; Windows sweep 13 | Fractional rect clips 20× faster. basinmap 140→62 s. | Identical |
| 08-08/09 | MRC masked area-min, final-buffer Arc ownership, Gray8→RGB8 promotion | Worktree baseline | MRC stencil 144→12.6 ms. Structures 229→69 ms. Peak live memory halved. | Identical |
| 08-24/25 | Static efficiency review by three reviewers: 11 findings applied, 6 deferred. Memory audit in two waves. | Static review | Hard image-cache caps; decode memory counted at admission | Identical |
| 09-04 | Surface reuse; load-aware image-row fan-out | Interleaved A/B/B/A | 2.2–4.8× per scanned page. Whole document neutral; see §7 for why. | Identical |
| 11-04 | Head-to-head vs Hayro/PDFium; JPX headroom 2→1; lazy system-font index | Main checkout | chinese-medicine 494→191 ms/page. Cold start 133→8 ms. | Agreement unchanged |
| 11-04 R1–3 | jp2lam serial path, text/vector, image draw, glyph blit, run-aware fills, JBIG2 generic decode | Parallel per-area worktree sessions and bounded workers | From Hayro leading on 7 of 8 documents to Lege leading on 7 of 8, serially | Identical (3698/3698 renders) |
| 11-04 R4 | Reorder window, font dedup, adaptive glyph flattening, masked alpha pass, LUT nearest painter | Sequential in the main checkout, no subagents | beans 22.7→14.7 ms. Lege's own JBIG2 pages 105→5.5 ms. | Identical, except flattening, which has a knob that reproduces the old bytes |
| 11-06/08 | Six-renderer comparison; coverage averaging for minified stencils | Pinned cores, 3 repetitions, one process per job | 1.40× PDFium, 1.36× Hayro, 2.26× MuPDF. 72-DPI agreement 79.3→97.5%. | The stencil change is an accuracy fix costing 4–8% on MRC books |

### 2.2 How the runs went

**What worked:**
- **Attribution before optimisation.** Measure where the time goes, then change things.
- **Worktree baselines** from 08-08 onward.
- **Byte-identical hash gates** over 3,698 renders, plus knobs that reproduce the old bytes whenever a change deliberately alters output (`PDF_RENDERER_OUTLINE_SEGMENTS=8`, `JPX_REDUCE=0`, `GLYPHCACHE=off`).
- **Interleaved A/B runs on pinned P-cores.**
- **Sequential follow-up rounds.** Round 4 dropped a planned lever after profiling it, and found its biggest win, 105→5.5 ms, while testing something else.

**What went badly:**
- **Shared dirty tree (July).** Baselines were made by reverting individual files, which made attribution fragile.
- **Concurrent per-area sessions (November):**
  - one session's builds polluted the others' timings;
  - file ownership blocked fixes (a precomputed glyph coverage count was dropped because `prepared.rs` belonged to another session);
  - one session's misdiagnosis spread to the others;
  - AKR could not record from inside worktrees.
- **No standing performance-regression gate.** A quality tweak, the 2× JPX headroom, silently disabled reduced decoding for months: 494 vs 191 ms/page.
- **Wrong premises in briefs.** "viaduct is vector" (it is three JPEG pages); "Structures has a stencil" (it is a 1-bit /SMask); "13.42 taps/px" (really ~2.5× minification).
- **Protocol mismatches:**
  - July timed Lege on 20 warm threads against single-threaded PDFium;
  - an env knob, `JPX_CONCURRENCY=1`, silently meant "automatic";
  - the cold-start font scan was the whole result on 3-page documents;
  - Windows parked single-threaded runs on E-cores, a 1.6× error bar.
- **Ledger hygiene:**
  - an import turned 5 plans into 125 work records;
  - 38 evidence records cite the prunable `.agent/scratch`, including the 11-04 variance investigation;
  - July's history is not in the ledger.

**Still open from earlier runs:**
- **Unfunded deferrals in `lege-ecosystem.work.renderer-efficiency-deferrals`:** the CMYK conversion cache, the CCITT bulk skip, the lexer name bulk scan, and postprocess rayon.
- **jp2lam decoder plan phases 4–6:** ROI IDWT, the Tier-2 HashMap, PGO, BGRA output.
- **Surface-imagerows and MRC follow-ups:** the band integral replacing the 100 MB summed-area table, colour-key masks, and non-minifying soft masks.
- **Hairline stroke fast path.**
- **Band/tile planning, and permit classes for reorder-buffer memory** (DEFERRED).

What was **tried and rejected** is in Appendix C. The findings below are filtered against that list.

### 2.3 Standing over time

Against PDFium:
- **Before July:** 4–6× slower per page.
- **07-20:** JPEG pages at 0.6× PDFium's time and Latin pages at 4.9×; Latin dropped to 1–3.2× once the glyph cache landed.
- **11-06:** 1.40× faster overall, losing only on beans.

Against Hayro:
- **11-04:** Hayro led on 7 of 8 documents.
- **11-04, after the rounds:** Lege led on 4, then 7, then every serial cell.
- **11-06:** Lege 1.36× faster.

Memory against Hayro has not been worked on directly.

---

## 3. How this review was done

### 3.1 Agents and verification

**Wave 1, scoping (ten parallel read-only agents):**
- A ledger historian (§2).
- Eight code reviewers, one area each:
  - raster core;
  - text, fonts and glyphs;
  - image draw path and colour;
  - JPX and JBIG2 decoders;
  - parsing, interpreter and IR;
  - scheduler and consumers;
  - build, ISA, allocator and hashing;
  - architecture and GPU.
- A profiler, which built the renderer, generated a synthetic corpus, and timed and profiled it (§3.3).

**Wave 2, verification:**
- **Bugs.** Each claimed bug got its own adversarial verifier, told to refute it.
- **Performance findings.** Each was re-read against the code by a skeptic for its subsystem, including whether the proposed change really keeps output bit-identical.
- **Completeness critic.** Looked for uncovered areas, contradictions and history mismatches.
- **Empirical probes.** Run one at a time on a pinned core. They reproduced the bugs and A/B-tested the cheapest levers.

### 3.2 Limits

- **Comparison not re-run.** The private 8-document corpus (`D:/pdf-test`), PDFium, MuPDF and Hayro are not available in this container, so the 2026-11-06 comparison was not re-measured.
- **Different host.** This was a 4-core 2.3 GHz Xeon VM on Linux, not the i7-13700H on Windows.
  - Absolute timings do not transfer.
  - Poppler (`pdftoppm`) ratios are a sanity anchor, not an oracle.
- **Windows unmeasured.** No Windows-specific cost was measured: NT heap, VirtualAlloc page faults, UCRT `roundf`.
- **Corpus gaps.** The synthetic corpus has none of: CJK, Type 3, ICC/CMYK/Separation, encryption, object streams, annotations, JBIG2 generic or halftone regions, 16-bit images. Targeted PDFs were generated for each bug reproduction.

### 3.3 Synthetic corpus measurements

Single thread, 144 DPI, `debug-fast` build. A fat-LTO release build of the same harness was at most ~8% faster.

| Class | Lege warm (ms/page) | Lege document pass (ms/page) | pdftoppm (ms/page) | Lege / Poppler | Peak RSS, MiB (Lege / Poppler) |
|---|---|---|---|---|---|
| TrueType text, 2.7–3k glyphs | 1.89 | 2.12 | 15.6 | 0.14 | 14 / 23 |
| Type 1 text | 1.83 | 2.62 | 20.8 | 0.13 | 15 / 23 |
| Vector (page 0) | 58 | 42 | 77 | 0.55 | 25 / 24 |
| Gray JPEG scan, 2550×3300 | 20.1 | 45 | 51 | 0.88 | 60 / 28 |
| Colour JPEG scan | 13.2 | 40 | 53 | 0.75 | 83 / 28 |
| CCITT G4 bilevel | 63 | 73 | 43 | **1.70** | 56 / 28 |
| Transparency (page 0) | 154 | 100 | 136 | 0.73 | 28 / 32 |
| MRC, JPEG + G4 `/ImageMask` | 226 | 241 | 67 | **3.60** | 57 / 28 |
| JPX scan | 12.9 | 145 | 392 | 0.37 | 84 / 129 |
| MRC, JPX + JBIG2 `/ImageMask` | 233 | 279 | 122 | **2.29** | 78 / 54 |

**Where the time goes** (callgrind, exclusive share of instructions):

- **Text:**
  - page-surface memset 28% (by the timer, 0.43 of 1.84 ms);
  - glyph blits 17%;
  - SipHash on the glyph-cache key 9.7% at 144 DPI and 15.2% at 72 DPI;
  - glyph-miss rasterisation 10%;
  - ~4,350 allocations per page, about 1.3 per glyph.
- **Vector:**
  - rasteriser fill, edge deposit and accumulate ~52%;
  - out-of-line libm `floorf`/`ceilf` 6.3%.
- **JPEG:** Huffman decode ~53%; area minification 23%.
- **Bilevel:** building and reading the summed-area table, ~90%.
- **MRC:**
  - ~90% in the generic per-pixel stencil sampler: `pixel` 43%, `shade_uv` 19%, `axis_box_taps` 13%, libm rounding 7.7%;
  - decoding is 3% or less.

**Scheduler throughput** (4 cores, pages/s, compile + render workers):

| Class | 1+1 | 2+2 (default here) | 2+6 |
|---|---|---|---|
| MRC JPEG+G4 | 13.5 | 8.2 | 14.7 |
| MRC JPX+JBIG2 | 11.8 | 7.5 | 12.4 |
| JPX scan | 19.9 | 18.0 | 24.6 |

- **The 2+2 default is the worst configuration measured on scans.** Image-row fan-out switches off as soon as two pages are in flight, while the compile workers sit idle.
- **On one core the scheduler is 1.3–1.5× slower than a plain serial loop.** It builds a new surface and new parse caches for every page.

### 3.4 Lever probes

Paired A/B runs, 7 interleaved rounds, pinned core, `debug-fast` builds.

**x86-64-v2 vs baseline**
- Bit-identical output across all 156 pages × {72, 144 DPI} × {RGBA8, Gray8}.
- MRC: 0.913× time at 72 DPI and 0.845× at 144 DPI (7/7 rounds).
- Vector: 0.91–0.92×.
- TrueType text: 0.92–0.94× warm; 0.91× with the glyph cache off.
- Transparency: 0.97–0.99×.
- JPEG scans: **1.02–1.03× (slower, 0/7 rounds faster).** The cause is LLVM's `slow-unaligned-mem-32` tuning, which v2 enables: it splits 256-bit loads in 17 existing AVX2 functions. Adding `-C target-feature=-slow-unaligned-mem-32` brings JPEG back to 0.99–1.00×, keeps the other gains, and is bit-identical on 936/936 renders.
- Instruction counts fall by 8–15% on vector, text and MRC pages, about twice the cost of the libm functions themselves. The difference is call overhead, register spills and lost vectorisation.

**Gray8 output**
- The conversion adds 6.6 ms per text page and 7.4 ms per JPEG page at 1224×1584 (3.4–3.8 ns/px). A text page's whole RGBA render is ~1.25 ms.
- v2 halves it. The exact table rewrite reaches 1.5 ns/px and the SSE2 rewrite 1.1–1.4 ns/px, both verified over all 2²⁴ inputs.

**Allocator (mimalloc) and glyph-cache hash** *(probes pending, added in a follow-up commit)*.

**Bug reproductions** (§4)
- B1: base sharpness 0.74×; extreme sequence 0.35×; scheduler nondeterministic in 8–10 of 30 runs.
- B2: wrong typeface in 10 of 10 runs with a reused context.
- B3: 72% of ink pixels change with page order; every multi-worker run differs.
- B4: glyph widths fixed at 32 pt across Tz 50/100/200.
- B5: 100% of the keyed area painted.
- B9: process abort; the quadratic encoding loop takes 0.81 s for 16k entries against 0.004 s for the control.
- The fix candidates for B4 and B5 leave all 312 corpus page hashes unchanged.

---

## 4. Bugs found in passing (fix first)

Every row was traced end to end by a verifier told to refute it. "Reproduced" means a runtime probe showed it.

| ID | Bug | Who hits it today | Status | Fix | Output effect of the fix |
|---|---|---|---|---|---|
| **B1** | **A reduced-scale JPEG decode is cached under a key that omits the scale.** `ImageCacheKey::new` keeps the draw target only for JPX (`pdf-render-cpu/src/prepared.rs:563-570`). `JpegCodec` decodes at 1/2, 1/4 or 1/8 chosen from that target (`pdf-image/src/jpeg/mod.rs:58`; `pick_dct_size`, 1022-1044). | **lege CLI/GUI default PDF→PDF pipeline**, on every non-cover, gray-suitable page with DCT images. The 618×800 analysis render (`pdf_tokio_pipeline.rs:3289-3301`) caches a 638×825 decode of a 300-dpi Letter scan. The 927×1200 base, slow-OCR and 2× region renders then reuse it. Also hit: a JPEG drawn at two sizes in one document; margin mode at 200/400 dpi; FFI hosts rendering thumbnails then zooming. | **Reproduced.** Base sharpness 0.74× of fresh. A 0.25→2.0 sequence gives 0.35× (text nearly illegible). With 2 compile workers, a stale page appears in 8–10 of 30 runs. Cache off or `JPX_REDUCE=0` removes it. | Key DCT by the selected DCT scale, computed from the SOF (S–M). The sibling branch keys by the codec-reported level and also fixes a related gap: JPX output depends on the declared colour space, which the key also omitted. Fix the stale comments (prepared.rs:563-567, 2326-2330; `pdf-image/src/lib.rs:69-72`) and the AKR claim `target-only-for-jpx`. | Restores cold-render output. Adds one decode exactly where HEAD served the wrong one (+55 ms/page on a 400-dpi scan). Pair it with decode-level sharing (§5, B3). |
| **B2** | **The per-worker font-program cache recognises "same page" by raw `CompiledPage` address** (prepared.rs:36-45, 4061-4074). | Anything that reuses one `CpuWorkerContext` across pages: `lege-pdf-read` thread-local contexts (lege CLI/GUI, FFI, document-ocr), `pdfium-diff`, the wgpu CPU fallback. Type 1 and bare-CFF programs only. | **Reproduced.** A Charter page drew in Courier shapes, and a TrueType page in Charter shapes, in 10/10 runs. The wrong bitmaps land in the shared glyph cache under the correct font's key and reach later pages. `RenderSession` with page-cache capacity 0: 8/8 runs. At the default capacity 8: not triggered in ~66 runs, so the production risk is unproven but not excluded. | Hold `Arc<[FontResource]>` and compare with `Arc::ptr_eq` (S). | Only pages that were hit change. |
| **B3** | **The glyph-cache key quantizes the per-design-unit map at 1e-4** (prepared.rs:4229-4231), while a miss rasterises exact values. The step is 0.1–0.2 px per em, ~1000× coarser than PDFium's. Sizes within ~0.1 pt share whichever bitmap was inserted first. | Any multi-page render through one backend: lege with ≥2 page workers, `--pdf-to-images` page ranges, print. | **Reproduced.** 12.00 and 12.03 pt share a key; 72% of ink pixels change with page order (max 26/255). With ≥2 render workers, every run gives a different hash (12/12; 8/8 on all 16 pages). | Key on the exact bits, or store the exact (la, ld) and treat a mismatch as a miss (S). | Pages that never got a foreign bitmap are unchanged. Colliding pages become their cold-render bytes. |
| **B4** | **Tz (horizontal scaling) never reaches glyph shapes.** It is applied to positions only, on every glyph path: cached, outline, stroke, clip, tiled (`pdf-content/src/lower.rs:559`; prepared.rs:4213-4229, 4697-4699, 3964-3975). | Every consumer, on text with Tz ≠ 100. | **Reproduced.** A 48-pt "M" is 32 pt wide at Tz 50, 100 and 200 (Poppler: 16/32/64). Condensed text overlaps into a blob. | Add `horizontal_scale` to `GlyphRun`; fold Th into the outline map and the glyph key (M). | Deliberate change, on Tz ≠ 100 text only. The probe fix matches Poppler to within ~1 px and leaves all 312 corpus hashes unchanged. |
| **B5** | **The colour-key `/Mask` is lost on minified 8-bit gray images.** The Gray8→RGB8 promotion (`image.rs:673-697`) does not exclude `ColorKey`; `color_key_hit` then rejects the one-pair key (1628-1632). | lege CLI/GUI, print, pdfr: axis-aligned minified draws. | **Reproduced.** 100% of the keyed area paints. | Exclude `ColorKey`, as the sibling promotions already do (image.rs:720, 912) (S). | Keyed pixels become transparent again; all 312 corpus hashes unchanged. |
| **B6** | **Quarter-turn placements pass a swapped bounding box as the codec target** (prepared.rs:5269-5318; `jpx.rs:270-276`). | /Rotate 90/270 pages and quarter-turn `cm` in lege, print and pdfr. Non-square JPX/DCT images decode 1–2 levels too large: 4–16× the pixels, e.g. 160–380 vs 40–143 ms for one MRC layer. | Confirmed by code reading. A performance bug, not a determinism one. Present on the sibling branch too; reported to that session. | Per-image-axis extents `(ceil(\|a\|+\|b\|), ceil(\|c\|+\|d\|))`; identical to today's box when b = c = 0 (S). | Only rotated draws change. |
| **B7** | **"Serial" JPX decodes still fan out** on the ambient rayon pool for 8-bit RGB, RGBA and CMYK (`reconstruct.rs:1974-1987, 2014-2045, 1239-1245, 1449-1480`). | The 2026-11-06 8-worker cells (nested parallelism was not off for the RGB JPX books). In lege-process, render threads contend with the encode pool. | Confirmed. No wrong pixels. | Pass the request's worker budget into reconstruction (S). | None. |
| **B8** | **Unbounded per-thread parse caches for encrypted PDFs.** `lege-pdf-read` keeps a `ParseContext` per thread (`session.rs:21-46`), and decrypted stream bodies are owned (`pdf-document/src/resolve.rs:178-182`). | lege on owner-password PDFs, bounded only by the lifetime of a blocking-pool thread. FFI, document-ocr and print keep a closed document's caches until the next document. | Confirmed for production. The scheduler-memory sub-claims affect demo/bench only. | Byte-bounded `ParseContext` LRU, or lazy decryption from source ranges; session-scoped contexts (M). | None. |
| **B9** | **A crafted Type 1 font aborts the process.** `parse_subrs` sizes vectors from an untrusted `dup <idx>` (`pdf-font/src/type1.rs:310, 321-327`). | Any consumer compiling a page that selects the font: lege CLI/GUI, lege-pdf, pdfr, FFI, Android. Read-seam compile is not under `catch_unwind` (`session.rs:443-469`). | **Reproduced** with `pdfr doctor`. idx=1e16 aborts (SIGABRT, allocation of 2.4e17 bytes). idx=1e18 panics with capacity overflow. idx=usize::MAX panics on an index after the `+1` wraps. Satisfiable sizes inflate memory that the font caches never charge. | Clamp to the declared count, `section.len()/8` and a hard cap, or store subrs sparsely (S). **Fix-task card queued.** | None for valid fonts. |
| **B10** | **An uncoloured (PaintType 2) pattern is cached by name only**, while its underlying colour is baked in (`interpret.rs:227, 5114-5121, 5717, 5757`). | Every use after the first in one scope paints the first use's colour, for both fill and stroke. Example: a two-series hatched bar chart. | Confirmed (deterministic wrong output). The other half of the original claim (jp2lam `expect`s) is invariants only, so lint hygiene. | Key by (name, underlying-colour bits) (S). | Affected pages become correct. |
| **B11** | **Small crops drop page-spanning paths.** The path sanity limit is 64 × the *request* size, and the whole primitive is discarded (prepared.rs:3335-3339, 3797-3801, 5346-5357). | lege image-region crops shorter than ~19 px, which leave a hole in the JP2/JPEG overlay. Zoomed print or preview windows lose backgrounds and thick strokes. | Confirmed, low severity. | Make the limit page-relative: the device AABB of the page crop, stored on `CpuPreparedPage` (S). | Crops match the full render. |
| **B12** | **Composed print-to-file jobs hold every sheet in memory** (`print/src/spool/file.rs:176-186`). | `lege-pdf print --to-file` with a geometry option (`--n-up`, `--scaling fit`, `--margin-mm`), which the agent README recommends. 26 MB per A4 sheet at 300 DPI: a 100-page book takes 2.7 GB (10.5 GB at 600 DPI). CUPS and Windows compose one sheet at a time. | Confirmed. | Compose, write and drop each sheet (S). | PNGs are byte-identical. |
| B13 | **Persistent worker contexts survive a caught panic.** The read seam (`lege-pdf/read/src/session.rs:527-546`) and the wgpu CPU fallback (`pdf-render-wgpu/src/lib.rs:2638-2644, 2752-2775`) keep their thread-local `CpuWorkerContext` after catching a panic. A panic in the middle of a `RasterKernel` fill would leave its accumulators non-zero for the next render. The API-level justification (`pdf-render-api/src/lib.rs:486-489`) is correct for `submit_caught`/`render_blocking`, which use fresh contexts. | Latent: no known input panics inside a fill. Only lege's margin mode could publish the poisoned result. | LATENT, low | `RasterKernel` keeps a `dirty` flag and calls the existing `reset_cancelled_fill()` (raster.rs:485-492) at the next fill start (S). | None. |
| B14 | **Offscreen content is anti-aliased differently at its right edge.** The raster width is the offscreen's (`exec.rs:4464-4465`), and edges are clamped there (`raster.rs:1125-1130`). Affected: transparency groups and soft masks whose clipped content crosses the right boundary; tiling fills (both edges of the fill surface); cropped region renders (either edge). | Every consumer. Pages with BBox- or clip-limited groups, soft masks and patterns. | Confirmed by emulation and code reading; low. Errors up to 114/255 for shallow edges and ~32/255 at 45°, in the edge pixels of each row. | Pass the device-plane width (`page.size.width`) in `paint_command`; output is already restricted to command bounds (S). | Bytes change only in the affected boundary pixels. Note: any "rasterise a tile cell once and stamp it" optimisation would *add* such clamps at cell edges. |
| B15 | **The per-document name interner grows with every content-stream name.** Inline images are not counted against `max_ops` and intern every element of their `/CS` and `/F` arrays (`interpret.rs:650-667, 4745-4803`), so the real per-page bound is the 1 GiB decode budget. | Hostile PDFs only. lege drops the session at the end of each job. | Partly confirmed; memory only (NameIds never reach output). | Use `lookup` instead of `intern` at sites that only read, after `resource_subdict` (`interpret.rs:3308, 5033, 4832, 1691, 1714, 1478, 4734, 3857`) (S). | None. |

**Also confirmed by code reading:**
- **FE1:** colour operators can drop page content. Every `sc`/`scn` in a named Separation or DeviceN space rebuilds the image colour space just to test for `/Indexed`. With an `/ICCBased` alternate, that means re-inflating the profile every time.
  - About 1,000 such operators exhaust the page's 1 GiB decode budget.
  - After that, later images, forms and soft masks on the page fail to decode silently.
- **N1:** the 64 KiB stream window ladder truncates a correct-length stream over 64 KiB when its leading bytes happen to contain `endstream` or `endobj`.
- **AKR claim `promotion-is-exact`** (in `lege-ecosystem-perf.renderer.gray8-draws-take-the-rgb8-fast-paths`) is wrong for unmasked fractional placements, because the fast paths skip edge coverage. This was documented on 2026-08-08 in `../performance-history/optimization-mrc-mask-20260808.md`. The sibling branch's AKR draft restates the claim.
- **S8:** glibc's `powf`/`sincosf` dispatch to FMA or non-FMA variants by CPU, and their results reach pixels through CalRGB, Type 2 shading and round joins. The same binary can therefore produce different bytes on different machines, rarely.

---

## 5. Ranked opportunities

**Legend:**
- **Verdict** (from the skeptic pass): C = confirmed; P = partly confirmed (real but corrected).
- **Output:** "exact" means bit-identical by construction; the reasoning is in Appendix B.
- **History:** NEW, PREV (identified earlier, never funded), or TRIED.

### Tier A: small, exact, high value

| # | Change | Evidence | Expected gain | Output | Effort | History |
|---|---|---|---|---|---|---|
| A1 | Fix B1–B15 (§4); B13–B15 are low priority | Reproduced or confirmed | Correctness | See §4 | S–M | NEW |
| A2 | **Cheaper glyph-cache hash.** Replace SipHash on `GlyphCacheKey` (prepared.rs:309-319, 364) with seeded foldhash (already in Cargo.lock) or a hand-written hash over 2–3 u64 words. Move the hit counter off the shards' cache line (355-361). | Callgrind: 9.7–15.2% of text-page instructions | Directly on the beans gap. *Timing probe pending.* | Exact: no map iteration feeds output | S | NEW |
| A3 | **Gray8 output conversion** (`surface.rs:256-289`). Replace the per-pixel `round()` with an exact form. Two were verified **exhaustively over all 2²⁴ inputs**: three 256-entry f32 product tables with floor/compare rounding, or an explicit SSE2 loop at the baseline ISA. `(y+0.5) as u8` and round-half-even are **not** exact: 2,752 inputs land on exact .5 ties. | **Measured:** 7.0–7.2 ms per 1224×1584 page (3.6 ns/px). That is about **5× the entire RGBA render of a text page**. Gray8 is what `lege` requests on every gray-suitable page. | **Measured:** tables 1.5 ns/px (2.3×), SSE2 1.1–1.4 ns/px for page-sized buffers (up to 7.4× in cache). About 3–4 ms saved per 1200-px gray page in lege. | Exact (exhaustive) | S | NEW |
| A4 | **Remove libm calls where an exact rewrite exists:** `x.floor() as usize` → `x as usize` for clamped non-negative values (`pdf-color/src/icc.rs:154, 315, 453`; `axis_box_taps`); `r.trunc() as i64` → `r as i64` (`pdf-function/src/lib.rs:363`). **Hoist `axis_box_taps` per column and per row** in the generic sampler (now per pixel per axis). | MRC generic path: `axis_box_taps` 13.4% plus libm ~6.7%. Rounding ≥4.5% on vector, ≥6.7% on MRC. | Several % on vector, shading and MRC; more on the generic sampler | Exact per site; no ISA policy change | S each | NEW |
| A5 | **Gray8 image promotion** (`image.rs:673-697`): write straight into the `Arc` (S), then cache it through `promote_through_cache` as Indexed/ICC already do (M). | DHAT: two 6.31 MB allocations per draw, 68% of bytes on a gray JPEG document; gray JPEG 1.8× slower than colour | ~10 ms and 25 MB transient per 300-dpi draw; one entry shared by lege's analysis and base renders | Exact | S / M | Cost accepted in the AKR gray8 decision; caching NEW |
| A6 | **Stop `sc`/`scn` rebuilding image colour spaces:** test the array head first, or memoise per resource scope (`interpret.rs:1149-1152, 1557, 3838-3872`). | Per `scn`: 256 tint evaluations (Separation), 65,536 (2-colorant DeviceN), or a 1–3 ms profile inflate (ICC alternate) | Large on spot-colour and press PDFs; fixes FE1 | Exact | S | NEW |
| A7 | **Dedupe image XObjects within one compile:** a per-page cache of decoded payloads keyed by ObjectId (`interpret.rs:3297-3453`; `decode.rs:176`). | 8.5 ms per compile for one image drawn 12 times (5.6 MB inflated each time); every `Do` also copies the codec payload | Pages that reuse images; compile memory | Exact, apart from decode-budget charging | S–M | NEW |
| A8 | **Stream window ladder** (`resolve.rs:26-28, 237-325`; `pdf-object/src/parser.rs:397-426`): on a length miss, grow once to the declared end; keep the 64 KiB ×8 ladder only as the wrong-length fallback. | Compile 0.10 / 0.71 / 5.2 ms for 96 KB / 1.8 MB / 9.9 MB streams; production pays once per thread per stream | Compile time on large streams; fixes N1 | Exact except the N1 case | S–M | NEW |
| A9 | **Read-seam and consumer copies:** pack RGB once into the final `Arc` (`read/src/session.rs:663-700`); hand over ownership instead of `to_vec`; read the input straight into one `Arc`; reuse the first `RenderSession` (`lege-process/pipeline/config.rs:97-118`, `source.rs:84-89`). | 3 full copies per RGB render (4 for analysis); the file is copied and parsed twice | 1–2 MB per page at default sizes; 100–400 MB at 600-DPI print; one fewer xref walk | Exact | S | NEW |
| A10 | **Clip masks:** fills honour `all_opaque` (`exec.rs:4457`), as glyphs and images do; `render_tiling` goes through `mask_source` (3851-3866). | Lost run-aware path; duplicate masks | Clip-heavy vector pages | Exact | S | NEW |
| A11 | **Transparency:**<br>- skip the zero-fill when the seed covers the offscreen;<br>- composite at t = 255 with a per-row memcpy;<br>- add an opaque copy path in `composite_layer_px`;<br>- for knockout, restore and diff only each element's bounds.<br>(`exec.rs:388-393, 559-623, 4599-4740`) | Knockout is O(elements × area) today. Opaque copy proven exact by exhaustive brute force (Normal blend). | Office/Adobe transparency pages | Exact. Running the group directly on the parent is **not**; see §8. | S | NEW |
| A12 | **Decoder micro-work:**<br>- gray 9/7 JPX takes the fused final level (`reconstruct.rs:589-715`);<br>- drop the JPX output pre-zero (`jpx.rs:570-574`);<br>- vectorise Tier-1 dequantisation (`t1.rs:404-416`; 17 of ~170 ms at full resolution);<br>- keep the MQ A/C registers in locals per pass;<br>- batch JBIG2 RENORMD (`arith.rs:93-104`). | Code plus jpxbench tables | ~5–7 ms per gray layer; a few % of Tier-1. **A/B the MQ and RENORMD changes:** neighbouring MQ micro-changes measured slower before. | Exact | S each | Gray fused path PREV (`decode-optimize.md` §18); rest NEW |
| A13 | **Make "serial" serial** (B7). | Code | Comparable benchmarks; less contention with lege's encode pool | Exact | S | NEW |
| A14 | **Image-cache budget per consumer:** a budget option on `CpuBackendOptions` and the `lege-pdf-read` API. One-pass runs (scheduler tools, DjVu/EPUB/colour-fallback single renders) get a small budget; lege's planned path keeps within-page reuse. | Peak 84 → 29 MiB (colour scan) and 86 → 54 MiB (JPX) with the cache off, no speed loss | ~50–90 MiB per process on one-pass runs | Exact once B1 is fixed (today, cache residency changes output) | S | NEW |
| A15 | **ICC-RGB per-byte TRC LUT and table-byte return** (`icc.rs:135-158`; `image.rs:776-797`). Apply the same to the compile-time Flate ICC conversion, which today runs per texel with no memo (`interpret.rs:3655-3680`). | 15–25 ns/texel | 4–6× on the conversion | Exact (f32 emulation over all 256 values) | S | NEW |
| A16 | **DeviceCMYK:**<br>- call `adobe_cmyk_to_srgb_u8` directly (both round trips are identities);<br>- add `/Decode` byte LUTs;<br>- cache per draw identity. The deferral's precondition, the same CMYK image painted more than once per page, is now met by lege's analysis, base and OCR renders. | 12 ns/px; 132 ms on one cover | 3–6× on the conversion | Exact while the draw stays on its current path; promotion changes the border ring | S–M | PREV (`cmyk-source-conversion-cache`) |
| A17 | **AES:** a T-table or in-place CBC implementation; AES-NI later (`pdf-security` `aes.rs`). | ~50–100 cycles/byte; ~30–50 ms per 2 MB encrypted image | AESV2/V3 documents | Exact | S / M | NEW |
| A18 | **Add 16-bit Gray/RGB/CMYK to the promotion family** (`image.rs:714-728`). Today every bpc-16 draw takes the 30–250 ns/px generic loop. | Code. pdfTeX embeds 16-bit PNGs as 16-bit. | Large on those pages | Exact via `pixel()`; border-ring caveat | S | NEW |
| A19 | **PNG encode for print staging, preview and the FFI:** use `Compression::Fast`; add a raw-pixel FFI entry (`read/src/export.rs:330-349`; `lege-render-ffi/src/lib.rs:541-597`). | A level-6 adaptive encode of a 26 MB sheet likely costs more than its render (not measured) | Print and FFI latency | Pixels unchanged; PNG bytes change | S | NEW |
| A20 | **Text cold path:**<br>- hoist the Type 1 encoding search (`type1.rs:443-473`; 0.4–0.9 ms per font);<br>- hash each font program once per `Arc` per document (prepared.rs:4094-4117);<br>- dedupe misses within a run;<br>- O(1) glyph-cache eviction (prepared.rs:452-464). | Code | Cold pages and short documents (beans) | Exact | S | Reconcile with the 07-20 result: retaining cheap programs added +4% RSS for no gain |
| A21 | **Surface reuse:** 2–3 slots plus a stats counter (`surface.rs:346-357`). | lege alternates analysis and base sizes on shared threads | ~0.1 ms per miss on Linux; Windows unmeasured | Exact | S | NEW |

### Tier B: medium effort, high value

| # | Change | Evidence | Expected gain | Output | Effort | History |
|---|---|---|---|---|---|---|
| B1 | **`/ImageMask` stencil fast path.** Treat the stencil as a 1-bit image with LUT {paint: [rgb, 255], masked: [rgb, 0]}. Use the summed-area/row-band minify path and a transparent-skip nearest path, keeping the generic body on the anti-aliased border band (`exec.rs:768, 818, 847`; `image.rs:1319-1346`). | ~90% of MRC page time is in the generic sampler, 3.4–3.6× slower than Poppler. Lege's own MRC output uses stencils (`lege-process/core/accumulator.rs:37`). | Several × on MRC and stencil pages: scanned books, DjVu conversions, Lege's own output | Exact for solid-colour paint; shading/pattern brushes stay generic | M | NEW (the 2026-11-06 change covers `/Mask` stencils only) |
| B2 | **Bilevel summed-area table → row-band windowed-popcount filter** (`exec.rs:1331-1460`). | SAT build plus lookup ~90% of a bilevel page; 33.7 MB per draw (72% of peak); 267 MiB at 2+6 workers; 600-dpi pages fall back to a serial path | ~2× on bilevel pages; −34 to −135 MB per concurrent draw | Exact (same integers) | M | PREV (surface-imagerows note) |
| B3 | **Decode-level-keyed JPX/JPEG cache plus a per-page decode reference.** Implemented on `claude/jolly-ride-cyjbsm`. | That branch: analysis+base 569 → 398 ms on a synthetic JPEG book. JPX analysis and base renders miss each other today. | 15–35% of render CPU on MRC/JPX pages in lege (estimate) | Base, region and OCR exact. The analysis raster changes, so **run a layout-detection A/B on a GPU first.** | M | NEW |
| B4 | **Native gray kernels:** nearest, bilinear, area-min and masked kernels generic over 1 or 3 channels, so promotion goes away. | A5 data | ~⅓ of area-min work on gray scans | Exact | M–L | NEW |
| B5 | **Scheduler** (benchmarks and the published `render_all`):<br>- persistent worker contexts, plus a buffer pool that takes surfaces back when the `HostPage` drops;<br>- fused compile+render workers or work stealing;<br>- a per-request thread budget in place of the global `PAGES_IN_FLIGHT`. | The 2+2 default is the slowest configuration on scans (MRC 444 ms at 1+1 vs 736 at 2+2). On one core it is 1.3–1.5× slower than a serial loop. | Benchmark and published-crate throughput. No production consumer uses the scheduler. | Exact | M | PREV (outside review §12; optimization-plan §5.9) |
| B6 | **Fast global allocator** (mimalloc behind a feature) in lege, lege-gui, pdfr, the agent and the bench adapters. **Fix B2 first:** with mimalloc, B2 fired in 10 of 10 runs. | Round 4: halved the median compile time | *Allocator probe pending.* | Exact | S code / M validation (C toolchain for cross builds) | PREV (not adopted in round 4) |
| B7 | **Zero-copy tokenizer and operand moves** (`pdf-syntax/src/lexer.rs:227`; `pdf-content/src/tokenizer.rs`; `interpret.rs:923, 2107, 1705, 4843`; `lower.rs:848` capacity). | ~4,350 allocations per text page; ~10 per text-show operator; `FontMetrics::decode` runs twice | ~1 ms per text page; more under allocator contention | Exact | M | `lexer-name-bulk-scan` PREV; rest NEW |
| B8 | **Document-scoped ICC profile and function caches** (`interpret.rs:581-583, 3474-3534, 5526-5700`). | 1–3 ms per profile per page | PDF/X and press files | Exact (keep decode-budget charging) | M | NEW |
| B9 | **Glyph cache under contention:** an L1 cache per worker, local counters, padded shards, a document-wide outline cache. | beans scales only 1.25× from 1 to 8 workers | Multi-worker text | Exact | M | PREV (round 2, "probe per glyph") |
| B10 | **Shading spans:**<br>- hoist per-column and per-row terms when b = c = 0 (exact, since `(a·x + c·y) + e` with c = ±0);<br>- clamp `/Background` mesh layers to the draw bounds;<br>- build mesh layers at execution time. | `shade_pixel` 17.6% on the transparency page | 2–4× on axial shading spans | Exact | M | PREV (prepared.rs:930-931) |
| B11 | **Memory bounds:**<br>- byte-bounded `ParseContext` (B8);<br>- a bounded name interner;<br>- release parked surfaces when a thread goes idle or a job ends;<br>- drop clip masks after their last use and prebuild in waves under a byte budget. | §6 | Peak and steady memory | Exact | M | Partly PREV |
| B12 | **JPX coefficient planes as a strip ring** at the finest level (`t1.rs:150-167`; `stream97.rs`). | 39 MB per layer at 144 DPI; 155 MB at full resolution | ~¾ less decode memory per in-flight JPX | Exact | L | PREV (audit §5.4; phase 5) |
| B13 | **ISA policy:** `-C target-cpu=x86-64-v2` for distribution builds (Windows 11 24H2 already requires SSE4.2/POPCNT), or `#[target_feature(enable="sse4.1,popcnt")]` multiversions of the raster fill, sampler, shading and Gray8 loops. | **Measured:** the baseline build has 290 out-of-line rounding calls in 60 functions; v2 has none. **Output bit-identical across all 624 page/scale/format renders.** | **Measured, single-thread, paired over 7 rounds:** MRC −9% (72 DPI) and −15% (144 DPI), vector −8 to −9%, text with cache misses −9%, text compile stage −24%. **Caveat:** v2's LLVM tuning splits 256-bit unaligned loads in the existing AVX2 kernels, making JPEG scans ~2% slower. `-C target-feature=-slow-unaligned-mem-32` restores parity and stays bit-identical (936/936), but it is an unstable LLVM flag that rustc warns about. | Exact (measured) | S flag / M–L multiversion | NEW for the renderer; jp2lam declined target-cpu flags for portability |
| B14 | **PGO** (cargo-pgo trained on the perf-manifest classes), plus BOLT for the AppImage. | Large, branchy hot functions (`Interpreter::run` 41.5 KB, `lower_impl` 33.9 KB); no `#[cold]` anywhere | Typically 5–15% on interpreter, parser and entropy-decode work | Exact | L | NEW (jp2lam audit §12.3, unfunded) |
| B15 | **ARM:** enable jp2lam `simd` in every build (S); NEON kernels for JPEG IDCT/colour, `blend_mask`, `convert_coverage`, bilinear and Gray8 output (L). | Android is arm64-only with no NEON code; pdf-cli/FFI/agent builds get scalar 5/3 and colour transforms | Android, Apple Silicon | Exact (separate mul/add, no `vfma`) | S / L | PREV |

### Output-changing options (each needs a knob and a severity gate)

- **Adaptive path flattening** (`prepared.rs:1271-1274`, `CURVE_SEGMENTS = 16`). 2–8× fewer edges on small curves. Curves larger than ~48 px get *more* segments, which interacts with the stroker.
- **An outline stroker.** Today every flattened vertex gets a 16-gon round join, 240 extra edges per curve, and overlapping anti-aliased polygons darken the joins.
- **Asymmetric glyph sub-pixel phases** (`GLYPH_SUBPIXEL=4,1` or `4,2`). Never measured; only the N×N sweep was.
- **Rotated text through the glyph cache.**
- **A native Gray8 surface.** 4× less surface memory and clear cost. Exact only on all-neutral pages: DeviceCMYK black text comes out ±1.
- **Progressive JPEG decoded DC-only at 1/8** (±1 LSB).
- **A deterministic pure-Rust libm** for CalRGB, TRC, Type 2 and Type 4 (S8).

---

## 6. Memory plan

| Consumer | Size | Where | Lever |
|---|---|---|---|
| Decoded-image cache | Up to 96 MiB per backend. With the cache off: 84 → 29 MiB peak (colour scan), 86 → 54 (JPX). | `prepared.rs:725-732` | Per-consumer budget (A14). Size it after B1, because DCT entries keyed by scale multiply. |
| Bilevel summed-area table | 33.7 MB per draw at 300 dpi, up to 135 MB at 600 dpi; one per concurrent draw | `exec.rs:1331-1393` | Row-band filter (B2) |
| Gray promotion | 2 × 3n transient per draw (12.6 MB for a 2.1 Mpx image) | `image.rs:673-697` | A5, then B4 |
| JPX coefficient planes | 39–155 MB per in-flight decode | `t1.rs:150-167` | Strip ring (B12) |
| Scheduler reorder buffer and compiled queue | Up to 4 × workers finished surfaces (31 at 2+6, 79 on 20 CPUs), plus Flate samples in the uncharged queue | `pdf-render-scheduler/src/lib.rs:90-94, 404-433` | Unordered delivery; hold permits until emit; charge compiled pages. Scheduler tools only. |
| Parked surface per thread | One full page per rendering thread after every render (139 MB at 600-DPI print, 557 MB at 1200) | `surface.rs:252-259, 285-287` | Release when idle or when a job ends; capped pool |
| `ParseContext` | Unbounded per thread; decrypted bodies owned | `pdf-document/src/resolve.rs:57-79, 178-182` | Byte-bounded LRU (B11) |
| Name interner | Grows with every content-stream name (≤5M names per page) | `pdf-object` interner | Cap it, or look up instead of interning |
| Clip masks, mesh layers | Live for the whole page; the sum of all mask envelopes; `/Background` mesh layers span the full page | `exec.rs:95-103`; `prepared.rs:5821-5851` | Drop after last use; clamp layers |
| Print | Composed `--to-file` jobs hold every sheet (B12); `compose_sheet` builds whole sheets; Windows makes a BGR DIB copy | `print/src/spool/file.rs:176-186`, `compose.rs:267-293`, `spool/windows.rs:871-908` | Stream sheets |
| Input file | Whole file plus an `Arc` copy; opened twice | `lege-process/pipeline/config.rs:97-118` | A9. mmap is blocked: a SIGBUS handler would be process-global, and Windows locks mapped files. |
| Glyph-cache accounting | Charges 96 B per entry vs ~170–290 B real | `prepared.rs:452-464` | Honest charge |

**The 1,548 MiB maximum** (8 workers, 144 DPI, 2026-11-06) cannot be attributed here without the private corpus. Plausible contributors at 2+6 workers:
- reorder-window surfaces (up to 32 × a page surface);
- six concurrent JPX decodes at 39–155 MB each;
- six summed-area tables at 34–135 MB each;
- the 96 MiB image cache;
- Flate samples in the compiled-page queue.

To attribute it, rerun the heaviest document with `PDF_RENDERER_IMAGECACHE=off`, reorder window 12 vs 32, compiled-queue depth 2 vs 12, and byte gauges on the reorder buffer and the queue.

---

## 7. Fix the measurement setup before the next round

1. **The benchmark path is not the product path.**
   - The scheduler tools build a fresh `CpuWorkerContext` and `ParseContext` for every page (`pdf-render-cpu/src/lib.rs:1280-1286`; `lege-render-demo.rs:149`). Production `lege-pdf-read` keeps both per thread.
   - So surface reuse never engaged in the 2026-09-04 whole-document control, nor in the 2026-11-06 comparison. That control's "retaining the buffer costs nothing net" is therefore untested.
   - Until a production-like mode is added (or B5 lands), wins and losses measured with `pdfium-diff` do not carry over to `lege`.
2. **`lege-render-demo` misreports** (`lege-render-demo.rs:94-101, 123, 175, 185`).
   - Its font-scan timer wraps a lazy constructor.
   - Its 4-slot PNG channel blocks the collector, so encode stalls are counted as render time.
   - It has no worker knob.
3. **Build parity.** `pdfium-diff` is its own workspace and builds with cargo's default release profile: no fat LTO, 16 CGUs (`tools/pdfium-diff/Cargo.toml:69-72`). It also builds jp2lam without `simd`, which `lege` enables.
4. **"Nested parallelism off" was not off** for RGB JPX in the 2026-11-06 8-worker cells (B7). Correct the comparison note.
5. **The border ring is invisible to the equivalence tests.**
   - Fast-path tests compare against a `shade()`-only reference that has no edge coverage (`exec.rs:5767-5789`).
   - Before any further generic→fast moves, add fractional-placement tests against the full generic body (`generic_masked_reference`, `exec.rs:5179-5208`).
   - This has been known since 2026-08-08.
6. **Determinism tests at more than one size and order.**
   - Every image-cache test uses a single 16×16 JPEG at one size (`prepared.rs:6648-6654`). That is how B1 shipped.
   - Add a test that renders a page set in two orders, at 1 and N workers, through one backend, and compares per-page hashes. It would have caught B1–B3.
   - Add one that renders small then large through one cache (B1).
   - Add one with alternating fonts on pages that share an address (B2).
7. **Gray8 output has never been timed.** Every `render.output` figure is for RGBA, but `lege` uses Gray8 on gray pages.
8. **Add a standing regression gate.** History shows the 2× JPX headroom silently disabled reduced decoding for months. A small timed manifest with thresholds, run on every renderer change, would have caught it.
9. **Corpus gaps.** Add CJK, Type 3, ICC/CMYK/Separation, encryption, object streams, annotations, JBIG2 generic/halftone, 16-bit images and rotated pages. Several bugs sit exactly in these gaps (B4, B5, B6, B10).
10. **Windows numbers.** NT-heap and VirtualAlloc costs were never measured. The "49–54 ms fresh vs 3.3 ms reused" page-fault figure quoted as Windows evidence is a Linux glibc measurement (`../performance-history/optimization-surface-imagerows-20260904.md:68-77`).

---

## 8. Not recommended (and why)

- **A GPU image renderer for `lege-process`:**
  - It declines Gray8 output and crops (`pdf-render-wgpu/src/lib.rs:1937-1942`).
  - At 1.1 Mpx the CPU composite takes ~2.5 ms, while a GPU round trip costs 1–4 ms; decoding stays on the CPU and is 10–30× larger.
  - The plan's speedups were measured against July's CPU numbers.
  - It would share one GPU queue with layout detection and binarisation.
- **Full GPU rasterisation (vello-style):**
  - The path-batch prototype ran at 0.81× CPU, and the CPU text and vector paths have since become 2.4–2.9× faster.
  - Bit-reproducibility across GPU vendors is at risk.
  - It would be a second reference renderer.
- **GPU JPX or JPEG decode:** entropy decoding stays on the CPU, and it dominates (Tier-1 is 64 of 82 ms at one reduction level).
- **Region-of-interest decode for whole pages:**
  - JPX and JPEG already reduce resolution.
  - jp2lam's ROI saves only Tier-1 blocks, not the planes or the coarser IDWT levels.
  - CCITT and JBIG2 decode sequentially.
  - Revisit only for viewer tiles, with region-aware cache keys.
- **Changing the surface format** (planar, RGB8, BGRA): about 10K lines of kernels, shaders, C ABI and hash gates, for no measured gain. Fix the boundary copies instead (A9).
- **Multi-process rendering or cross-process caches:** eight PDFium processes already use ~1.8× the memory of one 8-worker Lege process.
- **Running a transparency group's ops directly on the parent** (proposed during this review): not exact.
  - The raster clamps edges to the offscreen's width (`exec.rs:4464-4465`; `raster.rs:1125-1130`).
  - So coverage in the group's last columns would change.
  - The exact sub-steps in A11 are the way to go.
- **Ideas tried and rejected before:** see Appendix C.

---

## 9. Suggested execution plan

The history favours a particular way of working:
- sequential rounds in a clean main checkout;
- worktree baselines;
- interleaved A/B on pinned cores;
- the 3,698-render hash gate;
- each round recorded in AKR as it lands.

| Round | Contents | Gate |
|---|---|---|
| 0: harness and tests | §7 items 1–7: production-like bench mode, demo reporting, profile parity, border-ring and multi-order determinism tests, a Gray8 timer | The new tests fail on HEAD for B1–B3 and pass after the fixes |
| 1: correctness | B9 first (crash). Then B1: merge the sibling branch after a GPU layout-detection A/B, plus B6. Then B2–B5, B7, B10–B12 and FE1. B13–B15 when convenient. | Corpus hashes, with documented intended changes for B1, B3, B4, B5, B6 and B10, plus the new determinism tests |
| 2: quick exact wins | A2–A21 | Hash-identical; interleaved A/B per item |
| 3: medium | B1–B11 | Hash-identical; peak-RSS gauges |
| 4: structural | B12–B15; output-changing options behind knobs | Severity against PDFium on the 60-page set |

---

## Appendix A. Artefacts

- **Raw data.** The measurement harness, scripts, generated corpora, profiles and bug-reproduction inputs from this review lived in a session scratchpad and are not committed.
- **Rebuilding the corpus.** The synthetic corpus can be regenerated with reportlab and Pillow, as described in §3.3: dense Latin text in TrueType, standard-14 and Type 1 fonts; vector pages; 300-dpi JPEG scans; Pillow G4 bilevel pages; transparency; MRC with a G4 or JBIG2 `/ImageMask` over JPEG/JPX.
- **Probe tool.** Each probe used a small out-of-workspace crate with path dependencies on the renderer crates. It ran sequential renders through one backend, whole documents through `RenderScheduler::render_range`, and printed per-page hashes. A tool of this shape is what CRATES-IO-PLAN §9 asks for as `lege-pdf/bench`.

## Appendix B. Verification record

Verdicts: C = confirmed, P = partly (real but corrected), R = refuted, N = needs runtime, D = already done.
Paths are under `lege-pdf/render/crates/` unless stated.

**Raster core**

| ID | Finding | Verdict | Correction or key note |
|---|---|---|---|
| R1 | Transparency groups: offscreen per group, f32 composite, quadratic knockout | P | (a) Running the group's ops on the parent is **not** exact. The raster clamp is the offscreen's right edge (exec.rs:4464-4465; raster.rs:1125-1130). (b) The opaque copy path is exact: brute force over 8.4M cases, Normal blend. (c) Bounds-limited knockout is exact. (d) The pool needs measurement. (e) The content-bounds half is already done. |
| R2 | Fills ignore the clip mask's `all_opaque`; masks live until page end | C | Also new: `render_tiling` bypasses `mask_source` (exec.rs:3851-3866) and builds duplicate masks. |
| R3 | Paths flattened at a fixed 16 segments | C | An output change. Curves larger than ~48 px get *more* segments. Interacts with R4. |
| R4 | Strokes: a quad per segment, a 16-gon per round join, `sincosf` per join | C | The lookup table is exact only if filled at runtime with the same expression. The bigger cost is downstream: 240 disk edges per flattened curve. |
| R5 | Shading spans recompute every term per pixel | C | Separable only when b = c = 0 exactly; radial is not separable. Clamp `/Background` mesh layers to the draw bounds. |
| R6 | Tiling re-lowers every tile | P | Tile skipping is exact only with a footprint derived from content. Cells are never clipped to `/BBox`, a conformance gap. |
| R7 | Compact-window pass 2 is dense | P | One interval per row is already tracked. `fill_into(dst, stride)` is the valuable half. |
| N2 | Surface reuse never engages under the scheduler | C | Scheduler tools only; production reuses. The scheduler also parks an alias of the returned page, so a fix needs return-on-drop. |
| B14 | Raster right-edge clamp makes offscreen content render differently from direct drawing | P | Confirmed for groups and soft masks. For tiling, the effect is at the fill's left/right edges, not between tiles. Errors are up to 114/255 on shallow edges. |

**Images and colour**

| ID | Finding | Verdict | Correction or key note |
|---|---|---|---|
| IM1 | ICC-RGB promotion in scalar float | C | The LUT and table-byte return are exact (f32 emulation). Compile-time Flate ICC conversion has the same issue. |
| IM2 | Bilevel summed-area table at 4 B per texel per draw | C | 600-dpi pages always take the serial fallback. |
| IM3 / N4 | Gray8 images promoted to RGB8 per draw, uncached | C | Also exposes bug B5. The AKR decision gives no technical reason for leaving it uncached. |
| IM4 | `/ImageMask` stencils never take a fast path | C | Measured at ~90% of MRC page time. Exact only for solid-colour paint. Lege's own MRC output uses stencils. |
| IM5 | CMYK conversion round-trips and per-tap conversion | P | Calling the conversion directly is exact. Promoting DeviceCMYK changes the border ring. |
| IM6 | Quarter-turn placements take the generic loop | P | Exact for the index and integer kernels. Bilinear needs a vertical-first variant, otherwise ±1 LSB. |
| IM7 | Promotion key re-hashes samples on every draw | C | A follow-up already documented. |
| IM8 | Clip masks, soft masks, alpha and blends skip the fast paths | C | Large effort. |
| IM9 | Progressive JPEG at 1/8 runs the full IDCT | C | ±1 LSB output change. |
| IM10 / FE1 | `sc`/`scn` rebuild image colour spaces | C, understated | Can exhaust the decode budget and drop content. |
| IM11 | Fast-path tests cannot see the border ring | C | Known since 2026-08-08. The AKR claim `promotion-is-exact` is wrong for unmasked fractional placements. |

**Codecs**

| ID | Finding | Verdict | Correction or key note |
|---|---|---|---|
| C1 | Gray 9/7 JPX skips the fused final level | C | Previously identified (`decode-optimize.md` §18), not new. |
| C2 | Coefficient-plane memory | P | The zero-stripe skip saves ~nil memory (calloc). The strip ring is the real lever. |
| C3 | MQ decoder fast path | C | Keeping A/C in registers is the first exact step. Must be A/B'd. |
| C4 | JBIG2 RENORMD is bitwise | C | Slow path only. Neighbouring MQ micro-changes have measured slower before. |
| C5 | JBIG2 MMR per pixel | C | MMR is rare in the profiled masks. |
| C6 | Tier-2 leftovers | P | ≤ ~2.4 ms per layer. |
| C7 | No region-of-interest decode | P | Large effort. ROI does not shrink the planes. |
| C8 | Refinement decoder per pixel | C | How often it is used is unknown. |
| C9 | Cancellation only at decode boundaries; separate jp2lam pool | C | Serial decodes contend with Lege's encode pool. |
| C10 | Globals cached; JPX output written into the Arc | C | Globals use a single-entry cache. The JPX output pre-zero is unnecessary. |
| B7 | "Serial" JPX decodes fan out | C | Also 5/3 `fuse_pack_i32_into`. Gray8 and sYCC stay serial. |

**Parsing, interpreter and IR**

| ID | Finding | Verdict | Correction or key note |
|---|---|---|---|
| FE2 | ICC profiles and functions cached only per page | C | Production also re-inflates profiles every page. |
| FE3 | Tokenizer and operand allocations | C | ~1 ms per text page. |
| FE4 | No document-level object cache | P | Only demo/bench re-parse per page; production re-parses once per thread. |
| FE5 | Byte-wise AES | C | AESV2/V3 only; 30–50 ms per 2 MB encrypted image. |
| FE6 | Nested streams re-decoded on every use | C | Shadings (`sh`) have no cache at all. |
| FE7 | Name interner | P | One lock per document, not global. Grows without bound. |
| FE8 | Function evaluation allocates per call | C | 5–10 ms per 65,536-entry LUT. |
| FE9 | Flate image buffers | C | 3–5 ms plus 2× peak per 26 MB image. |
| FE10 | Smaller parser and lowering items | P | `from_pairs` is O(n²) only for adversarial key orders. |
| N1 | Stream window ladder rescans | C | Production cost is once per thread per stream. Latent truncation bug. |

**Text and fonts**

| ID | Finding | Verdict | Correction or key note |
|---|---|---|---|
| T1 | Type 1 encoding loop is quadratic | P | Overstated 5–15×: really 0.4–0.9 ms per font. |
| T2 | Font identity rebuilt per page | C | Hash once per Arc. Content identity is still needed. |
| T4 | Shared-cache probe per glyph | C | Measured as SipHash at ~10–15% of instructions. |
| T5 | Cold misses do redundant work | C | Cold keys only. |
| T6 | Rotated text bypasses the glyph cache | P | Depends on the composed matrix. An output change. |
| T7 | Type 3 CharProcs decoded per occurrence | C | Doc-wide caching is blocked by the resources fallback. |
| T8 | Dense miss rasteriser | C | Small effect. |
| T9 | Asymmetric sub-pixel phases | N | Severity sweep needed. |
| T10 | O(n) eviction | C | Only at cap. The per-entry charge undercounts. |
| T11 | CJK system-font cliffs | C | Opt-in system fonts only. |
| T12 | Compile-side text allocations | P | Tens to hundreds of µs per page. |
| T13 | Hinting instance rebuilt | P | Dormant (hinting is off). |

**Scheduler and threading**

| ID | Finding | Verdict | Correction or key note |
|---|---|---|---|
| SC1 | Scheduler discards worker contexts | C | Benchmark and demo path only. |
| SC2 | Fixed compile/render split | C | A known deferral. Windows `available_parallelism` ignores affinity. |
| SC4 | Global `PAGES_IN_FLIGHT` | C | Undercounts pages in lowering and decode. |
| SC5 | Ordered delivery only | P | lege-process does not use the scheduler. |
| SC6 | Render-side font parse has no single-flight | P | A one-time herd, not per page. |
| SC7 | Thread topology | P | Not oversubscribed in steady state; three leaks. |

**Build and toolchain**

| ID | Finding | Verdict | Correction or key note |
|---|---|---|---|
| S1 | libm rounding calls on baseline x86-64 | P | Vectorisation is blocked only in Gray8 output. Many sites can be rewritten exactly without changing ISA policy. |
| S2 | Gray8 output calls `roundf` | C | Replace only the `round()`; no tables needed. |
| S3 | No global allocator | C | Fix B2 first. |
| S4 | PGO | C | Large effort. |
| S5 | SipHash in hot maps | P | Only the glyph cache is hot (~12% of instructions on text). |
| S6 | jp2lam `simd` differs per binary | P | The AVX2 9/7 rows are independent of the feature. |
| S7 | `pdfium-diff` profile parity | C | |
| S8 | glibc transcendental functions vary across CPUs | C, medium | Rare. Use a const table for `push_disk`; a pure-Rust libm is an output change. |

**Integration and consumers**

| ID | Finding | Verdict | Correction or key note |
|---|---|---|---|
| I1 | lege-process renders 2–4 products per page | P | Typically 2. Only JPX is re-decoded; DCT is shared at the wrong scale (B1). |
| I2 | Read-seam copies | C | 3 copies, 4 for the analysis image. |
| I3 | File copy and double open | C | One extra copy plus a second xref walk. |
| I4 | Single-slot surface reuse | P | About half the renders hit; ~0.1 ms per miss on Linux. |
| I5 | Image cache admits everything | P | lege needs within-page reuse, so the budget must depend on the consumer. |
| I6 | Decode and flatten before culling | P | Image pre-cull is S and exact. |
| I7 | Page cache sized by count, not bytes | C | A knob exists. |
| A1 | Banded execution | P | 1200-DPI print is not the default. compose_sheet is whole-sheet. |
| A2 | Native Gray8 surface | C | Exact only on neutral pages. |
| A3 | Not-to-do evidence | C | Cite the audit doc, not the abandoned AKR record. |
| A4 | Fused worker pool | P | The gap is ~4% across the whole table: needs a runtime A/B. |
| A5 | mmap input | C | Blocked by policy. Reading straight into an Arc is the safe win. |

## Appendix C. Tried and rejected (performance history)

Citation shorthand:
- `PH/x` is `performance-history/optimization-x-<date>.md`.
- `perf:` and `eco:` are AKR record ids, prefixed `lege-ecosystem-perf.` and `lege-ecosystem.` respectively.
- `DEF` is `handoffs/DEFERRED.md`.

| Idea | Result | Citation |
|---|---|---|
| AVX2 coverage conversion in RasterKernel | Slower in 8 of 9 microbenchmarks: the prefix sum cannot be SIMD and stay bit-exact. Still off (`AVX2_MIN_SPAN=usize::MAX`, raster.rs:70). | PH/raster "AVX2 (A3)" |
| PDFium-style integer origin snapping for glyphs | Severity 0.0894 → 0.1313. Kept 4×4 sub-pixel buckets instead. | PH/glyphcache B2 |
| Adopting fontdue | Rejected: no arbitrary transforms and no outline feed. Its techniques were ported. | plans/PLAN-TEXT-GAP |
| Text Phase C (flatten cache, run batching) | Skipped as low value. | PLAN-TEXT-GAP |
| Tiling lower-once / translate-many | Not byte-identical. | DEF, patterns, 07-21 |
| Incremental affine image stepping | No gain (271 vs 279 ms) and moved 6,479 px. Reverted. | DEF "Affine §11" |
| Font cache without the 400 µs parse gate; pointer-identity key | +4% RSS, no gain, 0 cross-page hits. Pointer identity was tested before FontResourceCache existed. | PH/renderer3 Task 2 |
| Summed-area table for area-min; averaging CMYK before converting | The SAT costs as much as the direct sum plus 120 MiB. Pre-averaging breaks byte identity. | PH/rgb8-areamin |
| Unconditional image-row fan-out | −31% whole-document throughput. | PH/surface-imagerows |
| Inlining big painters into `paint_image`; skipping `edge_coverage` | A sibling path regressed 22.6 → 31.1 ms. Skipping moved 188 border pixels. | PH/mrc-mask; perf:renderer.mrc-masked-draw-fast-path |
| Reciprocal division; regrouped tap loop; ⅛ dense threshold | Slower, ~10% slower, no change. | PH/image-blit R3 |
| Zero-padded AVX2 blend tail; AVX2 `fill_opaque` | 2× scalar cost on short rows; store-bound. | PH/text-vector R2/R3 |
| AVX2 horizontal pass for masked area-min; branch-free box average | 2% of the profile, so not built; neutral (memory-bound). | PH/round4 |
| mimalloc; deeper compiled queue; reorder window above 32 | mimalloc halved compile time but was not adopted; the other two gave nothing. | PH/round4 |
| 12-bit JPEG Huffman table | 2.1% slower. | PH/jpeg2 |
| Integer AVX2 IDCT | Not byte-identical. | PH/image-blit R2 |
| JPX on the global rayon pool; Budgeted(4); margin above 1.0 | Oversubscribes; MRC regresses; loses the reduction. | PH/jpx-integration |
| 2× JPX headroom | Silently disabled the reduced decode for months. Removed (494 → 191 ms/page). | perf:renderer-headsup.jpx-headroom-fix-20261104 |
| jp2lam f32x8 fused pack | Neutral under fat LTO: "do not re-land". | perf:assessment.fused-pack-simd-neutral |
| Direct-Arc JPX output | No speed gain; kept for memory. | perf:evidence.structures-direct-vs-owned-output/3 |
| Tier-1 `get_unchecked`; jp2lam `simd` | Neutral (±5%). | PH/jp2lam-serial R2 |
| JBIG2 context preload; `inline(never)` on renorm | Slower in both cases. | PH/jbig2 |
| GPU for minified 1-bit images | The CPU wins. | DEF, GPU section |

## Appendix D. Suggested AKR records

Run these with the `akr` CLI where it is installed. Do not hand-edit `.akr` files.

- **Observation** `lege-ecosystem-perf.renderer.scoping-20261108`: the summary of this note. Watch `pdf-render-cpu/src/{prepared,exec,image,surface}.rs`, `pdf-image/src/{jpeg/mod,jpx}.rs` and `lege-pdf/read/src/session.rs`.
- **Supersede** `lege-ecosystem-perf.renderer.gray8-draws-take-the-rgb8-fast-paths/1`. Its claim `target-only-for-jpx` is false: DCT also honours the target. Its claim `promotion-is-exact` is false for unmasked fractional placements, because of the border ring. The sibling branch's AKR draft supersedes the first claim but restates the second.
- **Work records**, one each:
  - Correctness: B2, B3, B4, B5, B6, B7, B9 and FE1 (decode-budget content loss); B1 is in progress on the sibling branch.
  - Measurement setup: the items in §6.
  - The stencil fast path (Tier B1).
  - The bilevel row-band filter (Tier B2).
  - The image-cache budget (A14).
- **Papercuts:**
  - `akr` is not installed in cloud containers, so evidence cannot be recorded in-session.
  - `lege-render-demo` misreports its timings (N3).
  - `pdfium-diff` does not use the shipped profile (S7).
  - `/usr/bin/time` and `perf` are missing in the container, so peak RSS was read from VmHWM.

