# Proposed AKR review — 2026-10-27 (updated 2026-10-28)

Updated through 2026-10-28 against the live ledger and working tree. Since the initial 2026-10-27 snapshot:
- **Completed (4 work items + 1 milestone):** `@lege-ecosystem.work.document-ocr-cli-native-text-backend-init/1` (lazy OCR backend init + checkpoint provider validation) and `@lege-ecosystem.milestone.business-pdf-ocr-cli/2` (5/5 checks satisfied); `@lege-ecosystem.work.jp2lam-ssim-controller/1`, `@lege-ecosystem.work.layout-vertical-bands-ocr-horizontal/1`, and `@lege-ecosystem.work.line-art-variance-postprocess/1` (completed on source-inspection evidence).
- **Superseded / abandoned (4 work items):** `@lege-ecosystem-perf.jp2.decode-optimization-handoff-2026-08-09/1` (folded into `@lege-ecosystem.work.jp2lam-jpeg-2000-decoder-optimization-plan/1`); `@lege-ecosystem.work.handoff-remaining-jp2lam-jpeg-2000-decode-gaps/1` and `@lege-ecosystem.work.zero-length-code-block-contribution-truncated/1` (abandoned as obsolete diagnoses); `@lege-ecosystem.work.viewer-movement-and-reliability/1` (abandoned in this repo; code lives in sibling `liminaldoc`).
- **Substantially advanced (GUI & sub-items):** `@lege-ecosystem.work.pixelkit-gui-1-5-2/5` is **active** with a runnable PixelKit GUI in `lege-process/GUI/PixelKit` (compact layout, Preserve mode, themes, locales, worker supervisor, 55 unit tests, -32.9% idle RSS vs Freya; 3/9 acceptance gates closed, remaining gates before cutting `cargo gui` over from Freya). `@lege-ecosystem.work.preservation-mode/1` is **active** with CLI + PixelKit GUI Preserve mode working; DjVu visual verification, 3-book measurements, byte-identity gate, and deferred fidelity remain. Three of five items in `pipeline-memory-follow-ups` (JP2 header dimension sniffing, moving `PdfWriterActor` onto a dedicated blocking thread, and streaming spooled hOCR EPUB sidecar packaging), two of six checks in `renderer-efficiency-deferrals` (`Matrix::transform_rect` and `CalRgbTransform` white-point adaptation matrix caching), and `JP2LAM_DECODE_TRACE` in `pdf-renderer-follow-ups-outside-pure-codec` also landed with test evidence.

Consult the generated [root outstanding-work index](generated/OUTSTANDING-WORK.md) and [PDF integrity outstanding-work index](../lege-pdf/pdf-integrity/docs/generated/OUTSTANDING-WORK.md) for live generated views, plus [active work](generated/ACTIVE-WORK.md) and [roadmap](generated/ROADMAP.md).

## Scope and current ledger summary

Originally reviewed every proposed head returned by AKR search in the root ledger and the separate `lege-pdf/pdf-integrity` ledger at `b4fb52b5`, then updated on 2026-10-28 as items were completed, dispositioned, or advanced:

- Root ledger live planning heads: **3 active work** (`pixelkit-gui-1-5-2/5`, `preservation-mode/1`, `thorough-ppocrv6-performance-and-output-preflight/3`) and **8 proposed work** (down from 18). `business-pdf-ocr-cli/2` is now **completed**.
- Root normative proposed heads: **36** (18 decision, 12 policy, 6 requirement).
- PDF-integrity ledger: **20 proposed heads** = 6 planning (1 milestone, 1 track, 4 work) and 14 design/requirement/term records.

## Root planning entries — all 18 (with 2026-10-28 status)

### Handoff: remaining JP2 decode optimization after renderer MRC pass

`@lege-ecosystem-perf.jp2.decode-optimization-handoff-2026-08-09/1` — **Superseded by `@lege-ecosystem.work.jp2lam-jpeg-2000-decoder-optimization-plan/1` (2026-10-28)**

Unique constraints (renderer `Budgeted(2..8)` concurrency, correctness gates, `Structures.pdf` page-0 baseline, and the negative throughput result on direct-Arc output) were copied into the main decoder optimization plan's `note` before superseding.

