# pdf-renderer

A read-only, memory-bounded, **concurrent** PDF rendering engine in Rust — a
semantic port of PDFium's behavior with a fundamentally different core: an
immutable document snapshot, worker-local execution state, and a
backend-neutral compiled-page IR that both a CPU rasterizer and (later) a WGPU
backend consume.

Design notes live in the repository working-note archive:

- `../../dev-notes/lege-pdf/render-refinement/plans/expanded-rust-pdfrender-plan.md` — the original phased roadmap (CPU phases 0–6 complete, WGPU phases 7–11 open) and the `CompiledPage` rendering contract.
- `../../dev-notes/lege-pdf/render-refinement/plans/skeleton-blueprint.md` — repo-concrete decisions this workspace encodes.
- `../../dev-notes/lege-pdf/render-refinement/CURRENT-STATE.md` — the reconciled state of the engine and the sweep programme.
- `../../dev-notes/lege-pdf/render-refinement/handoffs/DEFERRED.md` — the one-stop map of every deliberate approximation and open item.
- `../../dev-notes/lege-pdf/render-refinement/README.md` — index of the sweep history.
- `../CRATES-IO-PLAN.md` — the plan for publishing this engine as the `lege-pdf` crate.

## The one rule

> Rendering a page must operate on an immutable document snapshot, with all
> execution state and mutable scratch data owned by the worker or page render
> session. No global state, anywhere.

## Layout

```text
crates/
  pdf-geom              dependency-free geometry leaf (Matrix, Point, Rect)
  pdf-source            positional-read byte access (mmap / owned / file)
  pdf-syntax            lexer + primitive object parser, syntax limits
  pdf-object            object model, ObjectId, name interning
  pdf-structure         xref tables + streams, trailers, revisions, object
                        streams, stream filters, xref rebuild
  pdf-security          standard security handler, R2–R6, user + owner passwords
  pdf-document          DocumentSnapshot, page tree + recovery, outlines,
                        destinations, ObjectRepository, ParseContext budgets
  pdf-color             color spaces, ICC matrix/TRC + CMYK Lab-LUT, the
                        frozen DeviceCMYK policy, tint transforms
  pdf-function          PDF functions: sampled, exponential, stitching,
                        PostScript calculator
  pdf-font              encodings, CMaps (predefined CJK tables), Type 1,
                        bare CFF, TrueType/OTF via Skrifa, hinting, the
                        standard 14, system-font substitution
  pdf-image             image resource semantics + native codecs: JPEG,
                        CCITT G3/G4, JBIG2 (jbig2enc-rust), JPEG 2000 (jp2lam),
                        Flate/LZW/RunLength predictors, SMask/Mask/Decode
  pdf-content           content-stream interpreter → SemanticPage → CompiledPage
  pdf-page-ir           CompiledPage IR — THE rendering contract (dependency-free)
  pdf-render-api        RenderBackend trait, RenderRequest, HostPage, limits,
                        cancellation, the catch_unwind boundary
  pdf-render-cpu        reference CPU rasterizer (normative implementation)
  pdf-render-scheduler  compile pool → render pool, memory permits, reordering
  pdf-postprocess       postprocess graph + CPU executor (crop/resize/gray/
                        tone/threshold/dither/1-bit pack); experimental GPU
                        executor behind the `gpu` feature
  pdf-render-wgpu       experimental decoded-RGB image renderer + CPU fallback
                        (depends on lege-gpu; not part of the published crate)
  pdf-text              UTF-16 text, character geometry, rectangles, words,
                        bidi + normalization from vendored PDFium tables
  pdf-read              document doctor (`pdfr doctor` structural triage)
  pdf-profiling         opt-in stage timers (`profiling` feature)
  pdf-test-support      diff metrics, determinism harness, corpus access
  pdf-chaos-tests       stable-toolchain never-panic mutation gate
  pdf-cli               driver binary `pdfr` (info / doctor / dump / text /
                        render / attribute) and the `lege-render-demo` throughput
                        demonstrator (`--features demo`)
corpus/                 versioned test PDFs (see corpus/README.md)
fuzz/                   cargo-fuzz workspace, 9 targets (nightly/Linux to run)
tools/                  PDFium / MuPDF / hayro / poppler / ghostscript / pdf.js
                        differential harness and the text-extraction oracle
shaders/                WGSL for the experimental GPU paths
```

