# Integrating Lege-Dewarp into lege-process

Status 2026-11-14: implemented the same day, as "Flatten page" in the GUI and `--flatten` on
the command line (`--dewarp*` are aliases). What follows is the plan it was built from; where
the result differs, this list is right and the text below is not:

- The lege-gpu adapter is `DewarpNet::run(landscape, chw)`: it takes the prepared input
  tensor. The antialiasing resize stays in `lege-process/dewarp/imgops.rs` with the rest of
  the port.
- The model files are `lege-process/models/lege-dewarp-{576x384,384x576}.fp16.onnx`, embedded
  behind the default `dewarp` feature (not runtime assets only); the model card is
  `lege-process/models/lege-dewarp.md`.
- A final grid with more than 5 % folded cells passes the photo through (the tester only
  records the share; 252 good pages of its sets peak at 0.3 %).
- Flags are `--flatten`, `--flatten-pages 1|2`, `--flatten-fast`.
- Measured through Lege: see the evidence records under
  `lege-ecosystem.work.integrate-lege-dewarp`.

Lege-Dewarp flattens photographed book pages: a hand-held photo of a curved page or of a
two-page spread goes in, one flat image per page comes out. Pages that are already flat
(scans) are detected and returned untouched, which is what lets the step stay switched on.

## What exists

| Thing | Where |
|---|---|
| Model files (ft4, 2026-11-14) | `lege-dewarp-ft4-20261114/` at the repository root (untracked) |
| Reference implementation | the `dewarp-tester` crate, kept with the test photos outside this repository: `src/own.rs` (the passes, 475 lines), `src/geom.rs` (grids, output size), `src/imgops.rs` (resize, render), `src/baselines.rs` (`grid_displacement`), `src/backend.rs` (lege-gpu worker) |
| All measurements | `dewarp-tester/README.md` |
| Training log, export script | `google-colab-work/dewarp` (`NOTES.md`, `train/export_lege.py`) |
| Ledger | `lege-ecosystem.work.own-page-dewarp-model` and its evidence records |

### The model files

Two files hold the same weights at two static input shapes. Both are needed: the graph's
reshapes are baked for one token grid, so one file cannot serve both orientations.

| File | Input `image` | Used for | Size |
|---|---|---|---|
| `lege-dewarp-576x384.fp16.onnx` | [1, 3, 576, 384] | views taller than wide (a page) | 10.58 MB |
| `lege-dewarp-384x576.fp16.onnx` | [1, 3, 384, 576] | views wider than tall (a spread, a landscape photo) | 10.58 MB |

The `.onnx` files without `fp16` are the fp32 twins (21 MB each). They exist only for
cross-checks in ONNX Runtime, which cannot load the fp16 files; Lege does not ship them.
The `.json` beside each file records the checkpoint, the op counts and export parity.

- **Input:** float32 NCHW, RGB in [0, 1]. No mean or standard deviation (they are folded
  into the first convolution). The view is resized with an antialiasing filter
  (`imgops::resize_antialias_chw`); a plain bilinear shrink of a 24 MP photo aliases.
- **Output `flow` [1, 2, H/8, W/8]:** x then y. A correction to the identity map, in view
  values where -1 and +1 are the view's edges. Cell (i, j) of the flat page reads the view
  at `cell_centre(j) + flow_x`, `cell_centre(i) + flow_y`. It is a backward map: it says
  where to read, not where to move.
- **Output `maps` [1, 2, H/8, W/8]:** channel 0 page mask, channel 1 gutter heat, both as
  probabilities over the view.
- **Output `scores` [1, 2]:** `bend` (the page is not flat) and `spread` (two pages), both
  as probabilities.
- **lege-gpu:** loads unchanged under the existing `image` input target
  (`lege-gpu/src/vision/onnx/load.rs`), 13 ms per call warm on an RTX 4060 Laptop
  (Vulkan). fp16 initializers are widened at load. Ops: Conv, Gemm, MatMul, Softmax, Erf,
  ReduceMean, Resize, Reshape, Transpose and elementwise ops, all already supported.

### What it measures at

Distortion left after flattening, in % of the page diagonal, measured by registering a
flat scan of the same page to the output (lower is better):