Source: `lege-codecs/jp2lam/src/decode/mod.rs:421`.

### Rework jp2lam lossy rate control for perceptual, resolution-stable photographic quality

`@lege-ecosystem.encode-quality.improve-photographic-rate-control/1` — **Proposed; partly implemented; stale umbrella**

Perceptually weighted allocation and verified SSIMULACRA2 quality exist. The proposal's controller dependency (`jp2lam-ssim-controller`) is completed. Remaining: current closure measurement for the broad quality-per-byte goal against other codecs.

Source: `lege-codecs/jp2lam/src/model.rs:1028`.

### Decide whether a native-text-only OCR job may run without an available OCR backend

`@lege-ecosystem.work.document-ocr-cli-native-text-backend-init/1` — **Completed (2026-10-28)**

Implemented option (b): `DocumentProcessor::new` now initializes the OCR backend lazily via `OnceLock<Result<LiveBackend, BackendInitFailure>>` (still eagerly rejecting the unimplemented `WindowsAi`). `configuration_hash()` hashes the requested configuration so resume identity does not depend on whether OCR ran; resumed OCR-derived page checkpoints verify their text regions' `provenance.provider` against the live backend; and manifest OCR model/warning metadata is emitted only when a document has OCR-derived pages. `cargo test -p lege-document-ocr-cli` (6 passed) and `cargo test -p lege-document-pipeline` (26 passed) pass on Linux. This also completed `@lege-ecosystem.milestone.business-pdf-ocr-cli/2`.

Source: `lege-document-ocr/crates/pipeline/src/lib.rs:128`.

### Improve glyph-font quality and long-book growth (formerly: Emit printed text as an embedded per-document glyph font)

`@lege-ecosystem.work.glyph-font-text-output/1` — **Proposed; feature implemented, narrowed to quality/performance residue**

TrueTyping fonts, outline fitting, OCR ToUnicode, orientation, font banking and pipeline output are shipped. The record has been narrowed to the concrete residuals: remaining small-bold character substitutions and excessive font/glyph growth on long books.

Source: `lege-process/encoding/glyphfont.rs:2778`.

### Handoff: remaining `jp2lam` JPEG 2000 decode gaps

`@lege-ecosystem.work.handoff-remaining-jp2lam-jpeg-2000-decode-gaps/1` — **Abandoned (obsolete handoff)**

All four named correctness classes were resolved and recorded in the main decoder optimization plan.

Source: `@lege-ecosystem.work.jp2lam-jpeg-2000-decoder-optimization-plan/1`.

### jp2lam decoder optimisation: the remaining tail

`@lege-ecosystem.work.jp2lam-jpeg-2000-decoder-optimization-plan/1` — **Proposed; open tail (absorbed 2026-08-09 handoff)**

Single live record for remaining `jp2lam` decoder work. Open tail: (1) true windowed/line-based ROI IDWT with halo (profiling-gated), (2) Tier-2 dense block indexing in place of `HashMap<(usize, usize), MergedBlock>` and progression-order iteration without eager packet sorting, (3) Tier-1 style specialization, compact `FlagGrid` words, and stripe-major coefficient layout, (4) profiling-gated AVX2/PGO, (5) `Rgbx8`/`Bgra8` plumbing in `jpx.rs`, (6) direct sYCC 4:2:2 and 4:4:4 kernels, and (7) tile-part `QCC` support.

Source: `lege-codecs/jp2lam/src/decode/t2.rs:268`.

### jp2lam SSIMULACRA2 score-floor quality controller

`@lege-ecosystem.work.jp2lam-ssim-controller/1` — **Completed**

Perceptual targets, outer quantizer/inner PCRD rate search, floor checking, effort budgets, verified `Quality` and legacy `ApproxQuality` APIs, and display scoring are implemented and marked completed on source-inspection evidence.

Source: `lege-codecs/jp2lam/src/encode/backend/native/backend.rs:1022`.

### Correct DocLayout underdetection with vertical bands

