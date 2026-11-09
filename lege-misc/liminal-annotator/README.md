# Liminal Annotator

A native Windows rectangle editor for reviewing book-page layout boxes, written in Rust on
[LiminalKit](https://github.com/liminalism/liminalkit). It is the review tool for the layout-model dataset
that trains Lege's layout detector (`google-colab-work/layout-detection`; the spec is `annotator.md` there).
It is its own Cargo workspace, outside the Lege workspace, so builds at the repository root are unaffected.

You open a page, inspect the model's existing boxes, correct or add regions with the mouse, and press
**Right Arrow** to continue. Every edit is committed to SQLite when you release the mouse, so there
is no Save button. The original page pixels are authoritative. Boxes live in original image coordinates,
and downsampling to 640×640 (or any size) happens only at export.

## Quick start

```powershell
cd lege-misc\liminal-annotator
cargo build --release
$A = ".\target\release\liminal-annotator.exe"

# 1. Create a project from the four review sets (regression, dev gold, final test, hard cases)
& $A import-dataset work\review.db D:\projects\google-colab-work\layout-detection\data

# 2. Review (opens on the first unreviewed page)
& $A work\review.db --set regression

# 3. Export a versioned dataset: original-resolution COCO + 640×640 COCO and JPEGs
& $A export work\review.db --set regression --images --reviewed-only
```

`liminal-annotator --help` lists every command.

## Keyboard and mouse

| Input | Behaviour |
|---|---|
| Right / Shift+Right | Accept the page (mark reviewed) and advance / advance without accepting |
| Left | Previous page; its status is unchanged |
| `1`–`9`, Shift+`1`–`5` | Active class for new boxes; reclassifies the selected box (physical number row, any layout) |
| Drag on blank page | Draw a box. **Shift+drag** draws even on top of an existing box (for example a whole-page ignore region) |
| Drag on a box's edge | Resize: the half of the edge you grab picks the corner (left half of the top edge = top-left). The opposite corner stays fixed; a drag past it clamps, never flips |
| Alt + edge drag | Move only one edge |
| Drag inside a box | Move it |
| Wheel, `+`/`-` | Zoom around the cursor |
| Space+drag, middle drag | Pan |
| `F` / Ctrl+`1` | Fit page / actual pixels (1 screen pixel = 1 image pixel, pixel-exact) |
| Ctrl+Z / Ctrl+Y | Undo / redo. One gesture is one step |
| Ctrl+C, **Revert page** button | Revert the page to how it was when you opened it; press again for the original imported boxes. Each revert is one undo step |
| Delete | Delete the selected box |
| `U` | Toggle the selected box as an ignore region (`iscrowd=1`) |
| `X` / `N` | Exclude the page / flag it as needing attention (both undoable) |
| Tab / Shift+Tab | Select the next / previous box in reading order |
| Ctrl+arrows (+Shift) | Nudge the selected box by 1 image px (resize) |
| `V` / `T` / `H` | Outlines / class tags / toolbars on or off |
| `C` | Compare: imported (teacher) layer → each model's predictions → differences → off |
| `Q` | Queue: all → to do → needs attention → excluded → reviewed |
| Home, PgUp/PgDn | First unreviewed page; jump 10 |
| Escape | Cancel the current drag, then clear the selection |
| F1 / F11 / F12 | Help / full screen / performance overlay |

Clicking a label chip in the toolbar does the same as its number key. The hit band around every outline
is 8 logical pixels on each side, scaled for DPI, so it is the same physical size at every zoom. Where boxes
overlap, a corner of the selected box wins, then the closest edge, then the innermost box. On a box too
narrow for two bands, the middle third stays a move target. While you hover, a white marker shows which
corner will move.

Hotkey order, class colours, cache budget, prefetch depth and hit-band width are in
`<project>.settings.json`, which is created next to the project on first run. Hotkeys map to class names,
so changing them never changes the training taxonomy.

## What is stored, and where

`<project>.db` (SQLite, WAL) is the only live state:

| Table | Contents |
|---|---|
| `pages` | One row per page: book, split, set, image path, size, queue reason, review status, revision, priority, and the original COCO `images[]` entry verbatim |
| `annotations` | The editable (human) layer, xyxy in original image pixels |
| `imported_annotations` | The layer as imported. Never written after import; diffs and the delta report join against it |
| `predictions` | Model outputs, by model name |
| `events` | Append-only log of every committed page revision |

A dedicated writer thread commits page snapshots in order (coalescing repeated snapshots of the same page)
and acknowledges each revision. The status bar shows `saving…` until the acknowledgement arrives.
Navigating away never waits for, or loses, a save: the page's document stays in memory until its write is
acknowledged. Page statuses are `unseen`, `opened`, `reviewed`, `excluded` and `needs_attention`. A page whose
image failed to load is **never** marked reviewed.

Import streams the COCO file element by element (`train.json`: 16,473 pages and 118,142 boxes in 2.5 s), and
keeps every field it does not interpret: image extras (`label_meta`, `pdf_sha1`, …) and per-box provenance
(`source`, `score`, `teachers`, `evidence`, `repairs`, …), including each box's exact source `bbox`, `area` and
`segmentation`.

## Export

`export` writes `<out>/<name>/` and builds it as `<out>/.<name>.partial`, renaming it only when complete. An
existing export is never overwritten, and source JPEGs and annotation files are only read.

| File | Contents |
|---|---|
| `original/annotations.json` | COCO at original resolution. Unedited boxes keep their source numbers byte for byte |
| `s640/annotations.json`, `s640/images/…` | Pages **stretched** to 640×640, as Lege's runtime contract requires (no letterbox), with boxes scaled per axis, `area` recomputed, and a triangle filter equivalent to PIL `BILINEAR` |
| `review.json` | The human corrections only (modified, added, deleted, per page), in original coordinates |
| `delta.json` | Corrections against the imported layer, by class, with a from→to reclassification table |
| `manifest.json` | Filter, counts per split, clamped and dropped boxes, split-leak check, file hashes |

Conventions kept: `category_id = taxonomy_id + 1`, `iscrowd = 1` for ignore regions (never converted to
background), and a box polygon as `segmentation`. Each exported box carries
`annotator: {id, change, source_id}`. Excluded pages are left out. `--reviewed-only` exports only accepted
pages. The export refuses to run if any book appears in more than one split (`--allow-split-leak` overrides).

## Model review and active learning

```powershell
& $A import-preds work\review.db preds.json --model rfdetr-s [--scale-from 640] [--set dev_gold_prefill]
& $A priority work\review.db --model rfdetr-s      # rank pages, then open with --priority
& $A compare work\review.db --model rfdetr-s       # metrics on reviewed pages only
```

- Predictions are a COCO results list (`page_key` or `image_id`, `category_id`, `bbox`, `score`).
  `--scale-from 640` un-stretches boxes predicted on the 640×640 input.
- `priority` scores pages by teacher/student text-vs-figure disagreement, text inside figures, class
  conflicts, unmatched boxes, hard layouts (aged/dark scans, gutters), rare classes and ignore regions.
  `--priority` then orders the queue by that score, **diversified by book**: books take turns, so one
  unusual book cannot use up the review budget.
- `compare` reports per-class precision/recall at IoU 0.5, text↔figure group confusion, figure recall,
  figure and text clipping. Ground truth is the editable layer of reviewed pages. Predictions and the
  imported layer are only read.

## Performance

`liminal-annotator bench [--image page.jpg] [--frames N]` drives the real render path headlessly at 1080p,
1440p and 4K. `cargo run --release --example raster_profile` times the page raster alone by zoom, strip size
and thread count. The **F12** overlay and `LIMINAL_PERF=1` (which logs every page switch to stderr) measure
the live window.

The spec's targets are < 16 ms input-to-feedback at 1080p and < 50 ms to show an already-prefetched page.
Both are met at every size, including 4K, with the margins below.

### Measured on a quiet machine (pass 2)

| | |
|---|---|
| CPU | Intel Xeon @ 2.10 GHz, 4 vCPU (4 cores × 1 thread, one socket), 16 GB. The guest has AVX2 and AVX-512; the build targets baseline x86-64 and uses neither |
| System | Linux 6.18 virtual machine, Rust 1.97.1, release profile (thin LTO), 4 render threads (`available_parallelism`) |
| Quietness | 60% idle, 0.4% steal over the final runs; no other jobs |
| Workload | `bench --frames 240`, built-in synthetic 850×1280 page, 3 runs per column, run back to back |

Frame times in ms. Each cell is **p50 range / p95 range** over the 3 runs, so the width of a range is the
run-to-run variance. *Before* is `lege-pdf-agent-over-poppler` at fd17bdd, *after* is the head of `annotator-pass2`.

| | | page raster (zoom frames) | corner-drag frame | wheel-zoom frame | pan frame | switch to prefetched page |
|---|---|---|---|---|---|---|
| 1080p | before | 1.18–1.84 / 2.20–2.79 | 1.14–1.16 / 1.30–1.42 | 2.16–2.81 / 3.27–3.72 | 2.07–2.49 / 3.00–3.15 | 4.0–6.1 / 4.8–7.1 |
| | after | 1.10–1.31 / 1.30–1.75 | 1.13–1.20 / 1.23–1.47 | 2.04–2.32 / 2.21–2.91 | 1.25–1.52 / 1.45–2.11 | 3.1–4.7 / 3.5–6.6 |
| 1440p | before | 2.55–3.05 / 3.75–4.03 | 1.75–1.86 / 1.98–2.26 | 4.22–4.69 / 5.57–5.95 | 3.48–4.44 / 4.61–5.51 | 5.7–6.8 / 7.4–8.1 |
| | after | 1.45–1.83 / 1.76–2.80 | 1.73–1.80 / 2.17–2.91 | 3.07–3.54 / 3.50–4.78 | 2.15–2.19 / 2.38–2.60 | 4.5–4.8 / 4.9–5.3 |
| 4K | before | 5.52–6.21 / 7.92–9.52 | 3.60–3.77 / 4.52–4.72 | 10.05–10.75 / 13.32–14.02 | 8.01–10.64 / 11.48–13.46 | 11.3–15.4 / 11.6–15.6 |
| | after | 2.46–2.80 / 2.90–3.50 | 3.56–3.68 / 4.20–4.45 | 6.24–6.62 / 6.92–8.45 | 4.89–4.93 / 5.69–5.84 | 9.1–9.7 / 9.1–11.4 |

The drag frame is unchanged: it only copies the cached raster and strokes outlines, and no pass-2 change
touches it. The switch column has four samples per size, so its p95 is the worst of four. The headless bench
does not include the window system: the host copies the whole window buffer to the compositor on every
present (33 MB at 4K, with softbuffer), and that cost is not in these numbers.

`raster_profile` isolates the page raster (1920×1016 and 3840×2032 canvases; p50 min–median over 5 interleaved
runs of before and after, and median p95), 4 threads:

| canvas, zoom | before p50 / p95 | after p50 / p95 |
|---|---|---|
| 1080p, 0.78 (zoomed out) | 1.64–1.66 / 2.20 | 0.82–1.00 / 1.44 |
| 1080p, 1.00 (not the exact-copy case) | 1.16–1.89 / 2.41 | 1.01–1.12 / 1.44 |
| 1080p, exactly 100% | 0.33–0.36 / 0.54 | 0.23–0.24 / 0.36 |
| 4K, 1.57 | 5.63–6.34 / 8.33 | 2.42–2.72 / 3.35 |
| 4K, 1.89 | 5.24–6.98 / 8.43 | 2.18–2.63 / 3.40 |
| 4K, 1.00 (not the exact-copy case) | 2.63–4.03 / 4.55 | 1.51–1.68 / 2.28 |
| 4K, 3× (nearest) | 0.98–1.45 / 1.76 | 0.88–1.08 / 1.32 |

Single-threaded the same rows go from 3.20 to 2.57 ms (1080p, 0.78), 3.94 to 2.76 (1080p, 1.00), 14.8 to 8.2
(4K, 1.57) and 17.3 to 8.4 (4K, 1.89). Single-threaded runs agreed to within a few percent;
the threaded ones are where the variance was.

With two busy-loop processes holding half the cores, the old threading degraded sharply and the new one holds
up (4 threads, p50 / p95): 4K at 1.57 went from 11.7 / 18.5 ms to 3.9 / 4.9 ms, and 1080p at 0.78 from
2.7 / 4.7 ms to 1.45 / 2.0 ms. That is the closest stand-in here for the development laptop, whose background
jobs kept the CPU at 97–100%.

### What was kept

1. **Pan by shifting the raster actually runs now.** The code existed but was dead: in the running app 0 of
   240 pan frames used it. `after_edit` cleared the raster key on every view change, and after any zoom the pan
   origin was an arbitrary float, so two consecutive origins never differed by an exact integer. The key
   already holds image, origin, scale and canvas rect, so the explicit clear is gone, and a pan starts by
   snapping the origin to a multiple of 2⁻²⁰ px (under a millionth of a pixel). From that grid, origin plus a
   whole number is exact, so a shifted raster is bit-identical to a full render. Now every pan frame shifts
   except the first after a zoom, which is a full render (239 of 239, and 239 of 240 at 1440p). Pan frame p50: 1080p 2.07–2.49 → 1.25–1.52 ms, 1440p
   3.48–4.44 → 2.15–2.19, 4K 8.01–10.64 → 4.89–4.93.
2. **Each source row is resampled horizontally once.** Bilinear output is a horizontal blend of two source
   rows followed by a vertical blend, and neighbouring output rows share source rows, so a two-row cache
   replaces three blends and four gathers per pixel with one vertical blend plus an amortised horizontal one.
   Output is bit-identical. At exactly 100% on whole pixels, rows are straight copies (1080p single thread
   0.60 → 0.47 ms; neutral at 4K, where it is memory-bound). The row kernels are separate functions: inlined into
   one `render_rows`, the nearest loop measured 5–47% slower from register spills alone.
3. **Row bands claimed from a queue, caller thread works.** The old code spawned one thread per equal chunk and
   the caller only joined, so a render took as long as its slowest chunk. It now spawns n − 1 threads, cuts
   rows into 4 bands per thread, and every thread claims bands from a shared queue. Raster p95 at 4K: 3.86–5.20
   → 2.90–3.23 ms, and the gap between runs narrows. It is also much less sensitive to a busy machine (above).

### What was measured and not kept

- **Persistent worker pool (rayon, same 4-band split).** It matched the scoped version and did not beat it
  (4K 1.00 p95 2.38 vs 1.84 ms; equal or slightly worse on every other row) and would add 5 crates. The crate forbids `unsafe`,
  so a scoped persistent pool is not possible in std alone. Scoped spawn here costs about 25 µs a thread
  (100 µs p50 and 200 µs p95 for four, with rare 1–2 ms outliers). Thread creation is dearer on Windows;
  see the open items.
- **Caller works, equal static chunks.** Nearest-neighbour got slower (1080p 3×: 0.34 → 0.45 ms).
- **Dirty rectangle for 4K drag frames.** A drag frame at 4K is 3.6 ms; about 1.9 ms of it is the blit of the
  cached raster, so the best case saves about half. It was not built: selection fills are alpha blends (not
  idempotent over painted pixels), and `draw_tag` places each tag around every other box's rectangle, so
  dragging one box can move other boxes' tags and the damage area is not local. A stale-pixel bug in an
  annotation tool costs more than 1.9 ms. The frame is also already about 4× under budget, and the host's
  whole-buffer present copy is untouched either way.
- **Thread count from the area drawn.** `raster_profile` now prints strips of 32–512 rows at 1, 2, 4 and 8
  threads. On 4 cores splitting pays from about 120k px per thread (1920×256: 0.76 ms on 1 thread, 0.33 on
  4) and the existing rule (1 thread below 250k px, otherwise all) already gives that. No change was
  measurable here.

### Open items

- Run `raster_profile` and `bench` on the Windows laptop. Thread creation is dearer there, and the strip
  section at 8 threads is what decides whether the thread count should follow the area.
- The remaining cost at 4K is memory traffic: shifting the raster, blitting it, and the host's present copy.
  Drawing the page straight into the window buffer, or a ring-buffered raster, are the next steps if 4K ever
  needs more headroom.

The earlier laptop figures (i7-13700H, Windows, 8 threads, before the last optimizations) were drag frame p50
0.6 ms at 1080p, 1.0–1.2 ms at 1440p and 2.9–4.3 ms at 4K, and an isolated re-raster of 1.1 ms (1080p) and
3–4 ms (4K).

How the render path works:

- The page is resampled into a canvas-sized raster that is cached, so a drag only copies it and strokes
  outlines.
- Zoomed out, the page is drawn from a mip chain. From 1:1 to 2:1 it is bilinear on the original pixels.
  From 2:1 up it is nearest-neighbour, so every source pixel shows as a crisp square. At exactly 100% it is a
  straight copy.
- A pan by whole pixels shifts the cached raster and resamples only the newly exposed strips. This is
  bit-identical to a full render, and tests check it.
- The host repaints on every cursor move. Frames that would draw identical pixels are skipped.
- The current page and the 2 next and 1 previous pages are decoded in the background into a 256 MiB LRU.
  Every load carries a generation, so a slow decode never replaces a newer page.

## Tests

`cargo test` runs 60 tests:

- **`interaction`**: press and release inside one frame; edge-to-corner resize; Escape; pointer capture
  across the toolbar; zooming mid-drag; DPI-scaled hit bands; overlap priority; Shift-draw; skipped idle
  frames; page revert; a pan after a zoom that must shift the raster and match a forced full render pixel
  for pixel.
- **`coordinate_roundtrip`**: property tests over image↔screen mapping, zoom anchoring, fit/constrain and
  stretch round trips, plus checks that drags never flip a box or push it off the page.
- **`coco_roundtrip`**: lossless round trips, including the real 88-page regression set (read from
  `$LAYOUT_DATA_DIR` or the sibling checkout, skipped when absent); edit reporting;
  split-leak refusal.
- **`persistence`**: a child process that is **aborted** after acknowledged edits, and one aborted mid-write;
  navigation while saving; writer coalescing; a page whose image failed is never marked reviewed.
- **`dataset_scale`**: a 100,000-page project (800k boxes), with index-only lookups, under 5 ms per page,
  and app start-up checked.

## Design notes

- **LiminalKit is used unchanged**, as a git dependency that `Cargo.lock` pins to the same commit lege-gui
  uses (`Cargo.toml` shows the `[patch]` for building against a local checkout). The spec
  asks for an ordered pointer stream with capture, and for full physical-key events. The low-level
  `PixelApp` host already delivers mouse, key and scroll callbacks in order, with physical key names.
  So the app implements `PixelApp` directly, as the spec allows, and builds the queue and capture on top in
  `src/canvas/pointer.rs`. `src/canvas/` (viewport, pointer, hit-test, drag state machine, raster cache)
  knows nothing about COCO or layout classes, and is shaped to move into LiminalKit as a
  `liminalkit-canvas` crate.
- One gesture is one undo entry. The preview is always recomputed from the original box and the total
  pointer displacement, so it never drifts. Tentative geometry never reaches the database.
- On Windows, winit captures the mouse on press, so a drag that leaves the window still gets its release.

## Known limitations

- Very large pages (8,000–12,000 px) are decoded whole, at 4 bytes per pixel plus 33% for mips. That fits
  the current 1280-px dataset easily. Tiled or crop decoding is the next step if such pages appear.
- Only one window is supported; there is no in-app file picker (projects open from the command line).
- Model inference is not built in, by design: predictions are imported as files.