| Test | Paddle UVDoc | DocScanner-L | flattener | Lege-Dewarp |
|---|---|---|---|---|
| Cookbook with pictures, spreads not trained on | 0.53 | 0.43 | 0.43 | 0.25 |
| A text book never trained on | 0.51 | 0.45 | 0.45 | 0.25 |

- **Flat pages:** 6 of 6 test pages and 406 of 408 held-out scans are returned untouched;
  none of 590 curved photos is. The other three models alter every flat page.
- **Hand-held photos of spreads (26):** level with Paddle UVDoc on text-line straightness,
  not better.
- **Time per photo after decoding, in the tester:** 0.26 s for a 10 MP spread and 0.69 s
  for a 24 MP spread (ten model calls, two pages out); 0.02 to 0.03 s for a flat page (one
  call). About 0.13 s of a spread is the model; the rest is rendering on the CPU.

## What has to be built

Three pieces, in dependency order.

### 1. lege-gpu: a resident adapter

`OnnxSession::run_gpu` compiles the graph on every call, so it is not usable here. The
tester works around that with a worker thread around `run_gpu_each`; Lege should own a
compiled graph instead, as `SauvolaProcessor` does (`lege-gpu/src/vision/api.rs`).

Add a `DewarpNet` (name open) to `vision/api.rs` and export it from `vision/mod.rs`:

- `from_model_bytes(portrait, landscape)` and `from_model_paths(..)`: load both protos,
  check `ModelReport` for rejections, prepare each with its static dims, compile the first
  with `CompiledGraph::build` and the second with `build_related` so they share a device.
- `run(&self, view: &RgbImage) -> Result<DewarpOutput>`: picks the file by the view's
  orientation (wider than tall takes the landscape file), resizes with antialiasing to
  the model size, runs, and returns `flow`, `maps` and `scores` by output name.
- A `Mutex` around the compiled graphs: `CompiledGraph` is single-flight because its
  buffers are reused. One page needs up to ten calls in sequence, so hold the lock per
  call, not per page.
- Unlike the binarizer there is no size cache to manage: both shapes are fixed.

There is no practical CPU fallback. On lege-gpu's CPU reference executor fewer than three
calls finished in five minutes on the development machine. Without a usable GPU adapter
the step must switch itself off and pass pages through.

### 2. lege-process: the dewarp module

A new subsystem, for example `lege-process/dewarp/`, wired from `core/lib.rs` with
`#[path]` like the others. It is a port of the tester, about 1,000 lines with no new
dependency:

| From the tester | What it is |
|---|---|
| `own.rs`: `scan`, `predict`, `centred_to_grid`, `view_of`, `gutter_column`, `sample_grid`, `smooth_grid`, `resample` | the passes and the gate |
| `geom.rs`: `Grid`, `center_position`, `outline_points`, `bounds_of`, `median_drift`, `split_grid`, `dense_map`, `plan_output` with its aspect estimate, `folded_cell_fraction` | grid geometry and the output size |
| `baselines.rs`: `grid_displacement` and its `stats` | how far a map is from "do nothing": p99 move, bend after the best shift, zoom and turn are removed, zoom, turn |
| `imgops.rs`: `resize_antialias_chw`, `render` | model input and the final resampling |

Port it rather than rewrite it. Every constant below was tuned or verified with these
exact functions, and the tester can then check the port (see Verification).

**The passes** (`OwnScanner::scan`), for one decoded photo at its native size:

1. **Whole frame.** Run the model on the whole photo. Turn the cell-centred map into an
   edge-aligned grid of nodes in photo coordinates.
2. **Gate.** Measure that grid against the identity. Return the photo untouched when
   any rule holds:
   - `bend` < 0.5 and the 99th percentile of the move is under 0.4 % of the diagonal;
   - `bend` < 0.1 and that percentile is under 3 % (a flat page lying slightly off);
   - the page fills the frame: largest bend under 1.2 % of the diagonal, zoom within 8 %
     of 1, turn under 2 degrees.
3. **Crop pass.** Run the model again on the bounds of the predicted page plus a 5 %
   margin. This grid replaces the first.