Dependency direction is enforced by the build graph: nothing below
`pdf-page-ir` imports render types. WGPU is owned by `lege-gpu`; the
experimental `pdf-render-wgpu` backend reuses that process-wide context rather
than creating another device.

## What it renders

The CPU path is feature-complete for the PDF 1.7 imaging model as PDFium
implements it. Items marked *approx* carry a documented approximation in
`DEFERRED.md`; everything else is implemented and exercised by the sweeps.

| area | coverage |
|---|---|
| Document structure | classic and stream xrefs, incremental updates, object streams, linearized files, damaged-file recovery (xref rebuild, orphan page scan, count-backed placeholders, `N R` reference repair), page-tree inheritance, bounded limits on every walk |
| Encryption | RC4 and AES, revisions 2–6, empty/user/owner passwords with real `/U` validation, per-object and identity crypt filters |
| Filters | Flate, LZW, ASCII85, ASCIIHex, RunLength (including unknown-length inline EOD), DCT, CCITT, JBIG2 (with globals), JPX, PNG/TIFF predictors |
| Paths | fills (nonzero/even-odd) with analytic exact-area coverage, strokes with full affine pen expansion, dashes, joins, caps, miter limits, degenerate and rotated transforms |
| Clipping | rectangular and path clips, text clip modes, nested clip stacks, cancellation inside long mask fills |
| Color | DeviceGray/RGB/CMYK, CalGray/CalRGB, Lab, ICCBased (RGB matrix/TRC and CMYK Lab-LUT shapes; other profiles fall back to the documented device policy), Indexed, Separation and DeviceN tint transforms, Pattern |
| Shadings | types 1–7 (function, axial, radial, free-form and lattice Gouraud, Coons, tensor), `/Extend`, `/Background`, shading-painted image masks |
| Patterns | tiling (colored and uncolored, including text and image masks as the painted content) and shading patterns, nested pattern-space anchoring |
| Transparency | constant alpha, isolated and non-isolated groups (backdrop removal *approx*), knockout groups, luminosity and alpha soft masks with `/BC` and `/TR`, all separable and non-separable blend modes, generalized to images, shadings, and tilings |
| Images | 1/2/4/8-bit samples (16-bit where the codec delivers them), `/ImageMask` stencils, `/SMask` (including `/SMaskInData` and `/Matte`), color-key `/Mask`, `/Decode`, inline images, nearest/bilinear and area-weighted minification with fractional mask footprints (the MRC path) |
| Fonts | Type 1 (native engine: eexec, charstrings, seac, flex), bare CFF and CID-keyed CFF, TrueType and OpenType via Skrifa, Type 0 with embedded and predefined CMaps (GB1, CNS1, Japan1, Korea1 CID→Unicode), vertical writing, Type 3 with graphics-state leak containment, the standard 14 bundled, PDFium-compatible substitution and descriptor-driven inference, opt-in system fonts, resolution-dependent hinting |
| Annotations | appearance streams (`/AP /N` with state selection, PDFium two-pass order), hidden/noview flags, synthesized appearances for Highlight/Underline/Squiggly/StrikeOut without `/AP` |
| Optional content | `/OCProperties` default configuration, `/OC` on XObjects and marked content, `/OCMD` visibility expressions |
| Text extraction | PDFium-parity character order, bidi, normalization, tight and loose boxes, words as exact unions of glyph boxes, rectangles, text-in-rect, optional annotation and soft-mask content |
| Output | RGB8 and Gray8 surfaces today through the frozen surface contract; postprocess graph for crop, resize, tone, threshold, dither, and 1-bit packing |

Not rendered: 3D and multimedia annotations, JavaScript and actions, form
field appearance *generation* (existing appearances are drawn), and spot-color
overprint simulation.

## Guarantees

These are tested properties, not aspirations; each has a gate in the tree.

- **No global state.** Every page compiles and renders against an immutable
  `DocumentSnapshot`; parse caches, scratch rasters, glyph and image caches
  are worker-local or explicitly shared, bounded, content-hash keyed
  structures. Six threads resolving six pages from one snapshot produce
  byte-identical results, and `pdf-test-support`'s determinism harness
  re-checks this across worker counts and repeated runs.