`@lege-ecosystem.work.layout-vertical-bands-ocr-horizontal/1` — **Completed**

`layout_correct` implements horizontal growth, OCR gap boxes, image growth, line-art demotion and optional inverted redetection, hooked from `LayoutEngine::detect_single_blocking`.

Source: `lege-process/core/layout_correct.rs:47`.

### Skip line-art image detections via bpg-rs variance classifier

`@lege-ecosystem.work.line-art-variance-postprocess/1` — **Completed**

Variance/Sobel/orientation classifier in `content_class.rs` is live; PDF and DjVu pipelines call `should_keep_image_overlay` and `layout_correct` demotes line-art image detections.

Source: `lege-process/core/content_class.rs:94`.

### Relaunch Lege as the canonical open-source codec monorepo

`@lege-ecosystem.work.open-source-monorepo-relaunch/1` — **Proposed; partly implemented (0/6 checks)**

Direct DJVULibRust integration and the top-level AGPL decision are landed. Remaining: consistent license declarations across crates, sanitized history/tree and public cutover, clean-clone verification, and checksum-pinned optional OCR model bootstrap.

Source: `to-do-plans/open-source-monorepo-relaunch.md`.

### PagePress followup: PDF page import via lege-pdf-read

`@lege-ecosystem.work.pagepress-pdf-import-followup/1` — **Proposed; partly implemented**

Sibling PagePress already imports and merges existing PDFs through an isolated temporary `lopdf` backend. Replacing or augmenting that backend with `lege-pdf-read` remains open; OCR layers and automatic deskew are separately scoped.

Source: `/home/dk/rust-projects/pagepress/src/pdf_import.rs`.

### PDF renderer follow-ups (outside pure codec)

`@lege-ecosystem.work.pdf-renderer-follow-ups-outside-pure-codec/1` — **Proposed; 2 of 4 items done (2026-10-28)**

- **Done:** (1) Session-based packed JP2 decoding (`Jp2Decoder` + direct packed output), and (3) optional one-line stderr decode telemetry behind `JP2LAM_DECODE_TRACE=1` in `lege-pdf/render/crates/pdf-image/src/jpx.rs`.
- **Remaining:** (2) Four-byte `Bgra8`/`Rgbx8` renderer format plumbing when GPU/BGRA surface compositing wants 4-byte pixels, and (4) a fresh `pdfium-diff` regression run on a JPX-heavy page.

Source: `lege-pdf/render/crates/pdf-image/src/jpx.rs:104`, `lege-pdf/render/crates/pdf-image/src/jpx.rs:340`.

### Deferred pipeline memory follow-ups

`@lege-ecosystem.work.pipeline-memory-follow-ups/1` — **Proposed; 3 of 5 items landed (2026-10-28)**

- **Done (2026-10-28):**
  - Native dimension sniffing for JPEG 2000 inputs (`jp2_header_dimensions` in `lege-process/pipeline/source.rs` parses the JP2 `ihdr` box or bare/`jp2c` codestream `SIZ` marker from a 64 KiB prefix for both `_jp2.zip` and folder sources).
  - Moved `PdfWriterActor` blocking I/O off the current-thread control executor (`spawn_pdf_writer_actor` in `lege-process/pipeline/helper_functions.rs` now runs on a dedicated blocking worker via `runtime_stats::spawn_blocking` + `rx.blocking_recv()`).
  - Streamed spooled hOCR EPUB sidecar packaging (`build_epub_from_spooled_hocr_pages_with_outline_cancellable` in `lege-process/pipeline/epub_pipeline.rs` reads and parses one `.hocr` file at a time into `PageText` instead of loading all raw hOCR strings into memory at once).
- **Remaining:** Extend byte admission/reservations through DjVu queues; measure peak RSS on the decisive workloads.

Source: `lege-process/pipeline/source.rs:889`, `lege-process/pipeline/helper_functions.rs:1331`, `lege-process/pipeline/epub_pipeline.rs:460`.

### Replace the Freya processor GUI with PixelKit in 1.5.2

`@lege-ecosystem.work.pixelkit-gui-1-5-2/5` — **Active; runnable GUI largely implemented (3/9 checks satisfied)**

