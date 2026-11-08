# Layout models measured: RF-DETR-S, PP-DocLayout-M, PP-DocLayout-L (2026-11-08)

Question asked: should the shipped RF-DETR-S be tested against PP-DocLayout-L
now that Lege runs Vulkan only, and does the model need retraining on more
pages (the owner saw missed detections on the globalhistory PDF)?

Short answers:

- **PP-DocLayout-L cannot run in Lege as exported.** Its paddle2onnx graph
  takes `image`, `im_shape` and `scale_factor`, does NMS in-graph and returns
  `[300·N, 6]` rows; lege-gpu rejects it for seven unsupported ops. It was only
  ever a teacher for pseudo-labelling. Running it in Lege needs a re-export cut
  before NMS plus `Range`, `BatchNormalization`, `Clip`, `Floor`, `GatherND`,
  `Log` and `Mod` in the wgpu runtime. Its accuracy was measured here through
  onnxruntime instead.
- **Accuracy order on the globalhistory pages is PP-L > RF-DETR-S > PP-M**, and
  RF-DETR-S beats PP-M on every under-detection metric. At Lege's production
  threshold of 0.35, though, RF-DETR-S loses most of its margin: its scores are
  calibrated lower than PP-M's, so the 0.35 cut-off (tuned for the PP-M-era
  model) drops real text blocks. The two globalhistory misses are the dust
  jacket and a full-page time chart; both are detected at 0.20.
- **In the Lege output, a second effect hides text on exactly these pages:**
  a text region inside a detected image region is carved away, so a dust
  jacket or a full-page chart detected as `image` keeps no text regions with
  any model. The raw detections are correct; the policy decides.
- **Retraining on 100k pages is not the first lever.** The training labels are
  pseudo-labels from PP-L and DocLayout-YOLO; the gold review (180 dev and 130
  test pages) has not been done, so every number below is against teacher
  labels and the teacher is the ceiling. Lowering the threshold for this model
  (or recalibrating its scores at export) recovers the observed misses without
  training anything.

## Models

| Model | File | Size | Lege contract | Vulkan latency, 1×3×640×640, median of 30 |
|---|---|---|---|---|
| RF-DETR-S (shipped, embedded) | `lege-process/models/layout-rfdetr-s.onnx` | 48 MB | yes (`pp_image` → `boxes` [1,300,4], `scores` [1,23,300]) | 32.7 ms |
| PP-DocLayout-M (`doclayout-m` feature, on-disk override) | `lege-process/models/doclayout.onnx` | 23 MB | yes | 22.0 ms |
| PP-DocLayout-L (teacher) | `D:\projects\google-colab-work\layout-detection\models\pp-doclayout-l\pp-doclayout-l.onnx` | 129 MB | **no** (3 inputs, post-NMS output) | does not load |

`bench_onnx_gpu` (debug-fast, RTX, tensor-core GEMM on). PP-L on DirectML
through onnxruntime measured about 55 ms in the training workspace; a Vulkan
figure needs the re-export above.

## Data

Training workspace `D:\projects\google-colab-work\layout-detection` (DATASET.md):

| Set | Pages | Books | Boxes | Labels |
|---|---|---|---|---|
| train | 16,473 | 591 | 111,636 | pseudo (PP-L + DocLayout-YOLO agreement, synthesis rules) |
| dev_auto | 544 | 20 | 3,483 | pseudo; 28 pages per dev book |
| dev_gold_prefill | 180 | 20 | | pre-filled, **review pending** |
| final_test | 130 | 14 | | pre-filled, **review pending** |
| regression | 88 | 3 | 494 | pseudo; sahib, Structures, confed |
| globalhistory subset of dev_auto | 28 | 1 | 279 | pseudo; `globalhistoryofm0000unse-1`, dark scan, 808 pages in the PDF |

So "the DETR test set" is 544 + 88 teacher-labelled pages; no human-verified
page has been scored yet. globalhistory is a dev book: the model never trained
on it.

## Accuracy

`train/eval_any.py` (new; wraps `eval_layout.py` so that PP-L's interface can
be scored with the same metrics), onnxruntime CPU, decode as lege-gpu does
(score cut-off, class-aware NMS 0.5, at most 300). AP is pycocotools over the
14 book classes; the text metrics are the under-detection metrics defined in
`eval_layout.py` (coverage of GT text-like ink, missed under 50 % covered,
clipped under 95 %, false text area).

