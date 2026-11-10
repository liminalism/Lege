# layout-rfdetr-s.onnx — Lege's book layout model (v2, 2026-11-10)

- **Architecture:** RF-DETR-S (DINOv2-S backbone + deformable DETR decoder), Apache-2.0
  (`rfdetr` 1.12.0, COCO-pretrained small checkpoint, detection head re-initialised).
- **Training (2026-11-10, Colab A100, two sessions):** 6 epochs at 640, batch 16, on 64,785 pages
  from 547 of the owner's own books. Labels are DocLayout-YOLO detections with PP-DocLayout-L's
  labels for its `abandon` boxes, bulk-corrected and then reviewed by hand on 16,090 of the pages
  (figure boxes above all). Final epoch, which was also the best on validation (EMA mAP50:95 0.645).
  The previous model (v1, 2026-11-08, 16,473 pages of fused pseudo-labels) is in git history.
- **Export:** `train/export_lege.py` and `train/finish_v2_for_lege.py` in
  `google-colab-work/layout-detection`: the RF-DETR graph plus a head that converts cx,cy,w,h to
  xyxy x 640 and maps the book classes onto PP-DocLayout's 23 class ids, so it is a drop-in for the
  PP-DocLayout slot (`pp_image` [1,3,640,640], stretched page, ImageNet normalisation; outputs
  `boxes` [1,300,4] and `scores` [1,23,300]). toc, reference and formula are never emitted (their
  rows of the class map are zero): Lege has no use for them and such regions come out as text.
  Weights stored as fp16 (no Cast nodes; lege-gpu widens them at load), 58 MB.
- **Measured on pages reviewed by hand, from books neither model was trained on (decode threshold
  0.20), v1 -> v2:**

  | set | AP50 | AP50:95 | clipped text | missed text | false text area | over-merged | figure AP50 |
  |---|---|---|---|---|---|---|---|
  | dev, 928 pages of 20 books | 0.678 -> 0.739 | 0.547 -> 0.667 | 24.1% -> 7.7% | 0.7% -> 0.4% | 8.8% -> 5.7% | 4.4% -> 6.1% | 0.961 -> 0.979 |
  | final test, 514 pages of 22 books | 0.800 -> 0.845 | 0.689 -> 0.789 | 17.8% -> 3.8% | 0.5% -> 0.5% | 3.2% -> 1.6% | 2.9% -> 2.2% | 0.962 -> 0.973 |

  On v1's own pseudo-labelled sets v2 has less clipped text (regression 11.7% -> 9.0%, dev_auto
  7.7% -> 5.3%) but lower AP (regression AP50 0.835 -> 0.703), because those labels carry toc,
  reference, formula and footer conventions that v2's training set does not.
- **Through Lege itself (2026-11-10):** Lege's own detections (its threshold, NMS and
  post-processing) on 882 reviewed pages of ten of the dev books, read from `LEGE_BBOX_TRACE`
  and scored with `train/eval_lege_trace.py`. The corrections written for PP-DocLayout-M's
  under-detection (`layout_correct`: widening text boxes to ink, synthetic text boxes, image
  re-detection) hurt this model and were removed with it:

  | | with the old corrections | without |
  |---|---|---|
  | AP50 / AP50:95 | 0.486 / 0.363 | 0.663 / 0.598 |
  | text AP50, caption AP50 | 0.527, 0.139 | 0.863, 0.743 |
  | over-merged text | 24.4% | 3.6% |
  | false text area | 10.7% | 3.7% |
  | text coverage, missed text | 0.970, 2.8% | 0.967, 3.1% |
  | running headers and page numbers found as furniture | 810 of 1,034 | 1,003 of 1,034 |
  | ... found as text (they leak into crops and reflow) | 5 | 15 |
  | headings found | 403 of 504 | 428 of 504 |

- **Speed:** the same graph as v1 (about 32 ms per page with the tensor-core GEMM on Vulkan on an
  RTX 4060 Laptop).
- **Runtime override:** a `layout.onnx` (or legacy `doclayout.onnx`) asset next to the executable or in
  `LEGE_DATA_DIR`. The old PP-DocLayout-M can be embedded instead with the `doclayout-m` feature.
