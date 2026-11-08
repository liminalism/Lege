# PDF renderer comparison, 2026-11-06

Lege at `13109aed` against six other renderers on the eight-document private corpus in `D:/pdf-test` (1,849 pages). Machine: Intel i7-13700H (6P+8E, 20 threads), 32 GB, Windows 11, idle. Raw data and harness: `.agent/scratch/pdf-render-headsup-20261106/` (kept). AKR: `lege-ecosystem-perf.renderer-headsup.official-comparison-20261106`.

## Summary: everything relative to Lege

**Lege speed multiple** = how many times faster Lege renders than that renderer: geometric mean over 8 documents x {72, 144} DPI x {1, 8} workers of the per-cell ratio of median whole-document pages/s. **Agreement** = share of pages on which the renderer agrees with the majority of renderer families (not ground truth).

| Renderer | Lege speed multiple, overall | 1 worker | 8 workers | 72 DPI | 144 DPI | per-cell range | timed repetitions | agrees 72 DPI | agrees 144 DPI | agrees, mean | agreement vs Lege (pts) |
|---|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|
| **Lege (lege-pdf renderer)** | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | — | 3 | 79.3% | 100.0% | 89.6% | — |
| PDFium 158.0.8125 (Blanchon chromium/8125) | **1.40x** | 1.43x | 1.37x | 1.36x | 1.43x | 0.65-2.39x | 3 | 93.2% | 100.0% | 96.6% | +7.0 |
| MuPDF 1.28.6 (PyMuPDF 2.1) | **2.26x** | 2.25x | 2.27x | 2.23x | 2.29x | 0.86-12.34x | 3 | 95.9% | 99.9% | 97.9% | +8.3 |
| Hayro 0.9.0 | **1.36x** | 1.39x | 1.34x | 1.36x | 1.37x | 0.95-1.99x | 3 | 96.4% | 100.0% | 98.2% | +8.6 |
| zpdf 0.18.0 (CPU / tiny-skia) | **4.49x** | 4.86x | 4.15x | 4.52x | 4.46x | 1.80-17.21x | 1 | 99.2% | 99.5% | 99.4% | +9.7 |
| pdf-render 1.3.0 (PDFluent) | **4.54x** | 4.87x | 4.22x | 4.16x | 4.94x | 1.67-21.34x | 1 | 80.9% | 99.9% | 90.4% | +0.8 |
| pdf-toolkit 0.3.13 | **5.36x** | 5.85x | 4.92x | 5.40x | 5.32x | 1.42-25.33x | 1 | 44.7% | 71.7% | 58.2% | -31.4 |

A multiple above 1 means Lege is faster. Per-cell range is the slowest and fastest single (document, DPI, workers) cell.

## 144 DPI, 1 worker: median pages/s (Lege multiple in parentheses)

| Document | Lege | PDFium | MuPDF | Hayro | zpdf | pdf-render | pdf-toolkit |
|---|---:|---:|---:|---:|---:|---:|---:|
| Structures | **28.5** | 13.1 (2.18x) | 4.6 (6.20x) | 16.4 (1.74x) | 3.0 (9.65x) | 5.1 (5.64x) | 1.5 (18.90x) |
| beans | **124.5** | 158.7 (0.78x) | 84.1 (1.48x) | 74.0 (1.68x) | 25.5 (4.89x) | 5.8 (21.34x) | 10.4 (11.98x) |
| centralparktest-repair | **23.1** | 11.5 (2.00x) | 11.4 (2.02x) | 16.9 (1.36x) | 5.8 (3.96x) | 5.5 (4.22x) | 11.4 (2.02x) |
| chinese-medicine | **10.2** | 6.1 (1.66x) | 1.7 (6.07x) | 6.7 (1.51x) | 1.2 (8.84x) | 2.2 (4.62x) | 0.9 (10.85x) |
| sahib | **22.6** | 13.5 (1.67x) | 6.3 (3.56x) | 16.6 (1.36x) | 3.9 (5.77x) | 2.7 (8.22x) | 3.9 (5.75x) |
| viaduct | **47.8** | 41.2 (1.16x) | 43.6 (1.10x) | 45.6 (1.05x) | 23.3 (2.05x) | 18.6 (2.57x) | 29.5 (1.62x) |
| vogtle | **203.0** | 180.5 (1.12x) | 149.7 (1.36x) | 184.7 (1.10x) | 74.2 (2.74x) | 68.0 (2.99x) | 54.4 (3.73x) |
| xenosystems | **302.7** | 165.2 (1.83x) | 197.4 (1.53x) | 181.5 (1.67x) | 54.0 (5.60x) | 54.0 (5.61x) | 36.4 (8.31x) |

## 72 DPI, 1 worker: median pages/s (Lege multiple in parentheses)

