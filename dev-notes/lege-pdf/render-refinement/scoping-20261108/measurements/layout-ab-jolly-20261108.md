# Layout-detection A/B for the B1 branch (jolly-ride), 2026-11-08

Gate required by the scoping plan before merging `claude/jolly-ride-cyjbsm`
(B1: decode-level-keyed image cache plus a per-page decode reference shared by
the analysis, base and OCR renders). The branch changes the analysis raster, so
the question was whether layout detection changes with it.

Method: two `lege.exe` debug-fast builds, before (5763a86b, dazzling+gifted)
and after (27debaee, plus jolly), run with `LEGE_BBOX_TRACE=1 --no-ocr` on the
private corpus at `D:\pdf-test`, GPU inference (RTX 4060 Laptop, Vulkan). The
`planned raw … bbox=… conf=` detection lines, `img_det`, `[layout]`, overlay
placement, FULL-PAGE and MRC lines were compared order-independently (the
pipeline is parallel, so raw line order differs run to run).

| Document | Pages | Detection/placement lines | Result |
|---|---|---|---|
| beans | all | 3 | identical |
| viaduct | all (3) | 2 image detections | boxes move ≤ 0.5 px, confidence 0.85→0.83 and 0.86→0.84, same class |
| vogtle | all (22) | 42 | identical |
| xenosystems | 1–20 | 94 (74 boxes) | identical |
| sahib (300-dpi scan) | 1–10 | 10 | identical |
| Structures | 1–12 | 54 (31 boxes) | identical |

viaduct is the one document whose analysis render changes (DeviceRGB JPEG pages
now rendered from the decode level shared with the base render). The shift is
sub-pixel and does not change any region decision. Gate passed; jolly merged as
27debaee.

Raw traces of the run are not committed (private documents).
