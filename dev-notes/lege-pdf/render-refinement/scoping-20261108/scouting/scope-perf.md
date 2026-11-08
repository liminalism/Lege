# Scoping: remaining renderer perf items (baseline 5b24f8e4, read-only)

Paths are relative to D:\projects\lege-ecosystem. R = lege-pdf/render/crates. CPU = R/pdf-render-cpu/src.
"dazzling" = origin/claude/dazzling-planck-z44poy, "jolly" = origin/claude/jolly-ride-cyjbsm, "gifted" = origin/claude/gifted-pascal-eyn4qw.
Line numbers in the review match HEAD within a few lines unless noted. Nothing here was built or run; "est." marks reasoning from the code and the review's numbers.

## 0. Ranking (gain per effort for lege-process PDF->PDF: analysis + base + OCR render per page, Gray8 base on gray pages, MRC/JBIG2 stencil pages common)

| Rank | Item | Effort | Why here |
|---|---|---|---|
| 1 | B1 stencil fast path | M | ~90% of MRC page time is the generic stencil sampler (3.6x slower than Poppler); Lege's own output uses stencils; exactness is provable in integers. |
| 2 | B6 mimalloc Windows A/B (measure only) | S code, M validation | Cheap: readprobe already has a `mimalloc` feature. NT heap hands every >512 KB block back to the OS, so page surfaces, promotions, SATs and JPX planes re-fault; never measured on Windows. May dominate A21/A5-cache/A11c. |
| 3 | A21 surface reuse slots | S | lege renders 2-3 sizes per page on one thread; the single `surface_buffer` slot misses on every alternation. |
| 4 | A16 CMYK direct call (+ Decode LUT, cache later) | S / M | Direct call is exhaustively provable over 256 values; 3-6x on a conversion that runs once per draw (up to 3x per page). |
| 5 | B2 bilevel row-band | M | 2x on bilevel pages; and any bilevel source over 32M bits (600-dpi letter = 33.7M) skips the SAT and runs a serial per-pixel loop. |
| 6 | A18 16-bit promotion | S | A one-line eligibility change; rare in scanner input. Do it with A16. |
| 7 | A9 remainder | S-M | One-time per job (file copy, second xref parse) plus avoidable full-page copies. Real for 100+ MB inputs only. |
| 8 | A7 dedupe image XObjects | S-M | Niche (same image drawn many times per page). |
| 9 | A4 hoist | M | Subsumed by B1 on the product path. |
| 10 | A5 caching half | M | Not before jolly; probably not at all (B4 native gray kernels is the better fix). |
| 11 | A12 | gray fused M, rest S | Only the gray 9/7 fused level is worth ~5-7 ms per gray JPX layer. |
| 12 | A14 | S | No gain for lege itself (keeps the default budget). |
| 13 | A20 remainder | S | Text cold path; mostly scans in lege. First bullet is already done on gifted. |
| 14 | A11c | S | Confirmed not worth it. |

Sequencing: land jolly before anything touching ImageCacheKey / promote_through_cache (A5-cache, A16-cache, A18, A14). Do B1 then B2 in one worktree (both edit exec.rs ~768-1460 and `is_binary_box_filterable`/`binary_box_lut`).

---

## 1. A4 hoist half: precompute axis_box_taps per column and per row in the generic sampler

HEAD (matches review): `CPU/image.rs:1893` `axis_box_taps`; called per pixel per axis by `box_taps_x/_y` (1203-1211) <- `area_average` (1255-1260) <- `shade_uv` (1139) <- `shade` (1022). The per-pixel driver is the `paint_row` closure in the generic arm of `paint_image`, `CPU/exec.rs:~930-1000`. A per-column/per-row hoist already exists on the fast paths: `paint_axis_aligned_binary_box` (exec.rs:1105-1150) and the area-min paths (2953-2964); it is exact because with `inv.b == 0.0 && inv.c == 0.0` the per-column value equals `inv.apply` bit for bit.
Branch overlap: dazzling's image.rs diff is only 669-697 (A5/B5), not `axis_box_taps`; jolly does not touch image.rs.
Change: in the generic arm, when `inv.b == 0.0 && inv.c == 0.0` (and the quarter-turn case a == d == 0, where fx depends on dy), build `Vec<Option<AxisTaps>>` for x0..x1 and y0..y1 once (copy exec.rs:1131-1150) and add `shade_with_axis_taps(dx, dy, tx, ty)` / `area_average_taps` that skip `box_taps_x/_y`. The soft-mask side has the same per-pixel cost (`smask_box_taps`, image.rs:1228); hoist it too or the 8-bit SMask path keeps most of the cost. `shade_clamped` (border ring) stays per pixel. Review says S; I say M (taps thread through `shade_uv`, `mask_hides`, `sample_smask`, the profiling variant).
Gate: byte-identical corpus hashes; `fast_path_matches_generic_shade` (exec.rs:5093) and dazzling's `fractional_placements_match_the_generic_body_including_the_border_ring`; add hoisted-vs-per-pixel equality over random placements (generator pattern at exec.rs:5485-5556).
Gain: review: axis_box_taps 13.4% + libm rounding ~6.7% of MRC time, so at most ~20% of generic-sampler pages. After B1 the generic sampler is left with 8-bit SMask, alpha < 1, soft masks, non-Normal blend and the border band, so the product gain approaches zero. Skip unless B1 is deferred.

