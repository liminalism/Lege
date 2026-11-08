# Local continuation of the 2026-11-08 renderer scoping review

Written 2026-11-08 on the Windows dev host, after the cloud session landed the
first half on `claude/dazzling-planck-z44poy`. Sources: that branch's
`IMPLEMENTATION-LOG.md`, the scoping plan `plans/optimization-scoping-20261108.md`,
and three scout reports in `scouting/` (`scope-integration.md`,
`scope-correctness.md`, `scope-perf.md`).

## 1. Where things stand

| Branch | Forks from | Contents | State |
|---|---|---|---|
| `origin/claude/dazzling-planck-z44poy` | 5b24f8e4 | B2 B3 B4 B5 B7 B10 B13 FE1; A2 A3 A4(libm) A5(Arc) A10 A11a/b; RGB8 packing; demo fixes; ring test; scoping docs + harnesses | 20 commits, hash-gated on 2,364 renders |
| `origin/claude/gifted-pascal-eyn4qw` | 5b24f8e4 | B9 Type 1 hardening (includes A20's encoding-search hoist) | 5 commits |
| `origin/claude/jolly-ride-cyjbsm` | 5b24f8e4 | B1 decode-level cache keys, colour space in the key, per-page decode reference shared by analysis/base/OCR renders | 8 commits; **changes the analysis raster**, so the plan requires a GPU layout-detection A/B before it merges |
| local `lege-pdf-agent-over-poppler` | = main = 5b24f8e4 | uncommitted GUI (app.rs, backend.rs) and AKR edits | untouched |

**Trial merge** (dazzling, then gifted, then jolly) exists at
a scratch worktree (trial HEAD 16a635cc; since redone on this branch as 05e1ded1, 5763a86b, 27debaee):
- 3 textual conflicts, all trivial: pdf-cli `tests/pipeline.rs` (two new tests at one spot), `Cargo.lock` (foldhash + jp2lam deps), `lege-pdf/render/CHANGELOG.md` (two new top sections).
- 1 compile fix: jolly added `decode_floor` to `CpuPreparedPage`; dazzling's ring test builds a literal, so it needs `decode_floor: None`.
- `cargo check --tests` and `cargo test` on pdf-render-cpu, pdf-content, pdf-font, pdf-image, lege-pdf-read: 55 suites, 665 passed, 0 failed.
- Not yet compiled: `-p lege` (jolly edits `lege-process/pipeline/page_output_plan.rs`) and the pdf-cli tests.
- No stale "only for JPX" comments remain. A5 (draw-time Gray8 to RGB8 into one Arc) and jolly's compile-time `promote_through_cache` are independent code paths.
- Worktree caveat: fresh worktrees check out CRLF and break pdf-text's digest check. Create them with `git -c core.autocrlf=false worktree add --detach ...`.

## 2. Decisions needed from the owner

1. **Merge go-ahead and target.** Recommend merging dazzling and gifted into `lege-pdf-agent-over-poppler` now (both gated), then jolly after step 3.1.2.
2. **jolly-ride gate.** Run the layout-detection A/B locally (this host has the GPU): detections on the analysis render before/after on beans, viaduct, vogtle, xenosystems and the sahib outputs; accept if region classes and boxes match within tolerance. Then merge.
3. **Border ring.** Opaque unmasked image fast paths draw a hard 1-px edge at fractional positions; everything else is soft. Options: (A) soften the fast paths' ring (M, about 1 ms per image page, ring change up to about 32/255, no change for page-aligned scans; also exposes the generic bilinear alpha-254 quirk to fix); (B) harden the generic body (S-M, reverses A10 and the 2026-07-21 rotated-edge AA). Recommendation: A, after one PDFium probe of a fractional opaque image upright and rotated; if PDFium snaps axis-aligned images to the outer integer rect, use hard for axis-aligned and soft for rotated. **No repo evidence exists for PDFium or MuPDF behaviour here; the scout's recollection is unverified.**

## 3. Order of work

### 3.1 Integration (half a day)
1. Merge dazzling, then gifted, into the user's branch (conflicts as above). `cargo check -p lege`; pdf-cli tests in the main tree (LF there).
2. Layout-detection A/B for jolly (decision 2). Merge jolly. Re-run the test suites.
3. Commit this plan into `dev-notes/lege-pdf/render-refinement/scoping-20261108/`.
4. AKR: propose a work record for rounds 1-2 with children per item below; evidence from the cloud log's gate table.

### 3.2 Round 1 remainder: correctness (all S-M)
Order: B6, B11, B14, N1, B15, B12, B8. Each gets a failing-then-passing test; corpus hashes identical except where noted.

| ID | Change | Anchors (HEAD 5b24f8e4) | Tests | Output change |
|---|---|---|---|---|
| **B6** | In `codec_target_size`, extents per image axis `(ceil(abs(a)+abs(b)), ceil(abs(c)+abs(d)))`. Must land **after jolly**: `decode_target_size`'s floor and the decode-level cache key derive from it, so they follow automatically. Correction to the review: the swap also *under*-decodes (a 1000x400 DCT at device 100x300 gets 1/2 scale and is upscaled), so rotated draws lose sharpness, not just time. | prepared.rs:5269-5318; pdf-image jpx.rs:270-276 | pure-function test on rotated matrices; render test: rotated non-square DCT equals the transpose of the upright render | rotated draws only |
| **B11** | Store the page crop's device AABB on `CpuPreparedPage` (6 constructors, like jolly's `decode_floor`); the `device_bounds_sane` limit becomes page-relative. | prepared.rs:3335-3339, 3797-3801, 5346-5357 | full-page rect and stroke; 8x8 window at (2400,3100), scale 4, equals the full render's crop | crops match the full render |
| **B14** | `paint_command` passes the device-plane width for groups and soft masks (crops reuse B11's extent). Tiling cells are lowered as their own page, so their clamp is inherent (the review over-scoped this). Scratch buffers become plane-wide for tiny groups, so A/B it; if it costs, split a `clamp_right` from the buffer width. | exec.rs:4464-4465; raster.rs:1125-1130 | group crossing the right edge: last-column AA equals an unclipped render | last column of affected rows |
| **N1 / A8** | Resolver: on a *repaired* `Parsed` whose declared `/Length` runs past the window, grow once to the declared end (capped at EOF) and retry; the ladder stays as the wrong-length fallback. | pdf-document resolve.rs:26-28, 237-325; pdf-object parser.rs:397-426 | 150 KB stream with `endstream` + `endobj` in its first 200 bytes (direct and indirect /Length); control with a genuinely wrong length | none except N1 pages (now correct) |
| **B15** | `lookup` instead of `intern` at the eight read-only sites; count each inline image as one op; intern only the closed set of spec names in `operand_to_object` and `decode_inline`. | interpret.rs:650-667, 4745-4803; 3308, 5033, 4832, 1691, 1714, 1478, 4734, 3857 (about +55 on dazzling after 1146) | 100k distinct missing names with bounded `names().len()`; 50k-element `/F` array; `max_ops+1` inline images | none |
| **B12** | `Sheets` arm: compose, write, drop per sheet via a lazy iterator inside `submit_composed`; delete partial files on failure. | lege-pdf/print/src/spool/file.rs:176-186 | live-raster count at most 1 via a drop guard; sheet k-1 on disk before sheet k composes | PNG bytes identical |
| **B8** | Byte budget on `ParseContext`, enforced in `begin_job()` (clear `cache` and `objstm` when over budget; a miss only re-parses or re-decrypts). Lazy decryption is L; skip. | lege-pdf/read/src/session.rs:21-46; pdf-document resolve.rs:57-79, 178-182 | encrypted document with N large streams: working set bounded across jobs | none |

### 3.3 G1 level 2: determinism harness (M)
- `pdf-cli/tests/determinism.rs` (new file, because dazzling and gifted both edit `pipeline.rs`): `SchedulerOptions{compile_workers, render_workers, reorder_window_depth}` over {1,2,4} workers, two page orders, two sizes; per-page hashes against cold fresh-backend hashes. Covers the shared glyph and image caches (B1, B3).
- `lege-pdf/read/tests/determinism.rs`: `open_with_cache_capacity(.., 0)`, real threads draining a shared cursor, fixture alternating Type 1, TrueType and a doubled-`/FontMatrix` Type 1. Covers the worker-local font cache (B2). `CpuBackend::render_to_host` builds a fresh context per page, so only the read seam reproduces B2.
- Fixtures from `corpus/mk_determinism_pdfs.py` (reportlab; run under WSL, or commit the generated PDFs).

### 3.4 Round 2 remainder: performance, ranked for lege's product path

| Rank | Item | Effort | Expected | Notes |
|---|---|---|---|---|
| 1 | **Tier B1 stencil (`/ImageMask`) fast path** | M | about 3x on MRC stencil pages (the generic sampler is about 90% of their time) | Two-entry LUT {paint: [rgb,255], mask: [rgb,0]} with /Decode polarity, routed through the existing bilevel box filter (`is_binary_box_filterable`, `binary_box_lut`, `paint_axis_aligned_binary_box`). `mix_bilevel` rounds identically to `weighted_average_rgba`. **Run only on the interior rectangle where `edge_coverage` is `None`** (separable for axis-aligned draws), generic body on the L-shaped border band, so hashes stay identical. Guard short sample buffers (the SAT builder indexes `samples[bitpos>>3]` directly; check the same hole for non-stencils). Stays generic: shading or pattern stencils, non-Normal blend, soft mask, rotation. Anchors exec.rs:768, 818, 847, about 930; image.rs:564-584, 1319-1346, 1476-1500, 1804. Subsumes the A4 hoist. |
| 2 | **Tier B2 SAT to row-band windowed sum** | M | about 2x on bilevel pages; 34 to 135 MB less per concurrent draw | New finding: a 600-dpi letter page (33.7M bits) exceeds the 32M-entry SAT cap and falls to the *serial* per-pixel loop, so the slow path runs exactly on the largest sources. Per destination row: `colw[x] = sum wy * bit`, prefix sum, `w_lo*colw[lo] + w_hi*colw[hi] + S*(P[hi-1]-P[lo])`, the same integers as `weighted_ones`. Rayon over row bands. Keep the SAT as the `#[cfg(test)]` oracle. Do after B1 (B1 routes stencils through it). Anchors exec.rs:1223-1450, 1090. |
| 3 | **mimalloc on Windows: A/B only** | S code, M validation | unknown on the NT heap (glibc: text 10-25% faster, compile 7-42% slower) | The repo has only a commented stub (lege-process/Cargo.toml:60, core/lib.rs:155). Build `readprobe` and `sessionbench` with and without `--features mimalloc`; measure wall, user CPU, `PeakWorkingSet64`, retained working set, single-thread pinned and 4+ workers. If adopted: `[target.'cfg(windows)'.dependencies] mimalloc`, `#[global_allocator]` in `core/main.rs` (not lib.rs, which would impose it on the GUI and FFI). The B2 ABA precondition is fixed on dazzling. |
| 4 | **A21 surface reuse slots** | S | lege alternates analysis, base and OCR sizes on one thread; the single slot misses every alternation | surface.rs:346-357; 2-3 slots plus a stats counter; measure on Windows (never measured there). |
| 5 | **A16 DeviceCMYK direct conversion** | S (+S-M for the /Decode LUT, M for the cache) | 3-6x on the conversion | Prove both round trips are identities exhaustively over 256 values; call `adobe_cmyk_to_srgb_u8` directly. Closes the AKR `cmyk-source-conversion-cache` precondition (analysis, base and OCR paint the same image). |
| 6 | **A18 16-bit in the promotion family** | S | large on bpc-16 pages (pdfTeX PNGs); rare in scans | add `Gray`, `Rgb`, `Cmyk` at bpc 16 to `generic_promotion_eligible`; border-ring caveat applies. |
| 7 | A9 remainder | S-M | per job; only large inputs | `Arc<[u8]>` to `Vec` needs a Vec-backed shared type; feasible for RGB8, a contract change for Gray8 `HostPage.pixels`, so skip Gray8. Read the file into one Arc; reuse the first `RenderSession`. |
| 8 | A7 image-XObject dedupe per compile | S-M | niche | interpret.rs:3297-3453 |
| 9 | A12 gray 9/7 fused final level; A20 O(1) eviction | S-M | minor | Skip the JPX pre-zero (needs `MaybeUninit`). The A20 encoding hoist is already in gifted (16decc2e). |

**Skip, with reasons:** A5 caching half (the key hashes the whole gray source per draw, est. 2-3 ms against 4-6 ms saved; 25 MB entries in a 96 MiB budget; jolly's decode reference already shares the decoded gray buffer across a page's renders; B4 native gray kernels is the right fix). A11c (calloc / `HEAP_ZERO_MEMORY` pages are free; `repeated_rgba_arc` shows it would not need new unsafe, but the gain is nil). A14 (lege keeps the default budget; no product gain). A4 hoist (M, not S; irrelevant once B1 lands).

### 3.5 Gate for every change
- Hash gate: `probe run --hash` at scales 1.0, 1.37, 2.0, RGBA and Gray8, fresh and shared backend, over the synthetic classes plus `D:\pdf-test\{beans,viaduct,vogtle,xenosystems}.pdf`. Differences must be on the change's intended list.
- Timing: `sessionbench` through `RenderSession`, 150 dpi, warm pass, `ab.py` with at least 9 paired order-swapped rounds; report the median ratio and wins, require identical checksums.
- One binary per commit, each from a detached worktree created with `-c core.autocrlf=false`.

## 4. Windows measurement recipe (this host)
- Corpus: `D:\pdf-test\` (beans 0.6 MB, viaduct 2.1 MB, vogtle 1.9 MB, xenosystems 3.2 MB; also Structures, centralparktest-repair, chinese-medicine, sahib).
- Harnesses live only on dazzling under `dev-notes/lege-pdf/render-refinement/scoping-20261108/harness/`; their path deps are six levels up, so build them inside a worktree at the same relative path. Copy the root `Cargo.lock` into each harness dir; `cargo build --profile debug-fast --manifest-path ... --target-dir <one per binary>`. `lege-pdf-read` pulls `pdf-text`, hence the autocrlf rule.
- Pinning: i7-13700H P-core logical CPUs are 0-11. `cmd /c "set RAYON_NUM_THREADS=1&& set PDF_RENDERER_IMAGE_ROW_PAR=0&& set PDF_RENDERER_JPX_CONCURRENCY=serial&& start "" /affinity FFF /b /wait python -I ab.py 9 A.exe B.exe -- file.pdf 150 gray8 3"` (`/affinity 555` avoids HT siblings). `ab.py` runs unchanged on Python 3.14 (miniconda; there is no `python3` alias; use full `.exe` paths).
- No callgrind on Windows: use more rounds and paired ratios; add `QueryThreadCycleTime` and `GetProcessTimes` to sessionbench as a tie-breaker. For instruction counts, install valgrind and cargo in WSL (Ubuntu-26.04 is present with `taskset` and `python3`, but lacks cargo, valgrind, fonts and reportlab). Profiling: `wpr` is installed; `cargo install samply` is the lighter option.
- `mkcorpus.py` is Linux-only (font paths, reportlab); generate the synthetic corpus under WSL once and keep it outside the repo, or rely on the real documents.

## 5. AKR records to add after integration
- Work: this plan's items as children; evidence: the cloud gate table (2,364 renders), local test counts, timing tables.
- Fix the claim `promotion-is-exact` in `lege-ecosystem-perf.renderer.gray8-draws-take-the-rgb8-fast-paths` (wrong for unmasked fractional placements; pinned by the ring test).
- Observation: B6 under-decodes rotated non-square images (sharpness, not just time).
- Observation: 600-dpi bilevel pages exceed the SAT cap and take the serial path.
- Papercuts: fresh worktrees plus autocrlf break pdf-text's digest check; harness path deps are six levels deep.