- **Never panic across a page boundary.** `RenderBackend` calls run under
  `catch_unwind`; a poisoned page becomes `RenderError::Panic`, at the backend
  and again at the scheduler. Workspace clippy lints deny `unwrap`, `expect`,
  and `panic` in library code. `pdf-chaos-tests` mutates real documents and
  asserts the invariant on stable Rust; nine `cargo-fuzz` targets cover the
  lexer, structure loading and decoding, content compilation, the JPEG, CCITT,
  and JBIG2 decoders, open-and-render, and text extraction. This is why the workspace
  release profile keeps `panic = "unwind"`.
- **Bounded memory and time.** `DocumentLimits` (object counts, reference
  chains, decoded bytes per context, revisions, page count),
  `ContentLimits` (nesting, operands, form and pattern recursion),
  `RenderLimits` (peak page bytes, group depth), separate encoded-input,
  decoded-output, pixel, and working-set ceilings at every codec boundary,
  and the scheduler's memory permits. Returned rasters are validated
  centrally before the sampler can index them, including those from injected
  codecs.
- **Cancellation.** One atomic `CancellationToken` observed by the scheduler,
  the backend, coverage and clip-mask fills, and inside the JPEG, CCITT, JPX,
  and JBIG2 decoders. Cancelled work discards partial output and resets
  reusable scratch; it never succeeds with a partial white tail.
- **Deterministic output.** The same input and options produce the same
  bytes regardless of thread count, worker assignment, or cache state; every
  performance pass in `performance-history/` is gated on unchanged page
  hashes.
- **Codecs are injected, never global.** `CpuBackendOptions::codecs` is a
  registry; a codec-free build passes `CodecRegistry::empty()`.

## Verification

The engine is graded differentially, not by eye. `tools/pdfium-diff` renders
the same page through this engine and through controls loaded at runtime
(a PDFium shared library; `mutool`, `pdftoppm`, `gs`, pdf.js, and hayro's
own renderer as subprocesses) and compares thresholded ink, gross pixel
difference, and continuous darkness. Disagreements are adjudicated against
at least two oracles before they count as defects; either oracle can be the
outlier.

- Sweep 15 (2026-07-26) is the current production baseline: 19,443 PDFs,
  100,109 sampled pages per oracle against both PDFium and MuPDF, six shards,
  zero failed shards, 2 h 08 m wall. PDFium classified 77,926 rows `ok` and
  22,105 `suspect`; MuPDF 84,687 and 15,244. Requiring both oracles to agree
  on direction with at least 0.05 ink delta reduced the suspects to 188 pages
  in 86 documents; every class that survived triage (Type 3 graphics-state
  leaks, malformed page trees, MRC soft-mask minification, patterned text,
  indirect CCITT parameters, JP2 palettes, cmap fallbacks, Type 1 `/Subrs`,
  shading-painted stencils, markup annotations without appearances) is
  closed with regressions.
- The native CCITT decoder is verified byte-identical to `hayro-ccitt` over
  a 1,260-case matrix; the JPEG decoder against libjpeg ground truth
  including fancy chroma upsampling; JPX against openjpeg on 441 real
  archive.org streams; 136 of 136 real embedded Type 1 faces parse.
- 1,023 unit tests across the crates plus integration suites per crate;
  `pdf-test-support` carries the diff metrics and corpus access they share.
- `tools/pdfium-text-diff` holds text extraction to PDFium's public surfaces
  (characters, boxes, rectangles, words).

The sweep corpora are external to the repository; `corpus/` holds the
self-authored fixtures and the perf manifest. The planned redistributable
benchmark corpus is described in `../CRATES-IO-PLAN.md` §9.

## Performance

Measured on an i7-13700H (20 logical cores), release profile, scale 2.0,
through `tools/pdfium-diff bench` and `profile`. Numbers are from the dated
notes in `../../dev-notes/lege-pdf/render-refinement/performance-history/`;
they are self-reported and PDFium is the only engine measured head-to-head so
far.

- **Whole documents** through the parallel scheduler (compile pool → render
  pool with memory permits): 1.5–11× PDFium's sequential throughput depending
  on class, e.g. a 74-page Latin text document at 316 pages/s against 213
  (2026-07-20), and JPEG scan documents at 5.8–11.2× (image-decode-bound
  pages parallelize across both pools).