4. **Spread.** When `spread` of the crop pass is 0.5 or more, find the column where the
   gutter heat peaks along the predicted page, searching the middle 40 % of the columns.
   Cut there if the peak's mean heat is at least 0.15, otherwise in the middle. Run the
   model once per half on that half's bounds; keep a half's result only if its median
   drift from the cut grid is at most 0.06 and it is finite (a page pass that wanders onto
   the facing page is discarded).
5. **Refinement, three rounds.** For each page: render a working copy through the current
   grid (long side at most 1100 px), run the model on it, and compose the correction onto
   the grid. Skip a correction that would move the page by 30 % of its diagonal or more.
   The working copies are model input only.
6. **Smoothing.** Replace each page's grid by a polynomial surface through its nodes
   (Chebyshev basis, degree 5 across and 3 down, Huber reweighting).
7. **Render.** Output width is the longest grid row in photo pixels; the aspect comes from
   the grid's arc lengths, or from perspective when the focal length is known, snapped to
   common paper sizes when close. Each page is sampled from the original photo exactly
   once, bilinear, white where the map leaves the photo.

Settings and their tested values (the tester's defaults):

| Setting | Value | Tester flag |
|---|---|---|
| Strict gate, share of the diagonal | 0.004 | `--gate` |
| Flat gate, share of the diagonal | 0.03 | `--flat-gate` |
| Fill gate, bend share | 0.012 | `--fill-gate` |
| Fill gate, turn in degrees | 2.0 | `--fill-turn` |
| Crop margin | 0.05 | (constant `MARGIN`) |
| Refinement rounds | 3 | `--refine` |
| Refinement limit, share of the diagonal | 0.30 | `--refine-limit` |
| Smoothing degrees, across and down | 5, 3 | `--smooth` |
| Force one or two pages | auto | `--pages` |

Keep these as named constants in one place; expose only the user-facing ones (below).

A faster variant exists and is measured: no refinement and no smoothing (`--refine 0
--smooth 0,0`) is four model calls per spread instead of ten and about half the time. Its
distortion is 0.23 and 0.28 on the two books above, still ahead of the other models, but
on the 26 hand-held photos its line spread is 1.9 degrees against 1.4.

**Failure handling.** Dewarping must never fail a page. On a model error, a non-finite
grid, a grid with no extent (`plan_output` returns `None`) or folded cells, return the
photo as it came and log it.

### 3. lege-process: where it runs

Only image inputs: the folder and ZIP modes in `lege-process/core/pnginference.rs`, whose
pages come from `ImageFolderPageSource` and `ZipImagePageSource`
(`lege-process/pipeline/source.rs`). PDF pages are rendered, not photographed; leave them
alone unless a later option asks otherwise.

**Before the resize.** Both sources decode at native size and then call
`normalize_folder_image_to_target`. Dewarping goes between the two: the final render
needs the photo's own pixels, and the flattened page is what should be normalised to the
target height. `SourcePage::original_width_pts` and `original_height_pts` must then be
the flattened page's size, not the photo's.

**One photo can become two pages.** This is the one structural problem. `PageSource`
promises a fixed `page_count()`, and `source_stage`, progress and the writer all count on
it. The fitting solution is a wrapping source, built when dewarping is on:

- At construction it surveys every photo: decode, run steps 1 to 6, keep only the result
  (untouched, or one or two small grids), drop the pixels. `page_count()` is the sum.
- `load_page(i)` maps `i` to a photo and a side, decodes the photo again, and renders
  that one page from its stored grid (step 7).
- A spread's two pages are asked for back to back, so a one-entry cache of the last
  decoded photo saves the second decode.
- `native_page_size` reports the photo's size, because the decode is the peak
  allocation; `decodes_at_native_size` stays true.

The survey costs one extra decode per photo and its model calls up front, before the
first page appears, so it needs its own progress phase. A cheaper survey from a reduced
decode is plausible, since no model input is larger than 576 x 384 and no working copy
larger than 1100 px, but that changes the model's inputs and has not been measured.

**Orientation.** The model expects upright pages. The tester applies the EXIF orientation
when it decodes (`imgops::load_photo`); `decode_folder_image` in Lege does not read EXIF
at all. Apply the EXIF orientation before the first pass, and read the 35 mm focal length
from the same block for the aspect estimate (optional; the arc-length estimate is used
without it).

**After it.** Nothing downstream changes: layout detection, margins, binarization and
encoding see an ordinary upright page image.

### Switches

Lege's CLI is a hand-written flag match in `lege-process/core/main.rs`, with the state in
`PipelineConfig` (`lege-process/pipeline/config.rs`). Proposed:

| Flag | Meaning |
|---|---|
| `--dewarp` | flatten photographed pages in folder and ZIP input |
| `--dewarp-pages 1\|2` | do not decide per photo: every photo is one page, or a spread |
| `--dewarp-fast` | one pass per page (no refinement, no smoothing) |

Recommended default: **off**, with a checkbox in the GUI for image input ("photographed
pages"). The gate is good on text scans, but see the first known limit below; a folder of
well-scanned illustrated pages should not be touched by default.

### Shipping the model

Follow the layout model's pattern in `lege-process/core/lib.rs`: two `include_bytes!`
statics behind a `dewarp` feature, and a runtime override through
`runtime_asset_path_if_exists` (for example `lege-dewarp-576x384.onnx` and
`lege-dewarp-384x576.onnx` beside the executable or in `LEGE_DATA_DIR`). Embedding adds
21 MB to the binary. Starting with runtime assets only, and embedding once the feature is
settled, keeps the default build unchanged meanwhile.

Move the two fp16 files to `lege-process/models/` and add a model card
`lege-process/models/lege-dewarp.md` in the style of `layout-rfdetr-s.md`.

## Verification

The tester stays the reference. Each check has a number to hit.

| Check | How | Expect |
|---|---|---|
| The adapter returns what the tester's worker returns | same view through `DewarpNet::run` and `dewarp-tester`'s lege-gpu backend | identical tensors |
| The port computes the same grids | run the 26 photos through Lege's module, compare page grids with the `grid` in the tester's per-photo JSON (decode with the same crate) | under 1 px mean in the photo |
| Flat pages are left alone | the tester's `flat-pages` (6) and `flat-heldout` (408) folders through Lege | 6 and 406 untouched |
| Curved photos are not | the 26 photos and Book100 | 0 untouched |
| Straightness of Lege's output pages | `dewarp-tester measure` on the pages Lege writes for the 26 photos | tilt 1.08, spread 1.36 deg, bow 0.345 %, within 0.2 deg |
| Page counts | a folder mixing single pages, spreads and flat scans | the document's page count equals the survey's; order preserved |
| No GPU | run with the GPU disabled | pages pass through, one log line, no error |
| Cost | 24 MP spread photos | about 0.7 s per photo beyond decode; flat pages about 0.03 s |

The photo sets are too large for the repository. Point the tests at them with an
environment variable and mark them `#[ignore]`, as other asset-dependent tests here do.

## Known limits

- **Flat pages that are photographs.** A full-page photograph has no text lines or page
  edges to judge flatness by. Of 400 flat scan pages of an illustrated cookbook the model
  altered 47, a few of them badly. Text scans are safe (263 of 264 untouched on two other
  books). This is why the default should be off. The cure is training data: flat samples
  from scans of illustrated books.
- **Hand-held photos.** On the 26 hand-held spreads it is level with Paddle UVDoc, not
  ahead. The clear gains were measured on books photographed lying on a surface.
- **Left pages of a spread** keep more distortion than right pages (0.36 against 0.16 on
  the unseen text book).
- **One resolution.** The model was trained and tested at 576 x 384 only; a 768 x 512
  export of the same weights is worse.
- **Thick books:** the page edge can include the edge of the page block.
- **Training data and licence.** The mix includes Book3D, which is tagged MIT on Hugging
  Face while its authors' repository words it as for non-commercial research. It was
  kept in its own shard sets so the model can be retrained without it. Settle this
  before the model ships in a public release.

## Order of work

1. `DewarpNet` in lege-gpu, with the tensor parity check.
2. The dewarp module in lege-process as a library function on one decoded image, with the
   grid parity check against the tester.
3. The wrapping source with the survey, EXIF orientation, and the switches; runtime
   assets only.
4. The flat, curved and page-count checks through the real pipeline; then the GUI
   checkbox.
5. Model card, embedding behind the `dewarp` feature, and the licence decision.