### globalhistory, 28 pages

| Model, threshold | AP50 | AP50:95 | text coverage | missed | clipped | false text |
|---|---|---|---|---|---|---|
| RF-DETR-S 0.20 | 0.826 | 0.728 | 0.993 | 0.000 | 0.037 | 0.019 |
| RF-DETR-S 0.25 | 0.817 | 0.720 | 0.973 | 0.009 | 0.037 | 0.019 |
| RF-DETR-S 0.30 | 0.698 | 0.636 | 0.969 | 0.019 | 0.033 | 0.019 |
| **RF-DETR-S 0.35 (Lege default)** | 0.632 | 0.577 | 0.966 | 0.023 | 0.028 | 0.019 |
| PP-M 0.20 | 0.793 | 0.640 | 0.945 | 0.028 | 0.266 | 0.031 |
| PP-M 0.35 | 0.626 | 0.521 | 0.940 | 0.056 | 0.266 | 0.017 |
| PP-L 0.20 | 0.937 | 0.766 | 0.950 | 0.005 | 0.243 | 0.021 |

Per class, AP50 at 0.20:

| Class (GT boxes) | RF-DETR-S | PP-M | PP-L |
|---|---|---|---|
| text (152) | 0.965 | 0.938 | 0.953 |
| footnote (31) | 0.494 | 0.434 | 0.535 |
| page_number (26) | 1.0 | 1.0 | 1.0 |
| figure (25) | 0.945 | 0.998 | 0.996 |
| heading (20) | 0.964 | 0.992 | 1.0 |
| caption (15) | 0.808 | 0.723 | 0.952 |
| running_header (5) | 0.752 | 0.546 | 1.0 |
| table (2) | 0.505 | 0.505 | 1.0 |

PP-L's lead is in captions, running headers and the two tables; on body text
RF-DETR-S is at least its equal. PP-L and PP-M clip a quarter of the text
boxes (their boxes stop short of the last line); RF-DETR-S clips 3 %. Note
that the labels are PP-L-derived, which flatters PP-L's AP.

The pages RF-DETR-S misses at 0.35 and finds at 0.20:

- `p0006`, the dust jacket: the two author columns on the grey ground (text
  and footnote-labelled blocks at x 544-892) score between 0.20 and 0.35.
- `p0533`, "Time chart for the Indian subcontinent": the whole right-hand
  column of dated entries is one GT text box; at 0.35 only 21 % of it is
  covered, at 0.20 all of it.
- `p0192`: a caption and a footnote are clipped (0.93 and 0.95 of the box) at
  every threshold.

### dev_auto (544 pages, 20 books) and regression (88 pages)

| Split | Model, threshold | AP50 | AP50:95 | text coverage | missed | clipped | false text |
|---|---|---|---|---|---|---|---|
| dev_auto | RF-DETR-S 0.20 | 0.695 | 0.618 | 0.982 | 0.006 | 0.077 | 0.022 |
| dev_auto | RF-DETR-S 0.25 | 0.674 | 0.605 | 0.979 | 0.009 | 0.083 | 0.016 |
| dev_auto | RF-DETR-S 0.35 | 0.618 | 0.557 | 0.963 | 0.025 | 0.087 | 0.012 |
| dev_auto | PP-M 0.20 | 0.576 | 0.432 | 0.954 | 0.020 | 0.290 | 0.025 |
| dev_auto | PP-M 0.35 | 0.532 | 0.401 | 0.928 | 0.064 | 0.285 | 0.014 |
| dev_auto | PP-L 0.20 | 0.785 | 0.636 | 0.977 | 0.007 | 0.198 | 0.023 |
| regression | RF-DETR-S 0.20 | 0.834 | 0.775 | 0.992 | 0.019 | 0.115 | 0.011 |
| regression | RF-DETR-S 0.25 | 0.832 | 0.774 | 0.991 | 0.022 | 0.112 | 0.011 |
| regression | RF-DETR-S 0.35 | 0.804 | 0.750 | 0.981 | 0.036 | 0.107 | 0.010 |
| regression | PP-M 0.20 | 0.563 | 0.447 | 0.982 | 0.025 | 0.240 | 0.017 |
| regression | PP-M 0.35 | 0.533 | 0.428 | 0.958 | 0.079 | 0.249 | 0.008 |
| regression | PP-L 0.20 | 0.823 | 0.697 | 0.987 | 0.000 | 0.164 | 0.017 |