- **Done:** Standalone PixelKit GUI lives in `lege-process/GUI/PixelKit` (`lege-gui-pixelkit`, `cargo gui-pixelkit` / `cargo gui-pixelkit-run`) with no Freya/Skia in its graph. Implements the compact layout (equal sibling mode buttons including Reflow, Preserve mode as a first-class mode with conflicting prep flags removed, compact independent parameter fields, 280px settings band recovering 110px for the queue, primary Process button separated from About/Queue/Log), Sanzo dark theme persistence (`#theme` satisfied), English/German `language_service` chrome (`#languages` satisfied), idle memory benchmark (`#memory` satisfied: 31.90 MiB RSS vs 47.57 MiB Freya, -32.94%), and real worker supervision. 55 unit tests pass.
- **Remaining before replacing Freya as default `cargo gui` (`#cpu-presenter`, `#controls`, `#tooltips`, `#docs`, `#progress`, `#visual-resize`):** cut `cargo gui` over to produce `lege-gui` on the PixelKit CPU presenter, and record manual acceptance evidence for control reachability/mutual exclusions, status-panel hover tooltips, embedded HTML Readme/Licenses browser launch, multi-page progress/ETA/Cancel verification, and resize/visual parity across window sizes (plus remeasuring memory on processing workloads).

Source: `lege-process/GUI/PixelKit/`, `docs/PIXELKIT-GUI-PREVIEW.md`.

### Preservation mode: an MRC document beside the e-ink preparation mode

`@lege-ecosystem.work.preservation-mode/1` — **Active; CLI and PixelKit GUI implemented, verification/fidelity tail open**

- **Done:** CLI `--preserve`, color PDF MRC (JP2 background + JBIG2 mask) and color DjVu layers (IW44 + JB2), plus the PixelKit GUI mode toggle and worker plumbing (verified end-to-end on a 2-page color PDF with JPX backgrounds, JBIG2 masks, and cancellation smoke).
- **Remaining:** DjVu decode-and-look verification, three-book size/time measurements, the pre-refactor grayscale byte-identity gate, making PixelKit the default `cargo gui`, and triggered/deferred fidelity items (colored ink, color source-MRC passthrough).

Source: `dev-notes/lege-process/PRESERVATION-MODE-PLAN.md:255`, `docs/PIXELKIT-GUI-PREVIEW.md`.

### Disposition the deferred renderer efficiency findings

`@lege-ecosystem.work.renderer-efficiency-deferrals/1` — **Proposed; 2/6 checks satisfied (2026-10-28)**

- **Done (2026-10-28):**
  - `#transform-rect-shared-helper` — `Matrix::transform_rect` landed in `pdf-geom` and replaced the duplicate implementations in `pdf-text` and `pdf-content::lower`.
  - `#calrgb-adaptation-matrix-cache` — `CalRgbTransform` landed in `pdf-color` (precomputing the `/WhitePoint` adaptation matrix `M⁻¹` and sanitised `/Gamma` once per colour space) and is cached directly in `pdf-content`'s `CieSpace::CalRgb`.
- **Remaining (4 investigations/decisions):** `#ccitt-bulk-skip`, `#cmyk-source-conversion-cache`, `#lexer-name-bulk-scan`, and `#postprocess-row-parallelism`.

Source: `lege-pdf/render/crates/pdf-geom/src/lib.rs:120`, `lege-pdf/render/crates/pdf-color/src/lib.rs:155`.

### Complete lege-viewer stage 6 movement and start stage 7 reliability

`@lege-ecosystem.work.viewer-movement-and-reliability/1` — **Abandoned in this repo (moved to `liminaldoc`)**

`b4fb52b5` deleted the migration copy; the reader lives in sibling `liminaldoc/crates/liminal-reader` where movement, password flow and page failure handling landed. Anchor-level trace replay remains tracked there.

Source: `/home/dk/rust-projects/liminaldoc/crates/liminal-reader/STAGES.md:304`.

### 3. Zero-length code-block contribution (truncated streams) — ~6 files

