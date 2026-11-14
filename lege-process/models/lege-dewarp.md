# lege-dewarp-576x384.fp16.onnx, lege-dewarp-384x576.fp16.onnx — the "Flatten page" model (ft4, 2026-11-14)

Lege-Dewarp flattens photographed book pages: a hand-held photo of a curved page or of a
two-page spread goes in, one flat image per page comes out. Pages that are already flat (scans)
are detected and returned untouched, which is what lets the step stay switched on for a mixed
folder. In the GUI it is the **Flatten page** toggle beside Resolution; on the command line
`--flatten`.

- **Files:** the same weights at two static input shapes, because the graph's reshapes are baked
  for one token grid. `576x384` (`image` [1,3,576,384]) serves views taller than wide (a page);
  `384x576` serves views wider than tall (a spread, a landscape photo). 5.27M parameters, fp16
  weights (lege-gpu widens them at load), 10.58 MB each. The fp32 twins exist only for
  cross-checks in ONNX Runtime and are not shipped.
- **Input:** float32 NCHW, RGB in [0, 1]; mean and standard deviation are folded into the first
  convolution. The view is resized with an antialiasing triangle filter
  (`dewarp/imgops.rs::resize_antialias_chw`); a plain bilinear shrink of a 24 MP photo aliases.
- **Outputs:** `flow` [1,2,H/8,W/8], the correction to the identity map (x then y, in view values
  where -1 and +1 are the view's edges; a backward map: it says where to read); `maps`
  [1,2,H/8,W/8], page mask and gutter heat as probabilities; `scores` [1,2], `bend` (the page is
  not flat) and `spread` (two pages).
- **Training:** on Colab, 2026-11-12 to 11-14; checkpoint ft4 (step 20,834, EMA weights,
  1.5M views). Log and export script: `google-colab-work/dewarp` (`NOTES.md`,
  `train/export_lege.py`). The mix includes Book3D (tagged MIT on Hugging Face; its authors'
  repository words it as for non-commercial research), kept in its own shard sets. The owner's
  ruling, 2026-11-14: not an issue, Lege is a free non-commercial program.
- **Runtime:** `lege_gpu::vision::DewarpNet` keeps both graphs compiled; 13 ms per call warm on
  an RTX 4060 Laptop (Vulkan). There is no practical CPU path (fewer than three calls in five
  minutes on lege-gpu's CPU executor), so without a hardware GPU the step switches itself off
  and pages pass through. Embedded behind the `dewarp` feature (default); runtime assets
  `lege-dewarp-576x384.onnx` and `lege-dewarp-384x576.onnx` override the embedded files.
- **The passes** (`lege-process/dewarp/mod.rs`, a port of the standalone tester): whole frame;
  the gate that leaves flat pages untouched; the page's own crop; for a spread, a cut at the
  gutter and one pass per page; three refinement rounds; a polynomial surface through each
  page's grid; one bilinear render from the original photo. Up to ten model calls for a spread,
  one for a flat page.

## Measured (standalone tester, 2026-11-14)

Distortion left after flattening, in % of the page diagonal, by registering a flat scan of the
same page to the output (lower is better):

| Test | Paddle UVDoc | DocScanner-L | flattener | Lege-Dewarp |
|---|---|---|---|---|
| Cookbook with pictures, spreads not trained on | 0.53 | 0.43 | 0.43 | 0.25 |
| A text book never trained on | 0.51 | 0.45 | 0.45 | 0.25 |

- **Flat pages:** 6 of 6 test pages and 406 of 408 held-out scans are returned untouched; none
  of 590 curved photos is. The other three models alter every flat page.
- **Hand-held photos of spreads (26):** level with Paddle UVDoc on text-line straightness, not
  better. The clear gains are on books photographed lying on a surface.
- **Time per photo after decoding:** 0.26 s for a 10 MP spread, 0.69 s for a 24 MP spread (two
  pages out); 0.02 to 0.03 s for a flat page.

## Known limits

- **Flat pages that are photographs.** A full-page photograph has no text lines or page edges
  to judge flatness by. Of 400 flat scan pages of an illustrated cookbook the model altered 47, a
  few of them badly; text scans are safe (263 of 264 untouched on two other books). This is why
  Flatten page is off by default.
- **Left pages of a spread** keep more distortion than right pages (0.36 against 0.16 on the
  unseen text book).
- **One resolution.** Trained and tested at 576 x 384 only; a 768 x 512 export of the same
  weights is worse.
- **Thick books:** the page edge can include the edge of the page block.
- **PDF input** is flattened from pages rendered at 2400 px or more, through the image pipeline:
  the PDF's outline and text layer do not carry over, and EPUB and reflow jobs ignore the option.