Same ordering on every split and every metric that measures under-detection:
PP-L first on AP, RF-DETR-S close behind and well ahead of PP-M; RF-DETR-S has
the lowest clipped-text rate by a factor of two to three (the PP models stop
boxes short of the last line). Going from 0.20 to 0.35 costs RF-DETR-S four
times its missed-text rate on dev_auto (0.6 % to 2.5 %) and twice on the
regression books (1.9 % to 3.6 %), for a false-text-area saving of one
percentage point.

## End to end through Lege

One `lege` binary (debug-fast, 2026-11-08), `--no-ocr`, `LEGE_BBOX_TRACE=1`,
embedded RF-DETR-S against PP-M supplied as `doclayout.onnx` through
`LEGE_DATA_DIR`, both at the production threshold 0.35:

| Document, pages | RF-DETR-S regions | PP-M regions | Image regions |
|---|---|---|---|
| globalhistory 1-40 | 482 | 480 | 27 / 27 |
| globalhistory 525-560 | 762 | 748 | 24 / 26 |
| sahib 1-10 | 13 trace lines | 11 | 1 / 0 |
| Structures 1-12 | identical traces | | |

(Region counts are `[REMAP]` lines, which the pipeline prints twice per
region on some paths; the comparison is like for like.)

`LEGE_LAYOUT_TRACE=1` on globalhistory page 7 (the dust jacket) shows
lege-gpu's decode agreeing with onnxruntime for both models: RF-DETR-S gives
the title at 0.81, the two text blocks at 0.62 and 0.58, the stamp at 0.41,
and the two full-page image boxes at 0.63 and 0.57; the six author-name
blocks score 0.25-0.31 and fall under 0.35. What the `--layout-visualize`
overlays then show is not a detection fault: every text region that lies
inside a detected image region is carved down to the part outside it (to a
2 px sliver when the image is the whole page, to the strip right of x=748
when PP-M's image box ends there). On the dust jacket and on the time chart
(page 534, image at 0.61 / 0.67) the page is therefore handled as a photo by
both models, and no text region survives to the MRC layer whichever model
runs. That is pipeline policy (`premask_figures`, the full-page image
handling in `pdf_tokio_pipeline.rs`), not model recall, and it is the second
thing a reader of the globalhistory output would experience as "missed
detections".

Overlays for pages 7, 193 and 534 with each model are in the session
scratchpad under `model-ab/vis/` (written by `--layout-visualize 1600`).

## What to do

1. **Threshold.** `PipelineConfig::new` sets `confidence_threshold: 0.35` with
   a comment about the *shipped PP-M-era model's* engraving scores. RF-DETR-S
   is calibrated lower. Measure 0.20 to 0.25 on the full dev_auto set for false
   positives (the overlap guard downstream removes text-column false positives)
   and move the default, or fold a score scale into the export. This is the
   change that recovers the globalhistory misses.
2. **Gold review before any retraining.** 180 dev and 130 test pages are
   pre-filled and waiting. Until they are corrected, model selection is against
   PP-L's own mistakes, and a 100k-page pseudo-labelled set would inherit the
   same ceiling (the clipped-caption pattern above is a teacher pattern).
3. **If retraining:** more pages per book of the under-represented classes
   (footnote 0.49 AP50 on every model, caption, running header), not more
   books at 28 pages each. The train set already has 16.5k pages; the misses
   seen here are threshold and label-quality effects, not capacity.
4. **PP-DocLayout-L in Lege** needs a pre-NMS re-export (the workspace has
   `train/cut_before_nms.py` for the RT-DETR head) plus seven ops in lege-gpu,
   and would cost about 1.7× RF-DETR-S's latency if the DirectML ratio holds.
   Its measured advantage is captions, running headers and tables. Worth a
   feasibility pass only after 1 and 2.

## Reproduce

```
CW=D:\projects\google-colab-work\layout-detection
$CW\.venv-ort\Scripts\python.exe -I $CW\train\eval_any.py <model.onnx> $CW\data <split>.json [--ppl] [--thr 0.20] [--out r.json] [--per-image p.json]
target\debug-fast\bench_onnx_gpu.exe <model.onnx> 1,3,640,640 30
```

`globalhistory_dev_auto.json` is the globalhistory subset of `dev_auto.json`
(same ids), kept beside the other splits.