- **Single pages, one thread:** image-heavy pages are at or ahead of PDFium
  (JPEG scans 0.6× of PDFium's time); Latin text body pages still cost
  1–3.2× PDFium per page, because the per-glyph path is slower than
  FreeType's even with the glyph cache.
- The 2026-09-04 pass reused the page surface across renders and fanned every
  image row loop over the rayon pool: scanned, bilevel, and MRC pages dropped a
  further 2.2–4.8× per page with identical output hashes, text and vector
  pages unchanged. PDFium has not been re-measured since.

What makes it fast: analytic coverage with `KernelSet` span dispatch; the
content-hash keyed `SharedImageCache` (96 MiB, 8-shard LRU); the font and glyph
caches; a JPEG decoder with a u64 bit buffer, batched AC decode, and AVX2
IDCT and color conversion; area-weighted minification with packed-bit
popcount for bilevel masks; JPX decoding with resolution reduction when the
placement is small; and the scheduler's deterministic reordering, which lets
slow pages overlap without changing output order. Tuning knobs are
environment variables prefixed `PDF_RENDERER_` (`IMAGECACHE`, `FONTCACHE`,
`GLYPHCACHE`, `GLYPH_SUBPIXEL`, `IMAGE_ROW_PAR`, `JPX_CONCURRENCY`,
`JPX_REDUCE`, `SURFACE_REUSE`, each with a `_STATS` companion where it caches).

The reproducible head-to-head against hayro, pdf-toolkit, PDFium, and MuPDF
is phase 1 of `../CRATES-IO-PLAN.md`; until it has run, no cross-engine ratio
other than the PDFium figures above should be quoted.

## Using it

Library entry points, bottom to top:

```rust
let source: Arc<dyn PdfSource> = Arc::new(MmapSource::open(path)?);
let doc = DocumentSnapshot::open_with_password(source, DocumentLimits::default(), Some("…"))?;
let compiler = PageCompiler::new().with_annotations(true);
let mut ctx = ParseContext::new();
let page = compiler.compile_artifacts(&doc, PageIndex(0), &mut ctx)?;  // semantic + compiled, one pass
let text = TextPage::build(&page.semantic, &TextPageOptions::default());
let backend = CpuBackend::new(CpuBackendOptions::default());
let raster = pdf_render_api::render_blocking(&backend, request_for(&page.compiled, scale))?;
```

`PageCompiler::compile_artifacts` is the preferred entry for document
applications: it returns the retained `SemanticPage` and the lowered
`CompiledPage` from one interpretation. `RenderScheduler::render_range`
drives whole documents through both pools. `lege-pdf-read` (`../read`) is the
owned seam the processor, the print crate, and the C ABI use; the published
facade described in the plan replaces it.

The developer driver:

```sh
cargo run -p pdf-cli -- info      <file.pdf> [--password <pw>]
cargo run -p pdf-cli -- doctor    <file.pdf>            # structural triage
cargo run -p pdf-cli -- dump      <file.pdf> <page>     # semantic display list
cargo run -p pdf-cli -- text      <file.pdf> <page> [--chars] [--rects] [--words] [--rtl]
cargo run -p pdf-cli -- render    <file.pdf> <page> <out.ppm> [--system-fonts]
cargo run -p pdf-cli -- attribute <file.pdf> <page> <out-prefix>   # difference attribution
cargo bench -p pdf-render-cpu                           # synthetic raster probe
```

`PDF_PASSWORD` is preferred over `--password` because argv is world-readable.

## Known limits

Each item below is tracked in `DEFERRED.md` with its status and the reason
it was deferred.

- **GPU execution is experimental.** `pdf-render-wgpu` paints decoded images
  on the GPU and falls back to the CPU backend for everything else; it is not
  a general renderer and is excluded from the published crate.
- **Output surfaces** are RGB8 and Gray8 over an opaque background; RGBA
  with a transparent background is part of the facade work in the plan.
- **ICC** coverage is the RGB matrix/TRC and CMYK Lab-LUT shapes that real
  documents use; other profile shapes fall back to the device policy.
- **Multiple Master** substitutes (`FoxitSansMM`, `FoxitSerifMM`) are not
  interpolated; the nearest standard face is used.
- **JPX** declines, as typed errors rather than wrong pixels: POC progression
  changes, subsampled components, arithmetic-bypass code-block style,
  truncated-packet salvage.
- **Hinting** is opt-in (`HintingPolicy::Auto`) because it moves glyph
  geometry away from the frozen surface contract.
- **System fonts** are opt-in because they make output host-dependent; the
  bundled standard 14 keep renders deterministic.
- **Non-isolated groups** import the parent backdrop but do not perform the
  backdrop removal of ISO 32000 §11.4.8 on the way out; this is the one
  documented approximation left in the transparency model.