`@lege-ecosystem.work.zero-length-code-block-contribution-truncated/1` — **Abandoned (obsolete diagnosis)**

Named repros were packet-header `inalign` desynchronization (already fixed), not truncated streams; genuine truncation remains strict by design.

Source: `@lege-ecosystem.requirement.update-sections-3-and-4-resolved-the-4/1`.

## Root proposed design records — all 36

These are not 36 additional implementation tasks. Decisions, requirements and policies describe intended or already-implemented behavior and do not use a work item's completion gate. Their proposed state still merits adoption/reconciliation. Imported historical headings especially overstate the pending backlog.

Important reconciliations: the old precinct/SOP/EPH, degenerate-DWT, zero-byte contribution and TPsot diagnoses are superseded in substance by the July 22 update and the consolidated decoder plan. The old per-row scratch observation predates coarse-chunk scratch. Final Arc ownership, masked MRC draws and Gray8 fast paths are recorded implementations. The verified-quality-scale decision replaces the earlier statement that Quality remains the legacy ladder. The later generic JBIG2 default qualifies the earlier 'JBIG2 symbol mode again' description. Prepress remains explicitly deferred scope, not a scheduled work item.

| Kind | Reference | Title |
|---|---|---|
| policy | `@lege-ecosystem-perf.policy.gpu-path-useful-but-only-with-renderer/1` | 13. GPU path: useful, but only with renderer integration |
| policy | `@lege-ecosystem-perf.policy.parallelism-policy-for-a-pdf-renderer/1` | 11. Parallelism policy for a PDF renderer |
| policy | `@lege-ecosystem-perf.policy.phase-5-true-windowed-line-based-decode/1` | Phase 5 — true windowed/line-based decode |
| policy | `@lege-ecosystem-perf.policy.recommended-renderer-api-and-policy/1` | 16. Recommended renderer API and policy |
| policy | `@lege-ecosystem-perf.policy.scratch-allocation-per-parallel-row/1` | 5.1 Scratch allocation per parallel row |
| decision | `@lege-ecosystem-perf.renderer.final-buffer-ownership/1` | Allocate renderer and JPEG final buffers in their shared Arc layout |
| decision | `@lege-ecosystem-perf.renderer.gray8-draws-take-the-rgb8-fast-paths/1` | Gray8 image draws take the axis-aligned RGB8 fast paths through a one-time promotion; non-JPX decodes are cached without a target size |
| decision | `@lege-ecosystem-perf.renderer.mrc-masked-draw-fast-path/1` | MRC pages are shading-bound, not decode-bound: masked area-min fast path in pdf-render-cpu |
| requirement | `@lege-ecosystem-perf.requirement.what-large-render-means-for-this-problem/1` | 1. What “large render” means for this problem |
| decision | `@lege-ecosystem.decision.a-fold-may-not-change-a-letters-counters/1` | Truetyping never folds shapes that enclose different counters, and a stroke is far even when it is a hairline |
| decision | `@lege-ecosystem.decision.jbig2-is-the-default-pdf-text-format-again/1` | JBIG2 is the Freya GUI's default PDF text encoder again; truetyping is the opt-in checkbox |
| decision | `@lege-ecosystem.decision.jbig2-symbol-substitution-is-opt-in/1` | JBIG2 writes the bit-exact generic region unless symbol mode is asked for |
| decision | `@lege-ecosystem.decision.jp2-region-floor-splits-by-region-size/1` | The verified JP2 display floor for an ordinary figure region is 65 when it covers under a quarter of the page and 70 otherwise |
| decision | `@lege-ecosystem.decision.jp2lam-ssim-canonical-metric/1` | jp2lam encode-time quality is shared JPXL SSIMULACRA2 |
| decision | `@lege-ecosystem.decision.jp2lam-verified-quality-scale/1` | jp2lam quality number is the verified SSIMULACRA2 scale; legacy ladder is ApproxQuality |
| decision | `@lege-ecosystem.decision.nup-grid-is-read-in-the-sheets-own-frame/1` | The N-up cell grid is read in the sheet's own frame, not transposed for a landscape sheet |
| decision | `@lege-ecosystem.decision.pagepress-boundary-rasterizer-vs-editor/1` | PagePress boundary: rasterizer stays in lege-process, editor suite starts in PagePress |
| decision | `@lege-ecosystem.decision.pipeline-memory-policy-from-external-review/1` | Pipeline memory policy adopted from external review |
| decision | `@lege-ecosystem.decision.printing-defers-prepress-to-lege-pdf-write/1` | Office printing is built; prepress is deferred, and belongs in lege-pdf/write rather than the rasterizer |
| decision | `@lege-ecosystem.decision.sideways-pages-are-turned-upright-with-rotate/1` | A sideways page is turned upright with /Rotate, never on its head |
| decision | `@lege-ecosystem.decision.simple-cli-default-binarization-is-adaptive/1` | The simple CLI mode binarizes adaptively by default, not with a fixed 180 threshold |
| decision | `@lege-ecosystem.decision.truetyping-banks-fonts-and-respects-a-bilevel-source/1` | A document may carry several glyph fonts, and a book already black and white is traced at its own resolution |
| decision | `@lege-ecosystem.decision.truetyping-leaves-bilevel-sources-alone/1` | Truetyping is skipped when the source's ink is already one bit; source masks pass through |
| decision | `@lege-ecosystem.decision.truetyping-renders-in-grayscale-by-default/1` | Truetyping renders its ink mask through the MRC clean path |
| policy | `@lege-ecosystem.policy.implementation-update/1` | 2026-07-19 implementation update |
| policy | `@lege-ecosystem.policy.later-update-section-2-resolved-and-sub-8-bit/1` | 2026-07-22 (later) update — Section 2 RESOLVED, and sub-8-bit precision added |
| policy | `@lege-ecosystem.policy.renderer-integration-policy/1` | 28. Renderer integration policy |
| policy | `@lege-ecosystem.policy.replace-eager-packet-position-sorting-where/1` | 29. Replace eager packet-position sorting where worthwhile |
| policy | `@lege-ecosystem.policy.reuse-per-packet-contribution-storage/1` | 31. Reuse per-packet contribution storage |
| policy | `@lege-ecosystem.policy.the-census-extraction-tool-your-main-instrument/1` | The census / extraction tool (your main instrument) |
| policy | `@lege-ecosystem.policy.tpsot-tnsot-non-conformant-tile-parts-2-files/1` | 4. `TPsot==TNsot` non-conformant tile-parts — ~2 files  [RESOLVED 2026-07-22 — the assembly-bug diagnosis in this section is WRONG; real cause was a missing `inalign` byte-realignment. See the 2026-07-22 update near the top.] |
| requirement | `@lege-ecosystem.requirement.degenerate-1-sample-dwt-split-8-files-resolved/1` | 2. Degenerate 1-sample DWT split — ~8 files  [RESOLVED 2026-07-22 — root cause was a K/INV_K scaling on a one-row resolution in the *vertical* pass; see the update near the top. The "relaxing the bound drifts" note below is accurate but the cause is now fixed.] |
| requirement | `@lege-ecosystem.requirement.orientation-environment/1` | 0. Orientation & environment |
| requirement | `@lege-ecosystem.requirement.precincts-sop-eph-10-files-largest-remaining/1` | 1. Precincts / SOP / EPH — ~10 files (LARGEST remaining class) |
| requirement | `@lege-ecosystem.requirement.reduced-reconstruction-geometry/1` | 8. Reduced reconstruction geometry |
| requirement | `@lege-ecosystem.requirement.update-sections-3-and-4-resolved-the-4/1` | 2026-07-22 update — Sections 3 and 4 RESOLVED (the §4 diagnosis below was WRONG) |

