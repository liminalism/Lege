# layout-rfdetr-s.onnx — Lege's book layout model

- **Architecture:** RF-DETR-S (DINOv2-S backbone + deformable DETR decoder), Apache-2.0
  (`rfdetr` package, COCO-pretrained small checkpoint).
- **Training (2026-11-07/08, Colab A100):** 12 epochs at 640 on 16,473 pages from 591 of the owner's
  own books. Labels are pseudo-labels: PP-DocLayout-L and DocLayout-YOLO detections fused with
  text-layer evidence; single-teacher boxes without text evidence are ignore regions (per-group
  logit silencing). No PubLayNet. Best EMA checkpoint (epoch 6).
- **Export:** `train/export_lege.py` in `google-colab-work/layout-detection`: the RF-DETR graph plus a
  head that converts cx,cy,w,h to xyxy x 640 and maps its 14 book classes onto PP-DocLayout's 23 class
  ids, so it is a drop-in for the PP-DocLayout slot (`pp_image` [1,3,640,640], stretched page, ImageNet
  normalisation; outputs `boxes` [1,300,4] and `scores` [1,23,300]). Weights stored as fp16 (no Cast
  nodes; lege-gpu widens them at load), 58 MB.
- **Measured (against pseudo-labels, three held-out test books, decode threshold 0.20):** AP50 0.835
  vs 0.563 for PP-DocLayout-M; clipped text 11.7% vs 24%; missed text 1.9% vs 2.5%. The owner's
  gold-label review is still pending. fp16 storage changed no metric beyond +-0.003.
- **Speed (lege-gpu, RTX 4060 Laptop):** about 32 ms per page with the tensor-core GEMM on Vulkan,
  42 ms on the f32 kernels.
- **Runtime override:** a `layout.onnx` (or legacy `doclayout.onnx`) asset next to the executable or in
  `LEGE_DATA_DIR`. The old PP-DocLayout-M can be embedded instead with the `doclayout-m` feature.