| Document | Lege | PDFium | MuPDF | Hayro | zpdf | pdf-render | pdf-toolkit |
|---|---:|---:|---:|---:|---:|---:|---:|
| Structures | **40.9** | 20.4 (2.00x) | 5.2 (7.81x) | 27.3 (1.50x) | 3.1 (13.38x) | 7.5 (5.46x) | 1.6 (25.33x) |
| beans | **186.2** | 287.0 (0.65x) | 189.8 (0.98x) | 168.2 (1.11x) | 62.3 (2.99x) | 23.0 (8.08x) | 31.4 (5.94x) |
| centralparktest-repair | **38.5** | 16.1 (2.39x) | 20.9 (1.84x) | 20.9 (1.84x) | 6.7 (5.72x) | 6.3 (6.12x) | 15.6 (2.46x) |
| chinese-medicine | **23.0** | 16.3 (1.41x) | 1.9 (12.34x) | 16.8 (1.37x) | 1.4 (15.95x) | 4.5 (5.07x) | 0.9 (25.25x) |
| sahib | **43.8** | 36.4 (1.20x) | 32.6 (1.34x) | 35.2 (1.24x) | 16.9 (2.58x) | 13.0 (3.38x) | 15.1 (2.90x) |
| viaduct | **68.2** | 56.6 (1.21x) | 79.7 (0.86x) | 62.5 (1.09x) | 36.0 (1.90x) | 40.7 (1.67x) | 47.9 (1.42x) |
| vogtle | **278.4** | 240.4 (1.16x) | 211.2 (1.32x) | 255.8 (1.09x) | 104.8 (2.66x) | 107.9 (2.58x) | 89.8 (3.10x) |
| xenosystems | **685.8** | 392.9 (1.75x) | 389.1 (1.76x) | 344.3 (1.99x) | 99.9 (6.86x) | 114.0 (6.01x) | 62.2 (11.02x) |

## 144 DPI, 8 workers: median pages/s (Lege multiple in parentheses)

| Document | Lege | PDFium | MuPDF | Hayro | zpdf | pdf-render | pdf-toolkit |
|---|---:|---:|---:|---:|---:|---:|---:|
| Structures | **117.2** | 61.6 (1.90x) | 22.3 (5.25x) | 65.3 (1.79x) | 12.8 (9.16x) | 21.9 (5.36x) | 7.2 (16.24x) |
| beans | **155.6** | 234.8 (0.66x) | 143.1 (1.09x) | 118.4 (1.31x) | 48.3 (3.22x) | 15.8 (9.87x) | 18.5 (8.40x) |
| centralparktest-repair | **115.0** | 63.8 (1.80x) | 59.5 (1.93x) | 83.7 (1.37x) | 28.5 (4.03x) | 24.3 (4.73x) | 52.5 (2.19x) |
| chinese-medicine | **43.4** | 27.9 (1.56x) | 8.0 (5.45x) | 26.1 (1.66x) | 5.4 (8.06x) | 11.9 (3.65x) | 4.6 (9.51x) |
| sahib | **93.9** | 63.2 (1.49x) | 33.4 (2.81x) | 79.3 (1.18x) | 19.1 (4.91x) | 12.1 (7.74x) | 17.7 (5.29x) |
| viaduct | **76.7** | 58.5 (1.31x) | 73.0 (1.05x) | 74.9 (1.02x) | 33.2 (2.31x) | 34.0 (2.26x) | 48.7 (1.57x) |
| vogtle | **507.8** | 404.2 (1.26x) | 234.1 (2.17x) | 534.4 (0.95x) | 272.4 (1.86x) | 241.4 (2.10x) | 232.9 (2.18x) |
| xenosystems | **1118.9** | 707.6 (1.58x) | 677.6 (1.65x) | 719.7 (1.55x) | 280.7 (3.99x) | 247.3 (4.53x) | 197.6 (5.66x) |

Peak working set at 8 workers, MiB (median): Lege 306 (doc median), 1548 (max); Hayro 216 (doc median), 1090 (max); zpdf 711 (doc median), 2572 (max); pdf-render 511 (doc median), 1251 (max); pdf-toolkit 534 (doc median), 2325 (max). PDFium/MuPDF memory not measured (the venv python.exe launcher hides the interpreter and worker processes from the sampler).

## 72 DPI, 8 workers: median pages/s (Lege multiple in parentheses)