## 2. A5 caching half: cache Gray8 promotions through promote_through_cache

HEAD: the Gray8 promotion is a draw-time step, `exec.rs:700-724` calling `gray8_promoted_to_rgb8` (image.rs:673; dazzling already made it write into one Arc and exclude colour-key masks). The cache route (`prepared.rs:2574` call, `2600-2640` `promote_through_cache`, `2645-2670` `promotion_cache_key`) runs at prepare time and only for Indexed and the "generic" family (ICC/Lab/Tint; `generic_promotion_eligible`, image.rs ~705).

Interaction with jolly (prepared.rs diff read):
- jolly rewrites `ImageCacheKey` (`target: Option<(u32,u32)>` becomes `space: Option<(u8,u8)>` + `scale: DecodeScaleKey`) and keeps `promotion_cache_key` valid by setting `space: None, scale: DecodeScaleKey::Level(0)` (comment: a promotion's width/height and content hash pin the resolution). A Gray promotion is then mechanically one more caller of `promotion_cache_key` (it already hashes samples and tags `gray` via `mix_color_space_identity`) with the `PROMOTION_CACHE_CODEC` sentinel. Do it after jolly to avoid rebasing struct literals.
- The semantic interaction is larger. Today lege's analysis render decodes a scan at a reduced level and the base at another, so the gray buffers differ and a content-keyed promotion could not hit across them. jolly's per-page decode reference (`page_output_plan.rs`) makes analysis decode at the base level, so analysis, base and OCR (when not larger) share one decoded Gray buffer and one promotion entry. Without jolly, only repeated draws of one image at one scale benefit.
- Behaviour trap: prepare-time promotion is unconditional, while exec.rs:703-714 promotes only for Normal blend, no clip mask, no soft mask, alpha 255, axis-aligned, and (minified or unmasked). A prepare-time version must rebuild that predicate from what lowering knows (`clip_has_mask`, `inv`, alpha, blend, footprint, smask/mask) and keep the exec fallback, or 3n buffers appear for draws that never used them (and the rotated/clip-masked ring behaviour must not move).

Costs the review does not count: the key hashes the whole Gray source (`content_hash_128`, two dependent multiply chains; est. 2-3 ms for 8.4 Mpx) on every draw, hit or miss, against a saved 3n write (25 MB for a 300-dpi letter page; est. 4-6 ms warm, more with first-touch faults on Windows). Each 25 MB entry also competes inside the 96 MiB budget (prepared.rs ~715 sizes it for ~3 full pages) and evicts the decodes jolly just made shareable once two pages are in flight.
Verdict: marginal. The real fix for gray scans is B4 (native 1-channel kernels: read 8 MB instead of write 25 MB then read 25 MB). I would not schedule the cache half; if tried, measure after jolly on a gray JPEG book with the Windows allocator (B6) first.
Gate if done: hash-identical (promotion is byte-identical by construction since `pixel()` is the definition); a determinism test rendering one page at two sizes in both orders through one cache.
Effort M. Gain est. 5-15 ms per 300-dpi gray page with jolly, about 0 without.

## 3. A7 dedupe image XObjects within one compile

HEAD: `pdf-content/src/interpret.rs:3329` `op_do`, `3360` `draw_image_xobject`. Every `Do` re-runs `decode_stream_data_to_codec` (`pdf-document/src/resolve.rs:523`; the review's "decode.rs:176" is the pdf-structure decoder behind it), builds a `SemImage` and pushes it (`images.push` ~3449). Confirmed per-draw copies: flate samples `Some(Arc::from(bytes))` (~3386, Vec to Arc copy) and the codec payload returned as a fresh Vec. `codec_data` is `SharedBytes` (Arc<Vec>), so lowering does not copy it again (lower.rs:603).
Branch overlap: dazzling's interpret.rs hunks are at 1160 (FE1), 5093-5179, 5714; none in 3290-3460. jolly: none.
Change: `HashMap<ObjectId, ImageId>` in `Interpreter`; if `obj_id` is `Some` and already compiled, `emit(DrawImage(prev_id))`. Placement: after the OC checks in `op_do` (already before the call). Pitfalls the review does not mention:
- `/ColorSpace` can be a name resolved in the current resource scope (`resolve_image_colorspace`, ~3376), so one ObjectId in two forms could resolve differently: key on (ObjectId, scope id) or dedupe only when `/ColorSpace` is not a bare name.
- Decode-budget charging drops from N to 1 (FE1 already accepted this kind of change); check `adversarial_tests.rs` for tests that rely on repeated draws exhausting the budget.
Gate: byte-identical corpus; new test with one image drawn 12 times (12 DrawImage ops sharing one ImageId, render identical) and one with two forms using different `/ColorSpace` resource names.
Effort S-M. Gain: review measured 8.5 ms per compile for one image drawn 12 times (5.6 MB inflated each). Pages that reuse images only; zero for scans.

## 4. A9 remainder: hand-over, one-Arc file read, reuse the first RenderSession

HEAD (matches; the RGB8 packing half is dazzling cfdf5f1 and not repeated here):
- `lege-process/pipeline/config.rs:97-118`: `ProcessingPipeline::new` does `Arc::from(pdf_bytes.into_boxed_slice())` (a full copy; transient 2x file), opens a `RenderSession` only to read `page_count`, and drops it. `from_file` is `std::fs::read`. `PdfPageSource::new` (`pipeline/source.rs:84-89`) -> `PdfRenderer::new_from_bytes` (`core/pagerender.rs:44`) opens the session a second time (second xref parse). `count_pdf_pages_from_bytes` (pagerender.rs:107, called from `core/main.rs:241`) is a third open with a third copy, CLI only.
- `.to_vec()` copies of render output: `core/pagerender.rs:158`, `pipeline/pdf_tokio_pipeline.rs:3297` (analysis RGB), `3497` (`compact_gray_surface`, full-size base Gray8), `3682` (slow-OCR RGB). `RgbImage::from_raw` needs a Vec.
Change, three independent pieces:
(a) Keep the session: store `Arc<RenderSession>` in `ProcessingPipeline`, add `PdfPageSource::from_session` / `PdfRenderer::from_session`, and pass it through `create_and_run_{pdf_tokio,djvu,epub}_pipeline` (they take `pdf_bytes: Arc<[u8]>`: pdf_tokio_pipeline.rs:4661, djvu_pipeline.rs:709, epub_pipeline.rs:115; test at pdf_tokio_pipeline.rs:5040). Saves one parse per job. S-M (signature churn).
(b) One-Arc read: have `new` take `Arc<[u8]>` and build it once in `from_file`. Zero-copy needs `Arc::<[u8]>::new_zeroed_slice(len)` + `read_exact` with one `unsafe`; the safe route keeps one copy but removes the second. S. Saves a transient 1x file size and a memcpy per job.
(c) Hand-over: `Arc<[u8]>` cannot become a Vec without a copy even when unique, so the fix is a seam type change: make `RgbSurface.pixels` / `GraySurface.pixels` a Vec-backed shared type (like `pdf_page_ir::SharedBytes(Arc<Vec<u8>>)`) with `into_vec()` via `Arc::try_unwrap`. RGB8 is local to `rgb_surface_from_rgba` (session.rs ~677; dazzling edited it, so do this after merging dazzling). Gray8 is the renderer's `HostPage.pixels: Arc<[u8]>` (surface.rs:256-289), so the change would propagate into pdf-render-api (a contract change): recommend RGB8 only, skip Gray8. M for RGB8.
Gate: exact by construction; existing pipeline tests; compare peak working set on a large input.
Gain: a few ms per page (8-25 MB memcpy each) and seconds plus a memory peak of one file size on huge inputs. Low value per page.

## 5. A11c: skip zero-fill of a seeded offscreen fully covered by the backdrop

Code: `exec.rs:388-393` (seed decision) -> `Surface::offscreen_seeded` (`CPU/surface.rs:147-173`) -> `Surface::offscreen` (111-130) -> `filled_arc(len, 0)` -> `Arc::<[u8]>::new_zeroed_slice`. Dazzling's A11 hunks are at exec.rs 4606-4740 (the composite-back side), not here.
Verdict: confirm not worth it, with a correction. On Linux `new_zeroed_slice` is calloc, so large groups get fresh zero pages and the zero is free beyond the copy's own first touch. On Windows Rust's `System::alloc_zeroed` is `HeapAlloc(HEAP_ZERO_MEMORY)`; whether the NT heap elides the memset for a fresh large block could not be verified, so the Windows cost is unmeasured (worst case a memset at ~10 GB/s: ~3 ms for a 33 MB page-sized group; only page-sized non-isolated groups, which scans do not have). The cloud note says the skip "needs new unsafe code"; true but unremarkable: `repeated_rgba_arc` (surface.rs ~358) already uses `Arc::new_uninit_slice` with a SAFETY comment, so this is ~30 lines of the same shape (allocate uninit, copy overlap rows, zero only margins). If mimalloc (B6) is adopted the zero-skip comes free for fresh pages.
Effort S. Gain 0-3 ms on rare pages. Skip.

## 6. A12 decoder micro-work

- Gray 9/7 fused final level (`lege-codecs/jp2lam/src/decode/reconstruct.rs`). Gap confirmed: Gray single-component packed output goes `reconstruct_grayscale_u8_into` (~586-650) -> `reconstruct_irreversible_97_centered` (~2005), which materialises the full f32 plane, then `write_gray_plane_f32` (~678) walks it again via `source_index`/`pixel_offset` per pixel. The fused path (`reconstruct_irreversible_97_fused_pack`, 1937-2085) exists only for three planes (`pack_row_f32` takes three rows). Change: a one-plane `pack_row_gray_f32` and an `Irreversible97` branch in `reconstruct_grayscale_u8_into` using `inverse_97_2d_even_but_last` + `final_level(kernel, first)` banded like the RGB version. dazzling edited this file (B7 worker-budget threading, ~120 lines across 556-1525 and 1937-2080), so build on dazzling. M. Gate: Gray8 JPX hash identical across serial/auto/budgeted and 1/4 Rayon threads (dazzling's B7 gate), plus a gray twin of the colour fused test (~2302-2409). Gain: est. 5-7 ms per gray layer at 300 dpi (two plane-sized passes removed).
- Drop JPX output pre-zero (`R/pdf-image/src/jpx.rs:~565-575`, `zeroed_arc`): jp2lam writes through `PackedWriteTarget` over `&mut [u8]` (initialised memory), so dropping the zero needs a `MaybeUninit` target or a full-coverage proof. Linux: calloc, free. Windows: unverified memset (est. 1-3 ms per 25 MB). Not worth the unsafe surface; B6 covers it.
- Vectorise Tier-1 dequantisation (`jp2lam/src/decode/t1.rs:404-416`): at HEAD a per-element `dequantize_i32_to_f32(q, step)` in a per-row loop. Whether LLVM already vectorises it at SSE2 was not checked (inspect asm first). If not, `chunks_exact(8)` with AVX2 dispatch (the crate already has AVX2 kernels); exact (one multiply). Review: 17 of ~170 ms at full resolution, best case ~8 ms (5%). jolly touches only `decode/mod.rs`.
- JBIG2 RENORMD batching (`lege-codecs/jbig2enc-rust/src/decode/arith.rs:93-104`, matches): the loop shifts one bit per iteration with a `ct == 0` byte-in check. Batch with `n = min(a.leading_zeros() - 16, ct)`. Exact. The review warns neighbouring MQ micro-changes measured slower: A/B it. Product impact small (decode <= 3% of MRC page time).
Effort: gray fused M, the rest S each. Total product gain small except gray fused on gray JPX.

## 7. A14 image-cache budget per consumer

Code: `CPU/prepared.rs:~725-732` (`DEFAULT_TOTAL_BYTES = 96 MiB`; `SharedImageCache::new(total_bytes)` already takes a budget), built with `Default` at `CPU/lib.rs:1130`. `CpuBackendOptions` (lib.rs:1046) has no field for it; it is built by `Default` in `lege-pdf/read/src/session.rs:399` and in the agent (`lege-pdf/agent/src/commands/{render,poppler,text}.rs`), by struct literal only in `pdf-render-cpu/tests/render_hinting.rs:90`. jolly changes `ImageCacheKey` but not the budget plumbing, and adds 87 lines to session.rs (expect a small conflict at `open_with_cache_capacity`).
Change: `image_cache_bytes: Option<usize>` on `CpuBackendOptions` (None = 96 MiB, Some(0) = off; `Default` unchanged), passed at lib.rs:1130 and into the `image_cache_enabled()` gate; in lege-pdf-read add `RenderSession::open_with_options(bytes, password, SessionOptions { cached_pages, image_cache_bytes })`, keeping `open` and `open_with_cache_capacity` as wrappers.
Gate: after jolly (today cache residency changes output, review B1, so one-pass consumers would change output). Test: budget 0 equals `PDF_RENDERER_IMAGECACHE=off`.
Effort S. Gain: -50 to -90 MiB peak on one-pass consumers. Lege keeps the default, so lege gains nothing; with jolly's per-level keys the 96 MiB default needs re-sizing (review).

## 8. A16 DeviceCMYK

Code: `CPU/image.rs:818-850` `cmyk_source_as_rgb8` per pixel does `u8 -> f32/255 -> clamp -> pdf_color::cmyk_to_rgb -> to_u8`. `cmyk_to_rgb` = `adobe_cmyk_to_srgb` (`pdf-color/src/cmyk.rs:1773`) = quantise `(v*255 + 0.49999997) as u8`, then `adobe_cmyk_to_srgb_u8` (1697), then `/255`. Only caller: `paint_axis_aligned_cmyk_area_min_opaque` (exec.rs:2457), reached only from the minified axis-aligned opaque `fast_cmyk_area` (exec.rs ~838). Magnified/rotated/masked CMYK uses `pixel()` per tap; CMYK with `/Decode` is excluded entirely (`decode.is_none()`).
"Both round trips are identities": plausible and cheap to prove: for every x in 0..=255, `((x as f32/255.0).clamp(0.0,1.0)*255.0 + 0.49999997) as u8 == x`, and `((b as f32/255.0)*255.0 + 0.5) as u8 == b`. Add as a test; if it holds, call `adobe_cmyk_to_srgb_u8(c, m, y, k)` directly (exact).
Further steps:
- `/Decode` byte LUTs: four 256-entry tables built with the same float expression `pixel()` uses (`comp()` -> clamp -> quantiser), fed to the direct call; extends the fast path to inverted-Decode CMYK. Test over all 256 x decode pairs.
- Per-draw reuse: add `ImageColorSpace::Cmyk` to `generic_promotion_eligible` (today only IccCmyk). Hazard: prepare-time promotion moves magnified/rotated CMYK draws from the generic body (A10 fractional edge coverage) onto RGB8 fast paths (ring by pixel centre, dazzling's "open decision"). Restrict to the current `fast_cmyk_area` predicate (minified, axis-aligned, opaque). Same ImageCacheKey edit as section 2, so after jolly.
Gate: hash-identical, with a CMYK JPEG scan and a CMYK Flate image added to the corpus first (the review notes the corpus has no CMYK); the exhaustive 256-value identity test.
Effort: direct call S; Decode LUT S-M; cache M. Gain: 12 ns/px to est. 3-5 ns/px with the direct call (it removes two f32 conversions and a clamp, not the grid interpolation); 132 ms per cover in the review becomes ~40-50 ms; the cache removes the repeat across analysis/base/OCR. Relevant to CMYK photo covers and some scans.

## 9. A18 16-bit Gray/RGB/CMYK in the promotion family

Code: `generic_promotion_eligible` (image.rs ~705-737) already accepts `bpc 8|16` but only for IccRgb, IccCmyk, Lab, TintLut(2) (the `matches!` at ~722-728). Gray/Rgb/Cmyk at bpc 16 take `pixel()` per tap (f32 `comp()` path, image.rs ~1527-1580) on every draw. `generic_source_as_rgb8` (~761) already resolves any eligible image through `pixel()`.
Change: add `ImageColorSpace::Gray | Rgb | Cmyk` to the `matches!` when `bpc == 16` (8-bit stays on direct paths). `promote_through_cache` is the only user, so the result is cached by content with no other code. Same ring caveat as A16: restrict by draw shape or accept and pin the ring change. `sample_lut` and ColorKey masks already exclude.
Gate: hash-identical by construction; add 16-bit gray and RGB corpus entries (none exist) and a test that a promoted 16-bit image equals its generic render at integer and fractional placements.
Effort S. Gain: from 30-250 ns/px per draw to one cached conversion; large on pdfTeX 16-bit PNG pages, irrelevant for scanner output. A 2.1 Mpx 16-bit gray hashes 4 MB per draw (est. 1 ms).

## 10. A20 text cold path

- Type 1 encoding search: already done on gifted, not at HEAD. HEAD `pdf-font/src/type1.rs:~469-472` still calls `find(region, b"readonly def")` inside the per-`dup` loop. Commit 16decc2e on gifted ("parse the Type 1 encoding in one pass; always retain parsed Type 1") hoists it before the loop (its message: 4.2 s to parse a 20k-entry crafted font before; Symbol 605 -> 152 us) and, because the faster parse fell under the 400 us `MIN_PARSE_TO_CACHE` gate, makes native Type 1 always retained (`should_retain_font_program` = `is_type1() || ...`, prepared.rs). So merging gifted satisfies this bullet; nothing separate to do.
- Hash each font program once per Arc: `FontProgramKey::for_resource` (prepared.rs:82) runs `content_hash_128(&resource.program)` in `resolve_program_bytes` (~4071-4090), once per font per page lower (memoised per page by `resolve_font_program`'s local map, 4061). Est. 0.2-0.4 ms per MB of program per page. Only large CJK/system fonts on short pages; scans have no fonts. Change: memoise the key on the program `Arc` (a `OnceLock` beside it in `FontResource`, or a `Weak`-validated side map as in the B2 fix; raw-pointer keys risk the same address-reuse ABA). S, exact.
- O(1) glyph-cache eviction: `SharedGlyphCache::insert` (dazzling prepared.rs ~490-510; 452-464 at HEAD) evicts with `shard.map.iter().min_by_key(last_used)`, a full scan of the shard per evicted entry. It runs only once the 32 MiB budget (4 MiB per shard) is full, i.e. long text documents; with ~300 B per entry a shard holds ~14k entries. Fix: CLOCK / linked LRU over slot indices, or batch eviction (scan once, drop the oldest eighth). Exact output (B3 made rasterisation deterministic). The cache also under-charges (96 B vs 170-290 B real). S. Same lines dazzling's A2 edits, so build on dazzling.
Gain: small and confined to long text documents. Skip for the scan-heavy product.

## 11. A21 surface reuse slots

Code: single slot `ctx.surface_buffer: Option<Arc<[u8]>>` (`CPU/lib.rs:1893` and `2090`: `ctx.surface_buffer.take()`; parked via `into_output_recycling`, surface.rs ~240-295). `reuse` (surface.rs:346-357) accepts a buffer only if `len` matches exactly and `Arc::get_mut` succeeds (consumer dropped it). dazzling touched surface.rs only for A3/RGB8 (268-291); jolly not at all.
Mechanism confirmed: lege renders analysis (small), base (full) and OCR (other size) on the same worker thread (thread-local context), so after each render the slot holds the wrong size, the next render drops it and allocates fresh. A fresh large buffer on Windows is a VirtualAlloc'd block (NT heap sends >~512 KB to VirtualAlloc and releases on free) faulted and zeroed by the kernel before the first write.
Change: replace the slot with up to 3 entries keyed by length (`take_for(len)` picks the sole-owned exact match; `park` replaces the oldest) plus a hits/misses counter in `RenderStats`. Memory: the base-size buffer is already retained today (33 MB at 300 dpi letter); the small analysis buffer adds ~3 MB; the OCR-size buffer is the real addition, so cap total parked bytes.
Gate: exact (`a_recycled_surface_is_byte_identical_to_a_fresh_one`, surface.rs:405; extend with a two-size alternation test) and the `render_determinism` tests. Measure on Windows with a sessionbench variant that renders two sizes per page (the existing one renders one).
Effort S. Gain: review says ~0.1 ms per miss on Linux; Windows unmeasured: est. 4-8 ms per base render (8k pages x 0.5-1 us) = 1-3% of a 300-dpi page. It overlaps B6 (a retaining allocator removes the same faults for every large buffer), so measure B6 first; if mimalloc is adopted this item mostly vanishes.

## 12. Tier B1: /ImageMask stencil fast path

HEAD: stencils are excluded from every fast path: `exec.rs:768, 818, 847` (`!img.is_stencil` in `fast_rgb8_base`, `rgb8_area_shape`, `fast_cmyk_area`), `is_binary_box_filterable` (image.rs:564-573), `lut_nearest_opaque` (image.rs:584), and `binary_box_lut` needs `sample_lut`, which `build_sample_lut` never builds for stencils (image.rs:1804). A stencil therefore takes the generic per-pixel arm (exec.rs ~930) and the last loop of `area_average` (image.rs:1319-1346), which calls `pixel()` per source tap; `pixel()` re-derives `components()`, `row_bits()`, `read_bits` and the `/Decode` polarity every call (image.rs:1476-1500). This matches the review's profile (`pixel` 43%, `shade_uv` 19%, `axis_box_taps` 13%).
No branch overlap: dazzling's exec.rs hunks are at 3855, 4460-4740, 6231+; jolly's is one attribution line.

Design (exact, because the generic result is reproducible in integers):
1. Polarity: bit 0 paints, 1 masks, unless `/Decode [1 0]` (image.rs:1485-1499). Add `stencil_box_lut() -> (zero, one)` with rgb = `stencil_rgb` in both and alpha 255 for the painting bit, 0 for the masking bit (swapped for inverted Decode). Then `mix_bilevel` returns exactly what `weighted_average_rgba` does: both round `(acc + weight/2)/weight` per channel (verified at image.rs:1960-1966 and 1933-1947), and rgb stays constant since `(rgb*w + w/2)/w == rgb`, matching the generic code's "keep paint colour, take averaged alpha" (image.rs:1336-1346).
2. Minified (footprint > 1), axis-aligned, Normal blend, no soft mask, solid colour, 1 bpc: route to `paint_axis_aligned_binary_box` (already handles `cmask`) by letting `is_binary_box_filterable` and `binary_box_lut` accept stencils; `BilevelIntegral::build` / `weighted_ones` are LUT-agnostic. Guard sample length (`samples.len() >= row_bits/8*height`): `pixel()` treats missing bytes as 0 (paint), but `BilevelIntegral::build` indexes `samples[bitpos >> 3]` directly (exec.rs ~1295-1318) and would panic on short data. Truncated JBIG2/CCITT output is plausible, so fall back to generic (or pad); the existing non-stencil gate may have the same latent hole, so check it.
3. Magnified / 1:1 (footprint <= 1), Nearest: a transparent-skip twin of `paint_axis_aligned_lut_nearest_opaque` (read bit, skip when masked, else source-over `stencil_rgb` at alpha*cov). Analysis renders are always minified, so this is secondary.
4. Border-band caveat (the central risk): the generic arm multiplies coverage by `img.edge_coverage(dx, dy)` for pixels whose square is not fully inside the image quad; `fast_binary` does not (nor do the other opaque fast paths; this is dazzling's "open decision"). A stencil spanning a page edge at fractional placement would change a ring of up to 1 px. To stay hash-identical: run the fast path only on the interior rectangle where `edge_coverage` is `None`, and let the L-shaped border band use the existing generic per-pixel body. For axis-aligned inverse, `edge_coverage` is `None` iff `p.x in [mu, 1-mu]` and `p.y in [mv, 1-mv]` (image.rs ~1061), which is separable: the interior x-range and y-range are two 1-D scans. The alternative (accept the ring change and pin it with `fractional_placements_match...`) changes every minified stencil draw's border by up to 1 px.
Stays generic: pattern- or shading-painted stencils (`stencil.shade`, exec.rs:3907), non-Normal blend, soft mask present, rotated/sheared placement.
Gate: byte-identical hashes on the MRC corpus classes (`mrc_jpx_jbig2`, MRC JPEG+G4) at 72/144/150 dpi and scales 1.0/1.37/2.0, RGBA and Gray8; extend dazzling's `fractional_placements_match_the_generic_body_including_the_border_ring` to stencils; unit tests for `/Decode [1 0]`, a clip mask, a short sample buffer, and a rotated stencil (must stay generic). A stencil MRC input: build with `harness/mkjbig2` (depends only on jbig2enc-rust, builds on Windows) plus a JPEG background, or use a Lege-produced output.
Effort M. Gain: the generic sampler is ~90% of MRC time (226 and 233 ms per page at 144 dpi vs 67 and 122 ms for Poppler); the SAT path ran the G4 bilevel page at 63 ms, so best case ~3x on the stencil part (est. 226 -> ~80 ms) before B2. Biggest single item.

## 13. Tier B2: bilevel summed-area table -> row-band windowed-popcount filter

HEAD: `BilevelIntegral` (exec.rs:1223-1450) is a per-draw `Vec<u32>` over the referenced source sub-rectangle (parallel `fill_parallel` above `BILEVEL_SAT_PAR_MIN_ENTRIES`); `MAX_ENTRIES = 32M` u32 (128 MiB). Above that `build` returns `None` and `paint_axis_aligned_binary_box` (1090) falls to its serial loop calling `binary_box_average` per pixel (image.rs ~985), with no row parallelism. Matches the review's 33.7 MB at 300 dpi. New finding: a 600-dpi letter page is 5100x6600 = 33.7M bits, over the 32M cap, so the SAT is skipped and the slow serial path runs exactly when the source is largest.
Exactness anchor: all integer. `AxisTaps` weights are `AXIS_TAP_SCALE` for interior taps and `w_lo`/`w_hi` at the ends (`weight_at`, image.rs:1874), so `ones_w = Sum_y wy(y) * Sum_x wx(x) * bit(x,y)`.
Design: split destination rows into independent bands (Rayon par over bands; only a band's edge source rows are recomputed). For one destination row, `colw[x] = Sum_y wy(y) * bit(x,y)` over the needed columns (byte-table unpack), then an inclusive prefix `P`; each destination pixel is `w_lo*colw[lo] + w_hi*colw[hi] + S*(P[hi-1]-P[lo])` (just `w*colw[lo]` when lo == hi): the same integers as `weighted_ones`, so output is byte-identical. Working set: one row of u32/u64 per worker (~40 KB at 300 dpi) instead of the SAT; total work ~ source bits x (1 + edge overlap), the same order as the SAT build but with no 33 MB write, no first-touch faults and no ceiling, and the 600-dpi case becomes parallel.
Overlap: B1 routes stencils through this function, so do B2 after B1 (or together). Keep the SAT under `#[cfg(test)]` as the oracle (existing tests ~exec.rs 5600-5780).
Gate: byte-identical hashes on the CCITT G4 / JBIG2 bilevel classes at 72/144 dpi plus a 600-dpi page (hits the old fallback); randomised equality row-band vs SAT vs `binary_box_average` over random placements.
Effort M. Gain: review ~2x on bilevel pages, -34 to -135 MB per concurrent draw (-267 MiB at 2+6 workers). For lege: external bilevel scans (CCITT/JBIG2 1-bit images); stencil pages are B1's.

## 14. Tier B6: mimalloc on Windows

Repo state: only a disabled stub. `lege-process/Cargo.toml:60` `# mimalloc = "0.1"  # Comment out mimalloc to avoid cross-compilation issues`; `lege-process/core/lib.rs:155-156` commented `#[global_allocator] static GLOBAL: mimalloc::MiMalloc`. No mimalloc in Cargo.lock; no allocator set in `lege` or `lege-gui` (std System = `HeapAlloc` on the process heap). `pdfium-diff` has only an optional dhat allocator; `jbig2enc-rust/tests/decode_alloc.rs` a counting allocator. The cloud `readprobe` harness has an optional `mimalloc` feature. Dev-notes (`optimization-round4-followups-20261104.md:29`) mention a scratch mimalloc build that "halved the median compile"; the review reproduced that only for MRC/bilevel compile under concurrency.
Prerequisite: the font-program cache ABA (review B2, which mimalloc made fire 10/10) is fixed on dazzling (57f6704), so merge dazzling first.
What a Windows NT-heap vs mimalloc A/B needs:
1. Renderer level, no product change: build `readprobe`/`sessionbench` twice from one commit, with and without `--features mimalloc` (sessionbench needs the same 6-line optional dep). `libmimalloc-sys` needs a C compiler; the `cc` crate finds MSVC via vswhere even though `cl` is not on PATH, and the MSVC Rust toolchain implies Build Tools, but verify with a build. Try env variants (`MIMALLOC_PURGE_DELAY`, `MIMALLOC_ARENA_EAGER_COMMIT`) to trade retention against speed.
2. Single-thread pinned and multi-thread (default scheduler, 4+ workers, unpinned) on text, MRC, bilevel, JPEG and gray-JPEG classes; report wall, user CPU, `PeakWorkingSet64` and post-run `WorkingSet64` (review: peak RSS 1.5-3x single-threaded, retained up to 5.5x glibc's).
3. Product level: a `mimalloc` feature on `lege` with `[target.'cfg(windows)'.dependencies] mimalloc = { version = "0.1", optional = true }` (this removes the cross-compilation objection in the Cargo.toml comment), `#[global_allocator]` in `lege-process/core/main.rs` rather than lib.rs (lib.rs would impose it on the GUI and any FFI consumer), then end-to-end `lege` runs on two real PDFs with layout detection on the GPU, comparing wall time and peak working set. wgpu/driver allocations sit outside this heap, so the process-level delta is what the user feels.
Effort: S code, M validation. Expected: unknown on the NT heap (review's numbers are glibc: text 10-25% faster, bilevel 12-18%, JPEG 7-9% mostly page-fault avoidance, compile 7-42% slower on stream-heavy pages). It may be better on Windows because every >512 KB buffer is a fresh VirtualAlloc. One afternoon of measurement; it competes with A21, A11c and the A5 cache.

---

# Part 2. Windows measurement setup

## Findings on this machine
- Python: `python` is miniconda 3.14.9 (`C:\Users\dkoep\miniconda3\python.exe`); `py.exe` exists. There is no `python3` on Windows: use `python -I` where the cloud notes say `python3 -I`.
- CPU: i7-13700H, 14 cores / 20 logical: P cores with HyperThreading are logical processors 0-11, E cores 12-19. So `/affinity FFF` = LPs 0-11 = all P threads, the repo's established mask (`lege-misc/dev-misc/jpxbench/check.sh:17,28`, `jpxbench/README.md:46`, dev-notes `optimization-jp2lam-serial-20261104.md:162`, and your memory note).
- Installed: `wpr.exe` (`C:\Windows\system32`). Not installed: valgrind, VTune, xperf, WPA, perf, hyperfine, pwsh (only Windows PowerShell 5.1); `cl` is not on PATH. Python has no `reportlab` (the corpus generator needs it, plus Liberation TTFs and X11 Type 1 fonts at Linux paths).
- WSL: Ubuntu-26.04 with `taskset` and `python3`, but no valgrind, cargo or fonts (installing needs root).
- No Windows bench script does more than the `start /affinity` idiom (grep 'affinity': jpxbench/check.sh, its README, four dev-notes). `lege-pdf/render/tools/pdfium-diff/run-sweep.ps1` is a correctness sweep without pinning.
- `lege-pdf/render/corpus/perf` holds only `manifest.json`, `pages.tsv`, `results/`; sources are external (`PDF_RENDERER_CORPUS_ROOT`). The cloud corpus and the three real documents (beans, viaduct, vogtle) are not in the repo.

## Building the harnesses on Windows
The harnesses exist only on dazzling (`dev-notes/lege-pdf/render-refinement/scoping-20261108/harness/`), not at HEAD. Their path dependencies are relative to that exact depth (`../../../../../../lege-pdf/read` = six levels up to the repo root), so they must sit at that path inside a checkout.
1. One detached worktree per binary, line-ending conversion off, because `lege-pdf-read` depends on `pdf-text` whose build.rs checks a digest that CRLF breaks (your memory note): `git -c core.autocrlf=false worktree add --detach D:\scratch\wt-A <commit>` (the `-c` applies to that command only; repo config untouched). The main checkout currently has LF in `fx_ucddata.inc`, so building there is fine.
2. Copy the harness into each worktree at the same relative path: `git archive origin/claude/dazzling-planck-z44poy dev-notes/lege-pdf/render-refinement/scoping-20261108/harness | tar -x -C D:\scratch\wt-A` (untracked files in a scratch worktree).
3. Copy the root `Cargo.lock` into each harness crate dir (each has its own `[workspace]`). `lege-pdf/read` is a member of the root workspace, so its `workspace = true` fields resolve; `jp2lam` and `jbig2enc-rust` are root workspace path deps, so the root `[patch]` entries are not needed.
4. `cargo build --profile debug-fast --manifest-path <harness>\sessionbench\Cargo.toml --target-dir D:\scratch\sb-A` (debug-fast; CLAUDE.md says fat LTO takes up to 15 min on Windows). `probe` also builds; its one Unix call (`/proc/self/status`, probe/src/main.rs:116) is `.ok()?`, so RSS lines are just absent; take `PeakWorkingSet64` from PowerShell instead.
5. One binary per commit into separate `--target-dir`s; never rebuild in place between rounds.
6. Corpus: generate under WSL (needs `fonts-liberation`, `python3-reportlab`, PIL, numpy, and a Type 1 font package; `mkcorpus.py` hard-codes `/usr/share/fonts/...`, so it will not run on Windows as is), or use real documents.

## Pinning, environment and running ab.py
- `ab.py` is stdlib-only and runs unchanged under Python 3.14 on Windows if you pass full `.exe` paths. It does no pinning: affinity and env vars are inherited by its children, so pin the whole script.
- cmd form (same idiom as the repo scripts; from Git Bash use `cmd //c`):
  `cmd /c "set RAYON_NUM_THREADS=1&& set PDF_RENDERER_IMAGE_ROW_PAR=0&& set PDF_RENDERER_JPX_CONCURRENCY=serial&& start "" /affinity FFF /b /wait python -I ab.py 9 D:\scratch\sb-A\debug-fast\lege-session-bench.exe D:\scratch\sb-B\debug-fast\lege-session-bench.exe -- doc.pdf 150 gray8"`
  To also avoid HT-sibling contention use `/affinity 555` (one LP per P core).
- PowerShell alternative: set `$env:` vars, `$p = Start-Process python -ArgumentList '-I','ab.py',... -PassThru -NoNewWindow; $p.ProcessorAffinity = 0xFFF`. There is a small race before the first child spawns; `start /affinity` has none.
- Hygiene: on AC, High performance plan (`powercfg /getactivescheme` could not be queried from this shell), nothing heavy running, discard the first launch per binary (Defender scans a fresh exe), at least 9 interleaved rounds (ab.py swaps order each round), confirm `output identical` (checksums). The cloud noise floor was +-10% from code layout alone, so an A/B under ~5% means nothing. `--pass 0` for cold, default (last pass) for warm.
- Pin for anything single-threaded; do not pin for multi-worker measurements (B6, scheduler); use `UserProcessorTime` and wall.
- Hash gate for every exact item: sessionbench checksum over the whole corpus, baseline vs candidate; finer grain with `probe run --hash` as the cloud did (2,364 renders per binary over 23 inputs).

## Replacing callgrind instruction counts
No drop-in exists here (no valgrind or VTune; PMC access through ETW is not set up on this hybrid CPU; `wpr` can record sampling traces but reading them needs WPA or xperf, neither installed). In order of preference:
1. More rounds and paired ratios (the repo's method), P-core pinned, min and median of warm passes.
2. Add thread cycle counts to `sessionbench` (`QueryThreadCycleTime` / `QueryProcessCycleTime`, kernel32; a two-line `extern "system"` declaration, no dependency) plus `GetProcessTimes` user time. Cycles are immune to frequency scaling and C-states but not to code layout: a tie-breaker, not callgrind.
3. Instruction counts from WSL: install `valgrind` and a Rust toolchain in WSL, build the harness there; callgrind totals are layout-independent and valid on WSL2, so they can confirm that small exact changes (A16 direct call, A18, RENORMD) cut instructions even when wall time is below the noise floor. Only before/after on the same OS is meaningful.
4. For where-does-time-go profiling: `wpr -start CPU -filemode` / `wpr -stop trace.etl` works as installed but needs WPA to read; the practical route is installing WPA (Store) or `cargo install samply` / `flamegraph` (ETW-based, may need an elevated shell). I did not install anything.