## Active root work outside the original proposed filter

- `@lege-ecosystem.milestone.business-pdf-ocr-cli/2` — **Completed (2026-10-28, 5/5 checks satisfied)**: the remaining `#core-cli` check was satisfied once `@lege-ecosystem.work.document-ocr-cli-native-text-backend-init/1` landed lazy OCR backend initialization.
- `@lege-ecosystem.work.thorough-ppocrv6-performance-and-output-preflight/3` — **Active (0/5 checks satisfied)**: implementation and verification existed in August; reopened because a scope revision invalidated all five older acceptance-evidence links. Needs refreshed evidence and formal re-completion, not a new OCR optimization.

## PDF-integrity planning entries — all 6

### M5 — consistency heuristics, validated against a legitimate corpus

`@audit.milestone.m5-consistency-heuristics/1` — Open and corpus-gated. Neither false-positive nor seeded-edit acceptance has evidence; corpus-dependent work is deferred by the operator timing decision.

### Fixture and legitimate-producer corpus

`@audit.track.fixture-corpus/1` — Ongoing. Collect provenance-labeled legitimate producer outputs and controlled edit fixtures; the threshold is at least 200 documents per family across at least eight families.

### Two-file compare mode (suspect vs trusted original)

`@audit.work.compare-command/1` — Open/speculative. Current CLI exposes inspection, not a two-file compare command; reuse M3 for input pairing and page correspondence.

