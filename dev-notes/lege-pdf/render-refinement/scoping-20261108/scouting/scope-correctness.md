# Scope: remaining correctness and test-infrastructure items (baseline 5b24f8e4)

Paths are relative to `lege-pdf/render/crates/` unless they start with `lege-pdf/`. HEAD = working tree at 5b24f8e4.
Branches: DZ = origin/claude/dazzling-planck-z44poy, JR = origin/claude/jolly-ride-cyjbsm, GP = origin/claude/gifted-pascal-eyn4qw.

Branch overlap (git diff --name-only 5b24f8e4 <branch>):
- DZ touches: lege-pdf/read/src/session.rs (RGB8 packing only, not WorkerContexts), pdf-cli tests/pipeline.rs and lege-render-demo.rs, pdf-content interpret.rs (FE1 hunk ~1146 adds ~55 lines, so every B15 site after 1146 shifts by about +55; B10 ~5093-5180), exec.rs (A10 @4460, A11, tiling @3855, ring test @6284+), prepared.rs (glyph cache only), raster.rs (B13 dirty flag @119-228, 480-530).
- JR touches: prepared.rs (cache key, `decode_floor`, codec-target code ~5300-5420), exec.rs (+1 line), lege-pdf/print/src/compose.rs, read/src/{export,lib,session}.rs, pdf-image/*.
- GP touches: prepared.rs (48 lines, font), pdf-cli/tests/pipeline.rs (+43).
- None touches: pdf-document/src/resolve.rs, pdf-object/src/parser.rs, print/src/spool/file.rs, `device_bounds_sane`, or the `let width = ...` line in `paint_command`.
- Merge friction only for: B6 (JR, same neighbourhood), B11 (JR adds a field to every `CpuPreparedPage` constructor, B11 must too), B15 (line shifts), G1 (DZ and GP both edit pipeline.rs; put new tests in new files).

---------------------------------------------------------------------------------------------

## B6: quarter-turn placements pass a swapped box as the codec target

Review claim: CONFIRMED at HEAD; unchanged on DZ and JR.

Code:
- `pdf-render-cpu/src/prepared.rs:5269-5318` `codec_target_size(ctm)`: AABB of the 4 mapped unit-square corners, `w = ceil(x1-x0)`, `h = ceil(y1-y0)`, times headroom. That is a DEVICE-axis box but consumers read it as IMAGE-axis (width, height).
- Consumers: `prepared.rs:2331` (one target shared by base image, smask, mask), `pdf-image/src/jpx.rs:270-276` (`DecodeResolution::AtLeast{width: tw, height: th}`), `pdf-image/src/jpeg/mod.rs:58-99, 1022-1044` (`pick_dct_size`: needs `scaled(width) >= tw && scaled(height) >= th`). DCT honours the target too (so AKR `target-only-for-jpx` is false, as the review says).
- Matrix (pdf-geom/src/lib.rs:65) is `x' = a x + c y + e`, `y' = b x + d y + f`. Image width axis maps to device vector (a,b); height axis to (c,d). Image-axis extents are `(|a|+|b|, |c|+|d|)`. Today's box is `(|a|+|c|, |b|+|d|)`. Equal only if b = c = 0. Quarter turn (a = d = 0): today `(|c|,|b|)`, correct `(|b|,|c|)`. Review is right.
- Correction to the review's impact: the swap can also UNDER-decode, not only over-decode. Example: DCT 1000x400 drawn with device extents u=100, v=300. Correct need (100,300) -> level 0 (400/2=200 < 300). Swapped request (300,100) -> 1/2 scale accepted (500>=300, 200>=100), v axis then upscaled 200->300. So some rotated draws lose sharpness, not just time. Direction depends on aspect ratio.
- JR interaction: JR replaces the call site by `decode_target_size(ctm, out.decode_floor)` (JR prepared.rs ~5374-5412), which calls `codec_target_size(ctm)` and `codec_target_size(ctm.then(floor))` and takes the per-axis max. If B6 lives INSIDE `codec_target_size`, both calls get it and the floor composes correctly. JR's cache key is `DecodeScaleKey::Level(codec.reduction_level(data, &descriptor, &params, limits))`, falling back to `Target(target_size)` for codecs returning None (JR ~3044-3060). The level is computed from the same `params.target_size`, so after B6 the key follows automatically with no key code change, and an upright and a quarter-turn draw of the same image at the same scale now share one cache entry. B6 should land on top of JR (or in the same PR) so the `Target(..)` fallback also gets correct axes. JR's test `decode_target_size(draw, skewed) == Some((32,16)) "per axis"` (JR ~7192) must be re-derived after B6 if that matrix has b or c non-zero.

Change (S, ~15 lines): in `codec_target_size`, `w = ceil(|a|+|b|)`, `h = ceil(|c|+|d|)`; keep the invertible/finite/>=1 guards and the headroom multiply; fix the doc comment. Note: the L1 sum overestimates a rotation by up to sqrt(2) at 45 degrees (hypot is the true footprint). L1 never under-decodes and equals today for b=c=0; use hypot only if a measured 45-degree case shows over-decode matters.

Tests:
1. Unit, prepared.rs `mod tests`: `codec_target_size` for `scale(100,50)` -> (100,50); quarter turn `{a:0,b:50,c:-100,d:0}` -> (50,100) (HEAD gives (100,50)); 270 turn; flipped `scale(100,-50)` -> (100,50); with translate; on JR also `decode_target_size(ctm, Some(rot90 floor))`.
2. Integration, pdf-render-cpu/tests/render_image.rs: non-square high-frequency DCT (64x16, `pdf_image::jpeg::encoder::write_jpeg`, as JR's `jpeg_payload_sized`), drawn upright at 32x8 and rotated 90 to 8x32. Assert rotated render == transpose of upright render (exact for nearest; +-1 for bilinear). Fails at HEAD (different decode level). Optionally assert the `SharedImageCache` holds one entry (JR API).
3. JPX: assert `reduction_level` equality in a pdf-image test (jp2lam is not a pdf-render-cpu dev-dep).

Effort: S. Output: only non-axis-aligned image placements (/Rotate 90/270 pages, quarter-turn `cm`), non-square JPX/DCT; bytes change in both directions (faster and/or sharper). Gate with the hash corpus; expect fewer decoded pixels on rotated MRC pages (review: 160-380 vs 40-143 ms) and possibly a higher decode level on some DCT pages.

---------------------------------------------------------------------------------------------

## B8: unbounded per-thread ParseContext for encrypted PDFs

Review claim: CONFIRMED, with precision: it is "unbounded per thread per session", and the retained data is the decrypted RAW (still compressed) stream body, not decoded data.

Code:
- `lege-pdf/read/src/session.rs:21-46`: thread_local `WORKER_CONTEXTS { session_id, parse: ParseContext, raster: CpuWorkerContext }`; `select_document` (:38-45) replaces both only when a different session first runs on that thread. A closed session's caches linger until that thread serves another document.
- `pdf-document/src/lib.rs:253-282` `ParseContext`: `cache: HashMap<ObjectId, Arc<PdfObject>>` (no bound; `cache_put` :350) and `objstm: HashMap<u32, Arc<ObjStmCached>>` (decompressed object-stream containers, unbounded, any document; `objstm_put` :358). `begin_job()` keeps both on purpose (test `begin_job_resets_job_state_but_keeps_worker_caches`, lib.rs ~1085).
- `pdf-document/src/resolve.rs:100-108, 140-143, 178-182`: for encrypted files each offset object runs `decrypt_uncompressed` -> `decrypt_value`; the `Stream` arm reads the raw body, decrypts, stores `StreamData::Owned(body.into())`. Unencrypted streams stay `InSource{offset,len}`. So stream-body growth is encrypted-only; the objstm container cache grows for everyone but is modest.
- Size: every stream body this thread resolved (images dominate).

Options:
1. Byte-bounded cache in `ParseContext` (recommended, M-). Track `cache_bytes` in `cache_put` (weight = Owned stream length + constant) and `objstm_put` (container data length). Enforce the budget at a JOB BOUNDARY in `begin_job()` (not inside `resolve`): if over (say 64-128 MiB per worker) clear the maps. A miss only re-parses or re-decrypts, so output cannot change; clearing between jobs avoids mid-page thrash and never invalidates an `Arc` a caller holds. ~25 lines plus a constructor knob (`ParseContext::with_cache_budget`).
2. Lazy decryption from source ranges (L). Keep `InSource` plus an "encrypted for object id" tag and decrypt at `stream_bytes` time. Removes the retained copy but touches every stream consumer (filters, length repair, the zero-copy DCT/JPX payload sharing). Not worth it unless peak decryption copies matter.
3. Session-scoped contexts (S-M but incomplete). A thread-local cannot be freed from another thread; only fixes the FFI/print "closed document lingers" half, not live-document growth (the lege case). Can be combined with 1 via an epoch bump on `RenderSession::drop` that each thread checks at next use.
Recommendation: option 1.

Tests:
- pdf-document unit (lib.rs mod tests): fill a `ParseContext` with Owned-stream objects past the budget, `begin_job()`, assert maps empty and `cache_bytes == 0`; under-budget keeps entries (extends the existing begin_job test).
- Integration lege-pdf/read/tests (new file): `pdf-test-support` has `builder::encrypted_fixture()` (builder.rs:535) and `aes256_encrypted_fixture()` (:627); build ~50 streams of 1 MiB (or lower the budget via the new constructor), compile+render all pages on one thread through `RenderSession`, assert a `#[doc(hidden)] ParseContext::cached_bytes()` stays <= budget + one job, and rendered bytes equal an unlimited-budget run.
Effort: M. Output: none.

---------------------------------------------------------------------------------------------

## B11: small crops drop page-spanning paths

Review claim: CONFIRMED (low severity). Precise mechanism: limit = 64 x max(output w, output h) as an ABSOLUTE coordinate bound in the request's device plane.

Code:
- `pdf-render-cpu/src/prepared.rs:5346-5357` `device_bounds_sane(points, size)`: `limit = 64 * max(size.width, size.height, 1)`; any `|x|` or `|y| > limit` returns None.
- Callers drop the whole primitive: fills `prepared.rs:3335-3339`, strokes `3797-3801`.
- `size` is the request `output_size`. The CPU backend ignores `RenderRequest.crop` (every test passes `crop: None`); the read seam makes a crop by translating `transform.matrix` and shrinking `output_size`. For a 19 px crop the limit is 1216 px; a full-page background at 300 dpi spans out to ~2500-3300 so it is dropped.
- Nested lowering (`lower_cell` etc.) calls `lower_impl` with the cell's own size; the same class of drop there is existing behaviour and arguably intended.

Change (S-M, mostly constructor churn):
- In `lower_impl` (`prepared.rs:1572`; has `page`, `base`, `size`) compute `ref_extent = max(w,h)` of `base.transform_rect(page.bounds.crop)` (pdf-geom `Matrix::transform_rect`), `max` with `max(size.w,size.h)`. Store on `CpuPreparedPage` as `sanity_extent` (or the final limit).
- `device_bounds_sane(points, size, limit)`; both callers pass `SANE_FACTOR * out.sanity_extent`. A 1e9 coordinate still trips it; full-page renders are unchanged (limit equal when crop = page).
- `CpuPreparedPage` constructors to update: prepared.rs:1452, :1589, :6454 (test helper), attribution.rs:360, exec.rs:6274 (test helper), plus DZ's ring-test `dispatched` (DZ exec.rs ~6400). JR adds `decode_floor` to the same constructors, so land after JR or expect a trivial merge. Consider a `CpuPreparedPage::empty(size)` helper for tests to stop the churn.
- The same stored extent is the natural fix for the crop half of B14.

Tests:
- Unit: extend `device_bounds_sane_rejects_corrupt_geometry` (~6198) with an explicit limit: points at -2400 accepted at limit 64*3168, rejected at 64*10; 1e9 rejected at both.
- Integration pdf-render-cpu/tests/render_fills.rs: 612x792 page with a black full-page rect and a thick stroke across it. Reference: whole page at scale 4. Window: 8x8 at (2400,3100) via `matrix = scale(4) then translate(-2400,-3100)`, `output_size 8x8`. Assert every pixel equals the reference sub-rectangle (HEAD: window white). Also assert a 1e9-coordinate path is still dropped in the window.
Effort: S-M. Output: only crop/zoom windows with page-spanning primitives (lege image-region crops < ~19 px, zoomed print/preview).

---------------------------------------------------------------------------------------------

## B12: composed print-to-file jobs hold every sheet in memory

Review claim: CONFIRMED.

Code: `lege-pdf/print/src/spool/file.rs:176-186`: `Sheets` arm builds `rasters = Vec::with_capacity(sheets.len())`, pushes every `compose_sheet(...)?`, then `submit_rasters(..., &rasters)` (:81-95) writes PNGs. Peak = all sheets. `spool/mod.rs:83-84` says composition is the spooler's call so a backend can stream; the review notes CUPS and Windows do one sheet at a time. No branch touches this file (JR edits print/src/compose.rs only).

Change (S):
- Factor `submit_rasters` into an inner `submit_composed(printer, title, options, compose, impl Iterator<Item = Result<SheetRaster, PrintError>>)`: `create_dir_all`, then per item: take raster, `next_sheet_name()`, `write_sheet_png`, push path, drop raster. `submit_rasters(&[SheetRaster])` keeps its public signature and calls it; `submit` passes a lazy `sheets.iter().map(|s| compose_sheet(session, s, &compose))`.
- Failure semantics change: HEAD leaves nothing on disk if composition fails; streaming leaves sheets 1..k-1. Recommend best-effort removal of this job's files and not advancing the counter (the job is not recorded either).

Tests (lege-pdf/print/tests/spool.rs next to the `submit_rasters` tests at :125-195):
- Streaming bound: feed the inner function a lazy iterator whose item k asserts on entry that sheet k-1's PNG already exists, and track live `SheetRaster`s with a drop guard (max alive == 1). HEAD semantics would fail (all composed before the first write).
- Equivalence: same pages via `submit_rasters(&all)` and via the streamed path give byte-identical PNGs and equal `RecordedJob`.
- Error path: item 3 fails -> Err, no leftover `sheet-*.png`, `last_job()` None, next job numbers from `sheet-0001`.
Effort: S. Output: none (PNGs byte-identical).

---------------------------------------------------------------------------------------------

## B14: offscreen content anti-aliased differently at its right edge

Review claim: PARTLY CONFIRMED. Right for transparency groups and soft masks; the scope list ("tiling fills", "cropped region renders, either edge") does not all follow from the proposed fix.

Code:
- `pdf-render-cpu/src/exec.rs:4464-4465`: `let width = surface.origin_x + surface.width;` in `paint_command`, passed to `fill_runs_cancellable(&page.points, subs, width, height, ...)` (:4475). For a group offscreen this is the offscreen's right edge in device coordinates (`Surface::origin_x = bounds.x.max(0)`, surface.rs:~123), not the device plane.
- `raster.rs`: `width` is both buffer size and x clamp. `deposit_edge_row` clamps `xa`/`xb` to `[0,width]` (:1125-1126), `deposit_partial` (:1208-1209), AEL variants use `fwidth` (:286-288, 343-344, 516, 591). Clamping a sloped segment at an artificial boundary replaces the real slope in the last column with a vertical drop, which changes accumulated area there. Plausible source of the review's 114/255 (shallow) and ~32/255 (45 degrees) figures; I did not re-run its emulation.
- Fix direction is right: `page.size.width` is the device plane and `paint_command` already has `page`. Only x needs it (rows are clipped exactly per scanline); left clamp at 0 is the plane's left edge, correct.
- NOT covered by this fix:
  * Tiling fill mask (`exec.rs:3824-3826`) already uses `page.size.width/height`. Tiling CELLS are lowered as their own small page (`lower_cell`, size = cell), so a clamp at the cell edge is inherent to cell-as-page.
  * Cropped renders: the plane IS the crop, so the clamp sits at the crop edge; fixing it needs a reference plane extent (same stored extent as B11) used as the clamp while buffers stay surface-sized. That is the M part.
- Side effect to measure: a plane-wide `width` makes `cov`/`acc_row` scratch (`raster.rs:226, 513, 588`) plane-wide for tiny groups. Row loops use `col_min..col_max` but `first = width` scans and `cov.resize` happen per fill; A/B a group-heavy page on a 2480 px plane. If it shows, add a separate `clamp_right` parameter to `fill_runs_cancellable`.

Change: S for groups/soft masks (`let width = page.size.width as usize;`, after confirming `origin_x + width <= page.size.width`, else take the max). M if clamp/buffer decoupling or the crop reference extent is wanted.

Tests (pdf-render-cpu/tests/render_group.rs, render_softmask.rs):
- Page 64x64; isolated, opacity 1, Normal group whose BBox clips at x = 40.0, containing a long thin shallow triangle crossing x = 40. Reference: same triangle drawn directly under an integer-aligned rect clip at x = 40. Assert byte equality (HEAD: last column of affected rows differs). Add a 45-degree variant and a soft-mask variant (mask content crossing mask bounds). Control: content fully inside stays identical.
Effort: S (groups/soft masks) to M. Output: only the last column of rows where a group's or soft mask's clipped content crosses the offscreen's right edge; no whole-page effects. Few corpus pages expected.

---------------------------------------------------------------------------------------------

## B15: name interner grows with every content-stream name

Review claim: MOSTLY CONFIRMED. All eight sites match HEAD. Memory only, hostile PDFs only, never affects output.

Code (HEAD pdf-content/src/interpret.rs; add ~+55 for sites after 1146 on DZ):
- Interner: pdf-object/src/interner.rs:46-70 (append-only map + table); `lookup` :71 does not insert; `Interpreter::intern` :737.
- Read-only sites: :3308 (`Do`), :5033 (`sh`), :4832 (`gs`), :1691 (`BDC` property; also interns constant `b"Properties"` per call), :1714 (same), :1478 (`named_space_is_pattern`), :4734 (inline `/CS` named space: `PdfObject::Name(self.intern(other))`), :3857 (named space inside a closure using `?`). Each can use `self.names.lookup(bytes)`: a never-interned name cannot be a key of a parsed resource dictionary, so the existing miss path (return Ok / None / Gray fallback) is taken, same observable behaviour. For `Properties` use `names.known` if present, else intern once at construction.
- Inline images: `Lexeme::InlineImage` arm (:650-667) does not increment `ops_executed` (the `max_ops` check is at :621-623, operators only; `max_ops = 5_000_000`, pdf-content/src/lib.rs:60). `operand_to_object` (:~4757-4787) interns every Name operand and dict key; `decode_inline` (:~4803-4820) interns each filter name. Only `MAX_OPERAND_STACK = 65536` (:32) bounds one dict. (The review's "real bound is the 1 GiB decode budget" I did not verify.)

Change (S):
1. The 8 lookup sites.
2. Count each inline image as one op in the `InlineImage` arm (with the `max_ops` check).
3. In `operand_to_object`/`decode_inline` keep `intern` only for the closed set of spec names (colour-space and filter names plus abbreviations, inline-dict keys); map unknown names to `lookup` or one sentinel `NameId`, so an adversarial array costs one entry. Behaviour for unknown filters/spaces is unchanged (they already fail resolution).

Tests (pdf-content/tests/adversarial_tests.rs, which already builds hostile PDFs with `PdfBuilder` and `PageCompiler`):
- 100,000 distinct missing names via `Do`, `gs`, `sh`, `BDC` property, `cs`: compile, assert `snap.names().len()` (NameTable `len`, pdf-object/src/lib.rs:220) grows by less than a small constant (HEAD: ~100k) and ops painted are unchanged.
- Inline image with `/CS [/I /RGB 255 <..>]` and a 50,000-element distinct-name `/F` array: name growth bounded; valid forms still render (existing inline-image fixtures).
- `max_ops + 1` tiny `BI..EI` images yields `ContentError::OperatorBudget`.
Effort: S. Output: none for valid PDFs; a hostile stream with > `max_ops` inline images now errors instead of compiling.

---------------------------------------------------------------------------------------------

## N1 / A8: 64 KiB stream window ladder truncates a correct-length stream

Review claim: CONFIRMED by code reading (no runtime repro done). No branch touches either file.

Code:
- `pdf-document/src/resolve.rs:26-28` `INITIAL_WINDOW = 64 KiB`; `parse_uncompressed` (:225-275) reads `abs..min(abs+window_len, source_len)`, calls `attempt_parse` (:277+), grows (x8) ONLY on `Attempt::Grow`, which comes only from `UnexpectedEof` or a missing header (:306-316).
- `pdf-object/src/parser.rs:397-426` `read_stream_body`: the happy path needs `data_start + declared_len <= input.len()` (:397-401). When the declared stream extends past the window that fails and the repair path runs (`find_subslice(tail, b"endstream").or_else(.. b"endobj")`, :~410-440). If the leading bytes contain `endstream` or `endobj` (embedded PDF/attachment, raw ASCII object dump) the body is truncated there, `StreamLengthRepaired{declared: Some(len), actual}` is emitted and `Attempt::Parsed` is returned, so the resolver never grows. With no needle in the window, `UnexpectedEof` -> Grow -> happy path. So only streams longer than the remaining window, with the marker inside the first window (or inside later windows 512 KiB, ...), are hit.
- The parser cannot tell "window" from "whole file" (it also parses whole in-memory files, where a truncated stream must be repaired), so the decision belongs in the resolver.

Change (S-M):
- In `parse_uncompressed`, after `Attempt::Parsed(parsed, _)`: if `!at_eof` and `parsed.repairs` has `StreamLengthRepaired{declared: Some(d), ..}` with `d >= 0` and the declared end lies beyond the window, set `window_len = min(d + window_len, source_len - abs)` and retry ONCE (a `grown_for_declared` guard) before accepting the repair. After the retry the happy path applies if `endstream` follows the declared end; otherwise the repair is genuine and kept (a wrong /Length > 64 KiB is still repaired, at the cost of one larger read).
- Cap by `source_len - abs`: a hostile `/Length` 2^40 costs no more than the existing x8 ladder reaching EOF. Indirect `/Length` works the same (resolved before the repair decision).
- Also check the object-stream/xref-stream readers for the same window-then-repair shape (not verified).

Tests (pdf-document resolve.rs mod tests or tests/; fixtures via `PdfBuilder::add_stream_with_length`, builder.rs:87):
1. 150,000-byte raw stream whose first 200 bytes contain `endstream\nendobj\n`, correct `/Length`: assert full 150,000 bytes and NO `StreamLengthRepaired` event (HEAD: ~200 bytes plus an event).
2. Same with indirect `/Length 5 0 R`, and with the object at a non-zero offset.
3. Control: genuinely wrong `/Length 200000` (actual 50 bytes) still yields the 50-byte body plus an event, and on a 10 MB file does not read more than ~(declared + 64 KiB) capped at EOF (counting `PdfSource` wrapper).
4. Control: < 64 KiB stream containing `endstream` text, correct length (already correct; pins behaviour).
Effort: S-M. Output: only documents with such streams (rare); they become correct. No corpus hash expected to move.

---------------------------------------------------------------------------------------------

## G1 level 2: worker-count x render-order determinism harness, plus read-seam test

Level 1 (DZ pdf-render-cpu/tests/render_determinism.rs, 237 lines): single thread, one `CpuBackend` and one `CpuWorkerContext`; (a) a page written into a freed page's `Arc` allocation to force address reuse (B2), (b) glyph sizes 100.00/100.04 in both orders (B3), each compared with a "cold" render. No threads, no scheduler, no images.

Existing pdf-cli tests: `pdf-cli/tests/pipeline.rs` (530 lines) renders ONE page through `CpuBackend::default().render_to_host` (fresh `CpuWorkerContext`/`ParseContext` each call); it never uses the scheduler. pdf-render-scheduler is a normal pdf-cli dependency; only `lege-render-demo.rs` (`required-features = ["demo"]`) uses it.

Scheduler API (pdf-render-scheduler/src/lib.rs): `RenderScheduler::new(Arc<dyn RenderBackend>, SchedulerOptions)`; `render_range(&snapshot, Range<u32>, &make_request, cancel, &mut emit)`; `SchedulerOptions{compile_workers, render_workers, compiled_queue_depth, reorder_window_depth, memory_limit_bytes}` is a public struct with `Default`; `reorder_window_for(c, r)` gives the matching window. `make_request(&DocumentSnapshot, PageIndex)` is arbitrary code, so it can map the position index to any (page, scale), which makes order and size caller-chosen. Output is delivered in position order.

Limit (review section 7.1, confirmed): `CpuBackend::submit`/`render_to_host` (pdf-render-cpu/src/lib.rs:1280-1286, 2276-2288) create a fresh `CpuWorkerContext` per page, so scheduler-driven tests exercise the SHARED backend caches (glyph cache B3, image cache B1) but NOT the worker-local font cache or surface reuse (B2). B2-class leaks exist only where a context is reused: the read seam's thread-local `WORKER_CONTEXTS` (lege-pdf/read/src/session.rs:27-35).

Proposal, two files:

A. `pdf-cli/tests/determinism.rs` (backend + scheduler; B1, B3 class). New file, to avoid the DZ/GP merge conflict in pipeline.rs.
- Fixture: one multi-page PDF from `PdfBuilder`: non-embedded Helvetica text at 12.00, 12.03, 24 pt and Times; a DCT page (64x16, high-frequency, `pdf_image::jpeg::encoder::write_jpeg`; dev-dep present); a JPX page (jp2lam dev-dep present); a /Rotate 90 variant; one Gray8 request. Work list of `(page, dim, format)` includes the same page at two sizes (0.25 then 2.0) for B1.
- Ground truth: per item, a fresh backend + `render_to_host`, FNV-1a over `host.pixels` (no new dependency).
- Matrix: `(compile_workers, render_workers)` in `[(1,1), (2,4)]` (add `(4,8)` if time allows) x orders `[forward, reverse, rotated by k]`. Each cell builds ONE `CpuBackend` shared across the whole list (as lege does per session) and runs `render_range` with `make_request` mapping position -> item through the permutation; collect `(item, hash)` and compare with ground truth; repeat each cell 3-5 times (races are probabilistic: review saw 8-10/30 for B1, 12/12 for B3 with >=2 workers). Select workers with `SchedulerOptions { compile_workers: c, render_workers: r, reorder_window_depth: SchedulerOptions::reorder_window_for(c, r), ..Default::default() }`; the scheduler spawns OS threads regardless of core count, so this is meaningful on a 1-2 core runner.
- Failure message names item, workers, order, repeat.
- Runtime: <=200x200 pages, ~12 items x 6 cells x 4 repeats, seconds.
- Expected: fails on HEAD for B1 and B3; green with JR + DZ.

B. `lege-pdf/read/tests/determinism.rs` (read seam; B2 class and capacity-0 regression).
- `RenderSession::open_with_cache_capacity(bytes, None, 0)` (session.rs:383): capacity 0 frees each compiled page after use so the next `CompiledPage` tends to reuse its address, the exact B2 trigger (review: 8/8 at capacity 0, 0/66 at default 8). Fixture: pages alternating a Type 1 program, a TrueType program, and a doubled-`/FontMatrix` Type 1 (pdf-test-support `fonts::minimal_type1/minimal_ttf`; embed as in pipeline.rs `embedded_type1_fontfile_renders_through_the_native_engine`, :493+, or GP's version; DZ's `larger_type1` for the variant).
- Workers: `std::thread::scope` with N in {1,4} real threads, each draining a shared atomic cursor over an order list (forward, reverse); each thread has its own thread-local context reused across pages (the leak condition). Cold truth: each page on a fresh `RenderSession` rendered on a fresh spawned thread.
- Assert per-page hash equality for every (N, order, capacity in {0,8}).
Effort: A = M, B = S-M. Level-1's `assert_same` can be copied (integration test crates cannot share code) or promoted to pdf-test-support. Output: none (tests only).

---------------------------------------------------------------------------------------------

## Open decision: the opaque image border ring

Code (HEAD; the test is DZ-only: pdf-render-cpu/src/exec.rs ~6356-6520, `fractional_placements_match_the_generic_body_including_the_border_ring`: 5 placements x 4 colour spaces x 4 mask kinds x 2 interpolations = 160 cases; masked cases byte-equal to `generic_masked_reference`, unmasked only where `edge_coverage` is None).
- Generic body: exec.rs ~945-975 (also :2664 masked border band, :3908 stencil, :5241 test reference): `img.edge_coverage(dx, dy)` (image.rs:1062; exact polygon clip of the pixel square against the unit quad, so it handles rotated quads): `Some(0)` skip, `Some(c)` paint `shade_clamped` at weight c/255, None = interior full weight.
- Fast paths that skip it (opaque, unmasked, axis-aligned): `paint_axis_aligned_rgb8_nearest_opaque` (:1594), `paint_axis_aligned_lut_nearest_opaque` (:1660), `paint_axis_aligned_rgb8_bilinear_opaque` (:1966), `paint_axis_aligned_rgb8_area_min_opaque` (:2431), `paint_axis_aligned_cmyk_area_min_opaque` (:2449); Gray8 and Indexed reach them through `gray8_promoted_to_rgb8`/expansion in `paint_image` (:629). They paint the 1 px ring by pixel centre, full weight or nothing.
- The cure already exists in `paint_axis_aligned_rgb8_area_min_masked` (:2534): with `inv.b == inv.c == 0`, `edge_coverage` separates per axis, so the interior is a rectangle of whole columns/rows; prepared body inside, generic per-pixel body in the border band; byte-identical. Recorded in AKR `lege-ecosystem-perf.renderer.mrc-masked-draw-fast-path` claim `edge-aa-separates` and `performance-history/optimization-mrc-mask-20260808.md` ~60-75.
- Measured on record (same note): the first masked version, which skipped the ring, moved a page hash: 188 of 1,231,580 px, all on the outer border, max delta 32/255.
- Separate small bug found by DZ's test: the generic BILINEAR sampler can return alpha 254 for an opaque image (tap weights sum 254/255); the magnified bilinear fast path keeps 255. The test grants +-1.

What the repo records about PDFium/MuPDF: NO measurement of how either treats an opaque image edge at a fractional device position. The only statements: exec.rs:~948 ("Matches PDFium's softer Type 3 / stencil edges (workstream H)"); PLAN-POST-SWEEP3.md:218-223 and DEFERRED.md:746-751 (A10 was added because Type 3 bitmap glyphs and 1-bit stencils rendered heavier than PDFium). The evidence base is stencils and bitmap glyphs, not opaque colour images. The AKR claim `promotion-is-exact` (perf decisions.akr ~59) holds only "on the fast path or on the generic border band that the masked painter keeps"; it is false for the unmasked fast paths, which have no band. From general knowledge, UNVERIFIED here: PDFium snaps an unrotated image to the outer integer rect of the unit square and stretches into it with no edge AA (AA only on the rotated transformer path); MuPDF's rectilinear path also rounds the destination rect. If so, neither Lege behaviour equals either reference for axis-aligned opaque images and "soft" is not demonstrably closer to the oracle. Measure with the repo's pdfium-diff before deciding.

Option A: soften the fast paths' ring (match the generic body)
- Touches: the five `paint_axis_aligned_*_opaque` painters and their dispatch in `paint_image`: interior rectangle by the per-axis test (`half_u = 0.5*|inv.a|`, `half_v = 0.5*|inv.d|`, as at :2546-2560), border band to the generic per-pixel body via a shared helper. Promotion paths need nothing extra. Also fix generic bilinear alpha 254.
- Output: every opaque unmasked axis-aligned image with a fractional edge changes on its ring pixels (soft instead of hard; ~32/255 max where measured, 1 px wide, only where the image edge is inside the output). Full-page scans aligned to the page edge are unaffected. Pages with inset figures, photos, margins, thumbnails change; hash gate needs re-baselining for those, magnitude small.
- Speed: ring pixels go from fast path to generic (~250 ns/px at the 2026-08 measurement). Ring = perimeter px; 1697x2400 draw ~8,200 px = ~2 ms on pages whose image render is 23-70 ms. The same technique on the masked painter measured -91%/-70% with hashes unchanged. Expect ~1 ms to low ms; confirm with an interleaved pinned A/B.
- Effort: M. Benefit: path independence (same edge with or without a clip/soft mask), restores `promotion-is-exact`, lets the equivalence test compare whole surfaces and unblocks further generic-to-fast moves (A16, A18).

Option B: harden the generic body's ring (hard edges for opaque images)
- Touches: `edge_coverage` consumers at exec.rs ~951, :2664, :3908, :5241 and image.rs:1045-1062; gate A10 off for opaque colour images (keep for stencils and bitmap glyphs where PDFium evidence exists). Masked paths then lose their ring AA for opaque images and the test reference changes.
- Output: all opaque images with a fractional edge change, including rotated/sheared placements, whose AA (landed 2026-07-21 "including rotated placements") would be lost (jagged rotated photo edges), unless limited to axis-aligned, which adds a second behaviour split. Reverses a deliberate quality workstream.
- Speed: neutral or slightly faster. Effort S-M code, larger test/AKR revision.

Recommendation: Option A, conditioned on one cheap measurement. It is the shipped, proven pattern; it keeps A10's rotated-edge AA; it costs only perimeter pixels; and it makes the equivalence test total. Before implementing, render one probe PDF (opaque RGB image at x=10.3, y=10.6, size 20.4x15.7, plus a rotated copy) through PDFium (pdfium-diff) and compare the ring against soft and hard variants. If PDFium shows a hard outer-rect edge for the axis-aligned case, the oracle-closer choice is a hard (outer-rect) ring for axis-aligned opaque images and soft only for rotated; then fast paths need only "paint the ring at full weight over the outer rect" and the generic body must change to match. Either way: decide with the oracle number, record it in AKR, supersede `promotion-is-exact`, land as its own round with its own hash re-baseline, after B6/JR so key and target changes do not mix with ring changes.

---------------------------------------------------------------------------------------------

## Summary table

| Item | Effort | Output change | Depends on / conflicts |
|---|---|---|---|
| B6 | S | non-axis-aligned image draws (both directions) | land on JR; fix inside `codec_target_size` |
| B8 | M | none | independent |
| B11 | S-M | crop/zoom windows only | JR constructor churn; shares `sanity_extent` with B14-crop |
| B12 | S | none | independent |
| B14 | S (groups/soft masks) / M (crops) | boundary pixels | perf A/B for wide scratch |
| B15 | S | none (valid PDFs) | line shifts vs DZ interpret.rs |
| N1 | S-M | rare streams become correct | independent |
| G1 L2 | M | none (tests) | green needs DZ+JR+GP; new files avoid pipeline.rs conflicts |
| Ring | M | ring pixels of opaque fractional images | after B6/JR; needs oracle probe |
