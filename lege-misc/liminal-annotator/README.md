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

`liminal-annotator bench [--image page.jpg]` drives the real render path headlessly at 1080p, 1440p and 4K.
The **F12** overlay and `LIMINAL_PERF=1` (which logs every page switch to stderr) measure the live window.
Measured on the development laptop while the machine was otherwise quiet (release build, 880×1280 scan,
8 render threads):

| | 1080p | 1440p | 4K |
|---|---|---|---|
| Frame during a continuous corner drag (p50) | 0.6 ms | 1.0–1.2 ms | 2.9–4.3 ms |
| Page re-raster on zoom (isolated, p50) | 1.1 ms | — | 3–4 ms |
| Switch to a prefetched page, live window | 1.7–2.3 ms | | |

The spec's targets are < 16 ms input-to-feedback at 1080p and < 50 ms to switch to a prefetched page. How
it gets there:

- The page is resampled into a canvas-sized raster that is cached, so a drag only copies it and strokes
  outlines.
- Zoomed out, the page is drawn from a mip chain. From 1:1 to 2:1 it is bilinear on the original pixels.
  From 2:1 up it is nearest-neighbour, so every source pixel shows as a crisp square. At exactly 100% it is a
  straight copy.
- A pan by whole pixels shifts the cached raster and resamples only the newly exposed strips. This is
  bit-identical to a full render, and a test checks it.
- The host repaints on every cursor move. Frames that would draw identical pixels are skipped.
- The current page and the 2 next and 1 previous pages are decoded in the background into a 256 MiB LRU.
  Every load carries a generation, so a slow decode never replaces a newer page.

## Tests

`cargo test` runs 57 tests:

- **`interaction`**: press and release inside one frame; edge-to-corner resize; Escape; pointer capture
  across the toolbar; zooming mid-drag; DPI-scaled hit bands; overlap priority; Shift-draw; skipped idle
  frames; page revert.
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