### Heuristic: font-resource fragmentation within a single text run

`@audit.work.intra-run-font-fragmentation/1` — Open/speculative. Needs implementation and corpus validation; no standalone suspicion claim is justified by the proposed signal.

### M5 plan of record

`@audit.work.m5-plan/1` — Partly started. Uncommitted corpus.rs and corpus-sweep.rs implement the collection/measurement harness; this does not complete corpus collection, heuristics or seeded-edit validation.

### OCR cross-check layer (lege-ocr)

`@audit.work.ocr-crosscheck/1` — Open/speculative. No OCR module/dependency is present in pdf-forensics; accuracy assessment and feature-gated integration are outstanding.

## PDF-integrity proposed design records — all 14

These govern M1–M5 and future work; they are not fourteen more unfinished features. M1–M4 are completed in the ledger. The M5 corpus gate is genuine remaining work. The crate-home decision refines the earlier crate-shape proposal, so the shipped standalone audit CLI should not be mistaken for a missing pdf-cli subcommand.

| Kind | Reference | Title |
|---|---|---|
| decision | `@audit.decision.crate-shape/1` | Facts in pdf-read, interpretation in a new pdf-forensics crate |
| decision | `@audit.decision.crypto-crate-choice/1` | M4 crypto layer uses the RustCrypto cms/x509-cert stack |
| decision | `@audit.decision.encrypted-input-handling/1` | Encrypted PDFs get a structure-only audit, upgraded by an optional password |
| decision | `@audit.decision.forensics-repo-home/1` | pdf-forensics lives in the pdf-integrity repo, path-depending on lege-pdf |
| decision | `@audit.decision.heuristic-corpus-size/1` | M5 heuristics need 200 labeled docs per family over 8 families |
| decision | `@audit.decision.product-framing/1` | Triage tool, not a fraud detector |
| decision | `@audit.decision.revision-model/1` | Xref sections and logical revisions are separate types |
| decision | `@audit.decision.signature-layering/1` | Structural signature coverage first, cryptography second, PKI via a mature crate |
| decision | `@audit.decision.strict-and-tolerant-parse/1` | Dual parse assessment instead of a second strict parser |
| requirement | `@audit.req.deterministic-evidence/1` | Evidence renders are deterministic |
| requirement | `@audit.req.explainable-findings/1` | Every finding is categorical, explained, and bounded |
| requirement | `@audit.req.read-only-analysis/1` | The tool never modifies its input |
| requirement | `@audit.req.repairs-are-visible/1` | Structural repairs never silently become facts |
| term | `@audit.term.logical-revision/1` | Logical revision |

## Validation status (2026-10-28)

- Root `akr check`: **exits 0** (0 errors). Remaining warnings are `AKR-G004` (uncommitted changes on watched paths) and `AKR-G023` (uncommitted plan files and the removed `lege-viewer/**` scope on `top-level-application-license-agpl/1`). Views in `docs/generated/` and `.akr/akr.lock` are up to date via `akr build`.
- Nested `akr --dir lege-pdf/pdf-integrity check`: retains its five inherited historical papercut errors (`AKR-G011`, `AKR-R052`).