| Document | Lege | PDFium | MuPDF | Hayro | zpdf | pdf-render | pdf-toolkit |
|---|---:|---:|---:|---:|---:|---:|---:|
| Structures | **160.7** | 93.3 (1.72x) | 24.3 (6.62x) | 121.1 (1.33x) | 13.3 (12.12x) | 32.7 (4.91x) | 7.2 (22.32x) |
| beans | **314.8** | 381.2 (0.83x) | 274.0 (1.15x) | 229.7 (1.37x) | 133.2 (2.36x) | 50.1 (6.29x) | 65.4 (4.81x) |
| centralparktest-repair | **165.8** | 79.2 (2.09x) | 85.2 (1.95x) | 97.1 (1.71x) | 27.9 (5.94x) | 19.6 (8.44x) | 53.1 (3.12x) |
| chinese-medicine | **90.9** | 71.1 (1.28x) | 8.5 (10.66x) | 73.5 (1.24x) | 5.3 (17.21x) | 20.0 (4.56x) | 4.1 (21.98x) |
| sahib | **176.9** | 157.3 (1.12x) | 150.2 (1.18x) | 155.9 (1.13x) | 77.1 (2.29x) | 57.3 (3.09x) | 69.7 (2.54x) |
| viaduct | **104.6** | 71.6 (1.46x) | 117.0 (0.89x) | 88.3 (1.18x) | 54.1 (1.93x) | 55.4 (1.89x) | 67.1 (1.56x) |
| vogtle | **673.1** | 515.2 (1.31x) | 301.6 (2.23x) | 596.9 (1.13x) | 374.1 (1.80x) | 313.5 (2.15x) | 340.9 (1.97x) |
| xenosystems | **2204.9** | 1607.5 (1.37x) | 1022.8 (2.16x) | 1207.9 (1.83x) | 542.9 (4.06x) | 483.8 (4.56x) | 315.3 (6.99x) |

Peak working set at 8 workers, MiB (median): Lege 187 (doc median), 508 (max); Hayro 131 (doc median), 526 (max); zpdf 642 (doc median), 2655 (max); pdf-render 393 (doc median), 1098 (max); pdf-toolkit 294 (doc median), 2266 (max). PDFium/MuPDF memory not measured (the venv python.exe launcher hides the interpreter and worker processes from the sampler).

## Accuracy notes

- Lege disagrees on 383 pages at 72 DPI, all scanned/bilevel books (chinese-medicine 242, centralpark 94, Structures 44, vogtle 3): text on minified 1-bit scan layers comes out thinner/broken. Unchanged since 2026-11-04. At 144 DPI Lege agrees on every page.
- zpdf renders 9 near-blank Structures MRC pages (6, 10, 12, 18, 34, 568, 628-630) solid black at both DPIs; its other disagreements are borderline (5-6% gross pixels).
- pdf-toolkit misses image content on many scan pages (e.g. a missing photograph, smeared chinese-medicine pages).
- Agreement is majority agreement, not ground truth: Hayro and pdf-render share one family vote; three engines can agree on the same wrong fallback.

## Method

- **Timing.** One child process per (renderer, document, DPI, workers, repetition), every child pinned to the P-cores (logical CPUs 0-11), engine order rotated per repetition, idle machine. Rep 0 timed all seven renderers; reps 1-2 timed Lege, PDFium, MuPDF and Hayro again. Figures are the median of the repetitions. Repetition spread (max-min over median, per cell): Lege median 13%, max 42%; PDFium median 13%, max 44%; MuPDF median 8%, max 33%; Hayro median 9%, max 37% (largest on sub-second runs). The timed span is page load/interpretation, decoding, rasterisation and materialised native pixels; it excludes file read, document open, process start and PNG encoding.
- **Workers.** 1 worker = serial pages. 8 workers: Lege = its bounded scheduler (2 compile + 6 render threads); Hayro/pdf-render/pdf-toolkit = 8 Rayon page threads sharing one document; zpdf = 8 threads each owning a document (its `PdfDocument` is not `Sync` and it has no document-level parallel API); PDFium/MuPDF = 8 worker processes each owning a document (their APIs are not thread-safe). All documents are opened before the timer.
- **Nested parallelism off** for comparability: `RAYON_NUM_THREADS`=workers, `PDF_RENDER_THREADS=1`, `PDF_RENDERER_JPX_CONCURRENCY=serial`, `PDF_RENDERER_IMAGE_ROW_PAR=0`. These are not each library's best automatic configuration.
- **Builds.** Rust adapters: release, opt-level 3, no target-cpu override. PDFium: verified Blanchon prebuilt DLL. MuPDF: official PyMuPDF wheel.
- **Accuracy.** Every page at 72 and 144 DPI, opaque white, RGB(A)8. A pair disagrees when >5% of pixels differ by >48 in any channel, or ink coverage differs by >1 pt and continuous darkness by >0.3 pt, or one is blank where the other is not; ≤1 px extent differences are white-padded. 144 DPI is box-reduced 2x before comparison. A renderer is `ok` when it agrees with every maximum clique of ≥3 distinct families. Lege/zpdf rasters were rendered 2026-11-06; the other five were rendered 2026-11-04 with the same binaries.
- **Limits.** Eight user-selected documents (scan-heavy) — not a broad, license-cleared corpus. One laptop with hybrid cores; absolute pages/s are machine-specific, the multiples are the portable figure.
