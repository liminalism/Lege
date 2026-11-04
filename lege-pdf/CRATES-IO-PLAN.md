# lege-pdf on crates.io: publication plan

Written 2026-11-03 against commit `94ae2770` on `claude/festive-bohr-162zke`.
Everything stated about the tree was checked in the tree; everything stated
about other crates was checked on crates.io or docs.rs the same day. Numbers
that were not measured in this session are marked as such.

The plan answers five questions in order: what we publish, in what shape,
what has to change first, how we prove the performance claim, and why one
might not do this at all. The decisions that are yours, not mine, are
collected in §13.

---

## 1. Summary

**Publish two crates, not one and not twenty.**

| crate | contents | status after this plan |
|---|---|---|
| `lege-pdf` | read, text, render (CPU), print (feature) | the product; 0.4.0 is the goal (the renderer workspace was numbered 0.4.0 on 2026-11-04; `lege-pdf/render/CHANGELOG.md`) |
| `lege-pdf-write` | the typed, append-only, image-oriented emitter | 0.1.0 after its honesty fixes; optional re-export from `lege-pdf` under a `write` feature later |

Not published: `agent` (stays an in-repo binary over `lege-pdf`), `pdf-integrity`
(own workspace, depends on `lege-pdf` by path), `pdf-render-wgpu` and the
`gpu` half of `pdf-postprocess` (stay in-repo, depend on `lege-pdf` plus
`lege-gpu`), `pdf-cli`, `pdf-chaos-tests`, `pdf-test-support`, `fuzz/`,
`tools/`, `corpus/`.

**There is no editor to publish.** Nothing in the tree modifies an existing
PDF. "Edit and write as a different crate" reduces to "write as a different
crate", and that is what §3 recommends.

**The performance claim is not yet backed by anything in the tree.** No
recorded timing of lege-pdf against hayro, pdf-toolkit, or MuPDF exists in
`dev-notes/` or `.akr/`. The only head-to-head numbers are against PDFium and
they are mixed (§9.1). The headsup benchmark in §9 is therefore not a nice
addition; it is the gate for every speed sentence in the README.

**Order of work:** benchmark harness first (it runs against the crates as they
are today), consolidation and facade second, print fixes third, packaging
last. §12 has the phases and gates.

---

## 2. Where the tree stands today (verified)

### 2.1 Inventory

| part | package(s) | lines of Rust | runtime deps beyond the tree |
|---|---|---|---|
| `render/crates/*` | 24 `pdf-*` packages | 105,939 | thiserror, memmap2, flate2 (zlib-rs), bitflags, rayon, crossbeam-channel, skrifa 0.44, jp2lam, jbig2enc-rust (decode), jpeg-encoder, png, hayro-ccitt (**dev-only**, differential oracle) |
| `read/` | `lege-pdf-read` | 3,303 | png, jpeg-encoder, skrifa, image (**declared, unused**) |
| `write/` | `lege-pdf-write` | 4,425 | thiserror, flate2 |
| `print/` | `lege-pdf-print` | 7,519 | png; `windows` 0.62 on Windows only |
| `agent/` | `lege-pdf-agent` | 6,673 | clap, serde, serde_json, anyhow, image, **lege-ocr (paddle-ocr → lege-gpu)** |
| `pdf-integrity/` | nested workspace | — | cms, x509-cert, rsa, sha2, … |

Tests: 1,023 `#[test]` functions across the render crates, 27 in read, 66 in
write, 110 in print. Nine `cargo-fuzz` targets. One chaos/mutation gate.

### 2.2 `lege-gpu` edges

Exactly two, both in `render/crates`:

- `pdf-render-wgpu/Cargo.toml:11` — a hard path dependency. The crate is the
  experimental image-only WGPU backend.
- `pdf-postprocess/Cargo.toml:16` — optional, behind the `gpu` feature; only
  `src/gpu.rs` uses it.

Nothing else in read, print, or the render crates names `lege-gpu`. Keeping
it out of the published crate costs nothing architecturally: the CPU backend
is the normative implementation, `RenderBackend` is a trait, and the WGPU
crate reaches the CPU side through the `PreparedRgbImagePage` seam. See §5.

### 2.3 Registry readiness of the codecs

Both codecs were published yesterday and today, so the renderer's image
dependencies already resolve from crates.io:

| crate | registry | tree | notes |
|---|---|---|---|
| `jbig2enc-rust` | 0.6.0 (2026-11-02), MIT AND Apache-2.0 | 0.6.0 | renderer uses `default-features = false, features = ["decode"]` |
| `jp2lam` | 0.3.0 (2026-11-03), MIT OR Apache-2.0 | 0.3.0 | the tree's manifest has a **path** dependency on `../../../jpegXL-rs/JPXL/crates/jpxl-perceptual`; the registry copy depends on `jpxl-perceptual ^0.3.0`, which is published (0.3.0, 2026-09-08) |

One caveat for a decode-only consumer: `jp2lam` pulls `jpxl-perceptual`,
`tempfile`, `memmap2`, and `rayon` unconditionally, and `jbig2enc-rust`
pulls `anyhow`, `fax`, `ndarray`, `rustc-hash`, `log`, `wide` even with only
`decode` enabled. `ndarray` is used in `decode/mmr.rs` and `fax` in the
halftone encoder. Slimming those feature sets is codec-crate work (§6.4), not
a blocker.

### 2.4 Standalone build probe (done in this session)

A scratch workspace containing only `render/crates/*` (minus `pdf-render-wgpu`
and `pdf-cli`), `read`, `write`, `print`, and the two codecs, with the
`jpxl-perceptual` path dependency replaced by its registry version and the
`gpu` feature's `lege-gpu` dependency removed, passes
`cargo check --workspace --all-targets` in 22 s on this container with every
other dependency fetched from crates.io. The full test run is reported in
§2.6.

The ecosystem workspace itself does **not** load here: `lege-process` depends
on `../liminalkit/crates/liminalkit-raster` and `jp2lam` on `../jpegXL-rs`,
both sibling checkouts. That is fine for the monorepo and fatal for a
published crate, which is why §11 requires a from-the-`.crate`-archive build
in CI, in an empty directory.

### 2.5 Package size

Measured with `tar czf` over the would-be package contents:

| contents | gzip size |
|---|---|
| Rust source only (render, read, write, print; no tests) | 0.88 MB |
| plus `pdf-font/src/cmap_data` (CJK CMaps) | +0.51 MB |
| plus `pdf-text/upstream/pdfium` (Unicode tables, build-time input) | +0.25 MB |
| plus the 14 Foxit faces | +0.25 MB |
| **everything above** | **1.87 MB** |
| with all `tests/` directories | 2.44 MB |

The 10 MiB crates.io limit is not in play. The one thing to `exclude` is
`pdf-image/tests/fixtures/nomad_flute_p0.truth.bin` (1.96 MB raw).

### 2.6 Standalone test run

See the end of this document (§15); the run was in progress when the plan was
written and its result is appended there.

### 2.7 Names

Checked on crates.io today: `lege-pdf`, `lege-pdf-read`, `lege-pdf-write`,
`lege-pdf-print`, `lege-pdf-render` are free. Of the internal crate names,
`pdf-font`, `pdf-content`, `pdf-syntax`, and `pdf-text` are **taken** by
unrelated crates. Publishing the graph under its current names is therefore
impossible, which settles one half of §4.

---

## 3. One crate or two: my opinion

You asked whether `lege-pdf` should be render and print only, with edit and
write published separately. After reading all four seams: **yes, split, and
drop the word "edit".**

**Why write is a separate crate**

- It is a different program for a different user. `lege-pdf-write` takes
  already-encoded page artifacts in arrival order and emits a PDF with an
  invisible text layer. Its audience is scan-to-PDF and OCR pipelines. The
  renderer's audience is viewers, converters, and services. Neither audience
  wants the other's API in its docs.
- It has two dependencies and no relationship to the renderer's internals. A
  `write` feature inside `lege-pdf` would have to be a pure re-export anyway.
- Its maturity is lower and its fixes are independent (§7). A 0.x renderer
  will break its API often; the writer should not be forced to bump with it.
- It is the one place where the outside review's "jack of all trades"
  framing would take hold. A separate README can say exactly what it is: an
  emitter with a closed vocabulary, not an authoring library.

**Why not three.** Print belongs inside `lege-pdf` as a feature. It is 7.5 k
lines over `read` with one platform dependency, and its most valuable future
feature, impose-to-PDF (§8.3), needs the reader's object graph. A feature flag
keeps it out of builds that do not want `windows`.

**Why the door stays open.** Once both are published, `lege-pdf` can add
`write = ["dep:lege-pdf-write"]` and `pub use lege_pdf_write as write;`. The
consumer who wants one line gets one line; the consumer who wants only the
emitter never compiles a rasterizer.

**What about editing.** The outside review's §6 table (page import, merge,
incremental update, forms, signatures) describes a product that does not
exist and that you said you do not want. The only item from that list worth
owning is page import, because imposition needs it (§8.3). Treat it as a
print feature, not an editor.

---

## 4. Package shape: consolidate the graph into one package

Two viable shapes:

| shape | published packages | what changes | cost | risk |
|---|---|---|---|---|
| **A. Consolidated** | 1 (`lege-pdf`) | 17 crates become modules of one package under `lege-pdf/src/`; `use pdf_x::` becomes `use crate::x::` | one-time import migration, loss of per-crate pipelining in dev builds | none on crates.io; module privacy replaces the build-graph layering rule |
| B. Renamed graph | 17 + 1 facade | every `pdf-*` package becomes `lege-pdf-*`; a facade re-exports | one-time rename (same churn as A), 18 versions in lockstep, 18 docs.rs pages | each internal crate becomes a public semver surface |

**Recommendation: A.** The reasons the review gave are right, and two more
decide it: four of the current names are already taken (§2.7), so B is a full
rename anyway; and under B every internal refactor between crates is a
breaking change for anyone who depended on an internal crate directly, which
people do (hayro-syntax has its own dependents). Under A the internals can be
exposed as `#[doc(hidden)]` modules behind an `unstable-internals` feature
with no semver promise.

**What consolidation does not change.** The immutable snapshot, the
compiled-page IR, the worker-local execution, the `RenderBackend` trait, the
codec registry, and every test stay exactly where they are. The dependency
direction the README promises ("nothing below `pdf-page-ir` imports render
types") becomes a test: a 60-line script that parses `use` statements per
module and fails CI on a forbidden edge. That is cheaper than it sounds and
more precise than Cargo, which only enforced direction at crate granularity.

**Churn estimate.** 432 cross-crate `use` lines and about 1,500 inline
`pdf_x::` path references in render, read, and print; `pdf_page_ir::` alone is
591 of them. This is a sed job with a compile-and-fix tail, roughly two days
including re-running the full suite, not a rewrite.

**Dev-build cost.** The frontend of one 95 k-line crate is serial where 17
crates pipelined. Expect `cargo check` after a one-line change in
`pdf-render-cpu` to take somewhat longer than today. Incremental compilation
still applies within the crate. If this hurts the renderer team's loop, the
mitigation is to keep `pdf-render-cpu`'s heaviest pieces as `#[path]` modules
compiled only under a `cfg`, not to abandon A.

### 4.1 Proposed layout

```text
lege-pdf/
├── Cargo.toml              package `lege-pdf`
├── README.md               the crates.io README (§10)
├── LICENSE*                see §6.1
├── THIRD-PARTY-NOTICES.md  PDFium Unicode data, Foxit faces (moved up from render/)
├── CHANGELOG.md
├── build.rs                today's pdf-text/build.rs (Unicode tables)
├── src/
│   ├── lib.rs              facade: Document, Page, RenderOptions, Bitmap, Text, …
│   ├── geom/  source/  syntax/  object/  structure/  security/  document/
│   ├── color/  function/  font/  image/  content/  ir/   (pdf-page-ir)
│   ├── render/             api/ (pdf-render-api)  cpu/  scheduler/
│   ├── postprocess/        CPU executor only
│   ├── text/
│   ├── doctor/             pdf-read
│   ├── print/              lege-pdf-print, behind `print`
│   └── session/            what lege-pdf-read becomes (§6.2)
├── fonts/  cmap_data/  upstream/   (data moved beside the modules that embed it)
├── examples/
├── tests/                  the integration tests, one file per former crate
├── write/                  package `lege-pdf-write` (unchanged location)
├── wgpu/                   package `pdf-render-wgpu` → `lege-pdf-wgpu`, publish = false
├── agent/                  unchanged, publish = false
├── pdf-integrity/          unchanged
├── bench/                  the headsup harness (§9), publish = false
├── corpus/  fuzz/  tools/  unchanged
└── render/README.md        keeps describing the engine (updated this session)
```

`render/crates/` disappears as a directory once the move lands. Git history
is preserved by `git mv`.

### 4.2 Feature contract

```toml
[features]
default = ["jpx", "jbig2", "mmap", "export"]
jpx     = ["dep:jp2lam"]            # /JPXDecode
jbig2   = ["dep:jbig2enc-rust"]     # /JBIG2Decode
mmap    = ["dep:memmap2"]           # MmapSource; owned/file sources otherwise
export  = ["dep:png", "dep:jpeg-encoder"]  # encode rendered pages
print   = ["dep:png"]               # imposition, spoolers; `windows` under cfg(windows)
system-fonts = []                   # FolderFontProvider opt-in (no new deps)
scheduler = []                      # kept always-on; listed for the docs
profiling = []                      # stage timers (today's pdf-profiling)
unstable-internals = []             # exposes the module tree, no semver promise
```

Reading, text, compilation, and the CPU rasterizer are not features. A PDF
library that cannot render is a different crate. `rayon` and
`crossbeam-channel` stay unconditional: the CPU backend fans out image rows
and clip masks over rayon even for one page.

Dropped from the published manifest: `gpu` (§5), the `demo` binary, the
`hayro-ccitt` dev-dependency's role as a gate (keep it as a dev-dependency;
it is permissively licensed and only runs in tests).

### 4.3 Dependency budget

Measured in the probe (§2.4): `cargo tree -e normal` for `lege-pdf-read`
resolves to **85 unique crates** today. The budget for `lege-pdf` 0.1 with
default features is **at most 60**, which is roughly where hayro sits. The
levers, in order of yield:

- the dead `image` dependency in `read/Cargo.toml:10` alone drags `tiff`,
  `image-webp`, `zune-jpeg`, `zune-core`, `moxcms`, `pxfm`, `weezl`,
  `byteorder-lite`, `half`, `quick-error`, and a second `fax` version — about
  a dozen crates for nothing;
- `jbig2enc-rust`'s `decode` feature pulls `ndarray`, `matrixmultiply`,
  `rawpointer`, `num-complex`, `num-integer`, `anyhow`, `log`;
- `jp2lam` pulls `jpxl-perceptual`, `tempfile`, `rustix`, `linux-raw-sys`,
  `fastrand`, `getrandom` on the decode path;
- `bytemuck_derive`, `syn` (two versions), `quote`, `proc-macro2` come in
  through `bytemuck`'s derive feature and `thiserror`; derive-free `bytemuck`
  is a one-line change.

Removing `image` and doing §6.4 lands near 55.

---

## 5. Keeping `lege-gpu` out

- `pdf-render-wgpu` is not part of the package. It becomes `lege-pdf/wgpu/`,
  package `lege-pdf-wgpu`, `publish = false`, depending on `lege-pdf` (path,
  with `unstable-internals`) and `lege-gpu`. Nothing it needs is removed:
  `RenderBackend`, `CpuBackend`, `PreparedRgbImagePage`, `CpuWorkerContext`
  stay public under the internals feature.
- `pdf-postprocess`'s `gpu.rs` moves into that same crate. The published
  `postprocess` module is the CPU executor and the graph types only.
- `pdf-cli`'s hard dependency on `pdf-render-wgpu` moves with it; `pdfr` stays
  in-repo as a dev driver over `lege-pdf` + `lege-pdf-wgpu`.
- When GPU rendering is live, it ships as `lege-pdf-gpu` on crates.io with
  `lege-gpu` published alongside, and `lege-pdf` never grows a wgpu edge. The
  README says exactly that so nobody waits for it.

---

## 6. What must change before 0.1.0

### 6.1 Licensing (decision, not code)

Facts: the workspace license is `AGPL-3.0-only` and every render crate and
`lege-pdf-print` inherit it; `lege-pdf-read` says `Apache-2.0` in its own
manifest (an inconsistency, since it links AGPL crates); `lege-pdf-write` has
no license field and `publish = false`; the C ABI README already warns that
anything linking it is a combined AGPL work. Your codecs are MIT/Apache. The
bundled PDFium Unicode sources and Foxit faces are BSD-3 via PDFium's
LICENSE, compatible with either choice, and must ship in
`THIRD-PARTY-NOTICES.md` inside the package.

Options:

1. **AGPL-3.0-only, MuPDF posture.** Allowed on crates.io (`mupdf` is AGPL
   with 1.77 M downloads). Keeps a commercial-license lane. Excludes every
   permissively licensed consumer: Typst, krilla, any library crate, any
   company that will not take AGPL. Those are exactly hayro's users.
2. **MIT OR Apache-2.0.** Maximum reach; the engine becomes free to everyone
   including competitors; `lege-process` and the GUI can stay AGPL.
3. **AGPL for the crate plus a published commercial option** stated in the
   README. Same reach as 1 with a clearer story.

My read: if the goal stated in your brief ("throw it into the gauntlet",
displace hayro's inertia) is the real goal, only 2 achieves it, because the
inertia is Typst's dependency on hayro and Typst cannot adopt AGPL code. If
the goal is a credible public artefact that also protects a product, 3 is
honest and defensible, and the README should then not frame itself as a hayro
replacement for library authors. Decide this before anything else; it changes
the README and the manifest and nothing else.

Whatever the choice, fix `lege-pdf-read`'s stray `Apache-2.0` and give
`lege-pdf-write` an explicit license.

### 6.2 The public API (the facade)

`lege-pdf-read` has the right shape and the wrong names. It wraps renderer
types completely (every surface, token, and error is its own newtype), which
is the hard part. What leaks is the processor: `RenderSession`,
`PageOutputPlan { analysis, base, regions, ocr }`, `GraySuitability`,
`preserved_jbig2_smask`, `PageTextEvidence::direct_scan`. Those stay in-repo
for `lege-process`, behind a `lege-pipeline` feature or in `lege-process`
itself; they are not the published API.

The published 0.1 surface, built on what exists:

```rust
let doc = lege_pdf::Document::open(bytes, &OpenOptions::default().password("…"))?;
let page = doc.page(0)?;                    // Page: size, rotation, boxes, label
let bmp  = page.render(&RenderOptions::scale(2.0).format(PixelFormat::Rgba8))?;
let text = page.text()?;                    // words with boxes in points, plain text
let all  = doc.render_all(&opts, Parallelism::Auto, |i, result| …);  // scheduler
```

Concretely:

- `Document::open{,_path,_mmap}` with `OpenOptions { password, limits:
  DocumentLimits, annotations: bool, system_fonts: Option<…>, hinting }`.
  `examine()` from `pdf-read` becomes `Document::diagnose()`.
- `Page` is `compile_artifacts()`-backed: one interpretation yields both the
  `SemanticPage` (text, image inventory) and the `CompiledPage` (raster).
  Today `read/src/session.rs:407` compiles for raster and
  `read/src/text.rs:225` compiles again with a fresh `ParseContext` for text;
  `has_text_layer` + `page_text` on one page is two interpretations. The
  single-pass entry point already exists at `pdf-content/src/lib.rs:339` and
  is documented as "the preferred entry point for document applications".
- `RenderOptions { scale | dpi, format: Rgba8 | Rgb8 | Gray8 | Mono1,
  background: Transparent | White | Color, annotations, color_policy, crop,
  limits, cancellation }`. Today `render()` always composites on white and
  drops alpha (`session.rs:469, 610-650`); RGBA output is table stakes for a
  viewer.
- `Text`: UTF-16-safe plain text, words with boxes in **points** (today they
  are returned pre-scaled into caller pixel space), characters, rectangles,
  text-in-rect. All of it is in `pdf-text` already.
- `Document::outline()`, `metadata()` (full Info dictionary, not just Title
  and Author), `page_labels()`.
- `render_all` over `pdf-render-scheduler`, which `lege-pdf-read` does not
  use today. The scheduler's memory permits and deterministic reordering are
  the thing nobody else on crates.io has; expose it.
- Structured errors (`ReadError` variants are all `String` today).
- `Send + Sync` for `Document` and `Page`, asserted by a compile-time test;
  the FFI already relies on it.
- Delete the dead `image` dependency in `read/Cargo.toml:10`; move
  `font_metrics.rs` (a processor convenience over skrifa) to `lege-process`.

Effort: about one week, most of it naming, docs, and examples, because the
mechanisms exist.

### 6.3 Hygiene the probe and reviews surfaced

- `print/tests/job.rs:118-121` reads `hello_world.pdf` from the render tree;
  `cargo package` would break it. Copy the fixture into the crate.
- `pdf-test-support::corpus_dir()` resolves `../../corpus`; integration tests
  that need it stay in the repo (`tests/` of the package is included by
  `cargo package`, so either vendor the few PDFs they need or gate them on an
  env var).
- Workspace lints `unwrap_used = deny` etc. must be restated in the package
  manifest (`[lints]` is not inherited by a consumer, but the package must
  still pass them).
- `unsafe`: 36 sites, all in three places: AVX2/SSE kernels in the JPEG
  decoder and the coverage converter (`pdf-image`, `pdf-render-cpu`), the
  `memmap2` map (`pdf-source`), and `MaybeUninit::assume_init` buffer
  initialisation. The README should say so in one sentence rather than let
  `cargo geiger` say it.
- MSRV is 1.97 (edition 2024, let-chains). That is four months old; hayro
  declares 1.92. Fine, but state it and test it in CI.
- `pdf-text/build.rs` verifies SHA-256 of the vendored PDFium inputs and
  stops the build on mismatch. Keep it; mention it in THIRD-PARTY-NOTICES.

### 6.4 Codec dependency slimming (nice to have, in the codec repos)

`jp2lam`: make `jpxl-perceptual` optional under an `encode`/`quality`
feature; drop `tempfile` from the decode path. `jbig2enc-rust`: check whether
`ndarray` is needed by `decode/mmr.rs` or only convenient, and whether `fax`
can sit behind `encode`. Each is a small release of a crate you already
publish. Saves perhaps ten crates in the consumer's tree.

---

## 7. `lege-pdf-write` improvements (the honest minimum)

The outside review's three claims about the writer are all true, and the
in-session review found more. Verified against the code, with effort:

| # | issue | where | fix | effort |
|---|---|---|---|---|
| 1 | Leaked object numbers: `write_outline` and `write_structure_tree` allocate an id, then bail, leaving an xref entry at offset 0 | `outline.rs:46-50`, `writer.rs:395-398` | allocate after resolution | 1 h |
| 2 | Raw-byte injection: `/Lang`, structure `/S` names, `/PageLabels` written without `write_name`/`write_text_string`; `set_structure(["Fig ure)"])` produces a broken catalog | `pages.rs:209-222`, `writer.rs:403-408` | route through the serializer or make them typed | 3 h |
| 3 | PDF/A: `PdfA1b` emits `/GTS_PDFA1` OutputIntent with no ICC stream, no XMP, no `/ID`; default metadata carries `Subject "PDF/A Document"` and `Keywords "PDF/A-1b"` under every profile | `meta.rs:28-102`, `meta.rs:76-77` | remove the profile (or rename `PdfA1bPartial` and document the gaps); drop the strings from `Default` | 2 h (real PDF/A-1b: 2–3 days, later) |
| 4 | Structure tree writes `/K (string)`, which is not a legal `/K`; `MarkInfo /Marked true` without any marked content | `writer.rs:388-424` | remove `set_structure` for 0.1; a real tagged-PDF implementation is a separate milestone | 1 h |
| 5 | CCITT `black_is_one: false` inverts (`/Decode [1 0]` unconditional) | `images.rs:134-138` | conditional Decode | 30 min |
| 6 | Helvetica fallback has no `/Encoding` but text is Latin-1 | `font.rs:243-253`, `text.rs:98-108` | `/WinAnsiEncoding` | 30 min |
| 7 | Classic xref overflow past 10 digits is a `debug_assert!` | `xref.rs:43-47` | hard `WriteError` now; xref streams later | 1 h / 1 day |
| 8 | No trailer `/ID` | `xref.rs:52-68` | content hash or caller-supplied | 2 h |
| 9 | Flat page tree (the file header promises balanced) | `pages.rs:2-7, 134-164` | fan-out ≤ 64, skeleton from `total_pages` | 1 day |
| 10 | One XObject per submitted image; dedup only for registered shared ids | `writer.rs:183-184` | content-hash dedup through the existing registry | half a day |
| 11 | Clippy `unwrap_used` fails at six library sites | `outline.rs:61,63,118,120`, `writer.rs:211,233` | fix | 1 h |
| 12 | Publishing hygiene: `publish = false`, no license/repository/readme fields, `workspace = true` deps, README with repo-relative paths, every module `pub` | `Cargo.toml`, `lib.rs:9-23` | concrete manifest, `pub(crate)` for sink/serialize/xref/pages, a `tests/` round-trip through qpdf or pypdf | half a day |

Items 1–8 and 11–12 are the 0.1.0 gate: about three days. Items 9–10 are
0.2. Anything from the outside review's "general vector authoring" list is
explicitly out of scope; the crate's README will say "image-oriented emitter"
in its first sentence.

---

## 8. `print` improvements

### 8.1 What the review confirmed

The composed route always rasterizes; there is no vector-preserving
imposition. The banded compositor exists (`compose.rs:196-261`) but **no
backend uses it**: CUPS, Windows, and file all call the full-raster
`compose_sheet`. Windows is `accepts_pdf: false` by construction
(`windows.rs:114`) and the whole winspool/GDI path is compile-verified only,
never run on a printer. CUPS is driven through `lp`, `lpstat`, `lpoptions`,
`cancel` subprocesses with no timeouts.

### 8.2 Fixes before `print` ships as a feature (3–5 days)

1. Booklet imposition mirrors backs for a short-edge flip but `Duplex` is
   passed through unchanged (`layout.rs:406`, `cups.rs:197`); coerce or
   reject the combination.
2. Windows decides orientation per sheet (`layout.rs:352`) but sets one
   `DEVMODE` from `sheets[0]` (`windows.rs:133`); force one orientation per
   job or switch DEVMODE per page.
3. `PrintDc::drop` calls `EndDoc`, which prints a partial job on error
   (`windows.rs:453`); use `AbortDoc`.
4. Windows hardware margins are read from a portrait DC and never rotated
   (`windows.rs:88-89`).
5. Subprocess timeouts on every CUPS call (`cups.rs:585-640`).
6. Hoist the per-sheet compiled-page cache to the job (`compose.rs:216`).
7. Deliver bands to `StretchDIBits` and to the PNG encoder so a 600 DPI A3
   sheet is not one 150 MB allocation.
8. Fixture path out of the crate (§6.3). Drop the `ReadError::Encode` abuse
   for PNG errors (`preview.rs:65,91`).
9. Expose a printer-independent preview: `plan_sheets` +
   `render_preview_png` exist; add `preview_sheets(...) -> impl Iterator`.
10. Mark Windows spooling **experimental** in the docs until it has been run
    against one real queue, or run it against one before 0.1.

### 8.3 The headline feature for 0.2: impose-to-PDF

A `print` that can only rasterize is a weaker story than a `print` that can
write a booklet PDF with its text intact. The pieces: `layout::impose`
already yields `Placement { transform, clip }`; what is missing is (i) a
`PdfObject` serializer (pdf-object has none), (ii) a deep copy of each source
page's `/Contents` + `/Resources` subgraph with renumbering and raw stream
pass-through through `DocumentSnapshot`'s resolver, (iii) wrapping each page
as a Form XObject (`/BBox` = source box, `/Matrix` = placement) drawn with
`q cm /Fx Do Q` per cell, (iv) an xref writer, which `lege-pdf-write` has.
Encrypted sources are decrypted on copy. Roughly 1.5–2.5 k lines; two to
three weeks. It is also the only "editing" capability worth owning (§3).

Deferred beyond that: direct IPP (`Print-Job` over HTTP) to drop the `lp`
dependency; PWG raster output; real CUPS hardware margins from the PPD; a
macOS run.

---

## 9. The headsup benchmark

### 9.1 What is actually on record

Every recorded head-to-head is against PDFium, from `pdfium-diff bench` on an
i7-13700H, scale 2.0, release build, private corpus:

- `handoffs/DEFERRED.md:570-581`: per-page single-threaded, PDFium 4–6×
  faster (MRC scan 142 ms vs 34 ms; text page 39 ms vs 6.6 ms); whole
  document through the scheduler, ours 1.65–2.34× faster.
- `memories/linux-root/pdfrenderer-perf-pass2.md:48-93` (2026-07-20): after
  the pass, whole-document Latin text 316 vs 213 pages/s (1.49× faster than
  PDFium); JPEG scan documents 5.8–11.2× faster; body text pages still
  1–3.2× PDFium per page.
- `performance-history/optimization-surface-imagerows-20260904.md`: a later
  pass took scanned, bilevel, and MRC pages down a further 2.2–4.8× per page
  (hashes identical), text and vector pages unchanged. No PDFium rerun after
  it.

No number against hayro, pdf-toolkit, pdf-render, or MuPDF exists in the
tree, and hayro 0.9.0 (released 2026-11-02) added glyph caching, text
hinting, and "performance improvements" across every sub-crate. The "5× faster
than all crates" result you remember is not in this repository; until it is
reproduced it cannot go in a README.

### 9.2 Harness: `lege-pdf/bench/`

A new out-of-workspace tool, `publish = false`, replacing the `benchmark`
subcommand of `pdfium-diff` for timing (the sweep and compare code stay where
they are and are reused).

**Renderers, all in-process, pinned by version in the lockfile and recorded
in every result file:**

| renderer | how | notes |
|---|---|---|
| lege-pdf | library | `CpuBackend` single page; `RenderScheduler` whole document |
| hayro 0.9.x | library, `embed-fonts` | font resolver ported from hayro's own `examples/render.rs` so standard-14 and CJK fallbacks match its test setup; opaque white background |
| pdf-toolkit 0.3.x | library, `raster` | 390 downloads; included for completeness, not positioning |
| pdf-render 1.3 (PDFluent) | library | hayro-0.5-derived; optional |
| PDFium | `pdfium-render` 0.9.5 over a pinned pdfium-binaries release | not thread-safe; sequential only |
| MuPDF | `mupdf` 0.8.0 (builds from source; AGPL is fine for a tool) | one context per thread for the parallel run, or sequential |

Each renderer runs in its **own child process** of the same binary
(`--renderer X`) so peak RSS (`getrusage`) and crashes are attributable, with
timing taken inside the child around the render call only. No PNG encode in
the timed region for anyone.

**Corpus.** Redistributable, licence-tagged, SHA-256 manifest, committed under
`bench/corpus/` (target ≤ 200 MB, ~60–100 documents, ~1,000 pages), stratified:

| class | why |
|---|---|
| Latin text: CFF, TrueType, Type 1, non-embedded | where PDFium still leads per page |
| CJK, vertical writing | CID tables, substitution |
| vector-heavy (maps, CAD, 8 k+ edges) | coverage kernel |
| shadings (axial, radial, mesh 4–7), tiling patterns | |
| transparency: groups, soft masks, blend modes, knockout | hayro lacks knockout/isolation; recorded as `unsupported`, not slow |
| JPEG scans, progressive and baseline | decoder throughput |
| JPX and JBIG2 MRC (Internet Archive style) | the codec story |
| CCITT G4 | |
| Type 3 fonts, inline images | |
| encrypted R2–R6 | hayro cannot open them; recorded as `unsupported` |
| malformed, recovered page trees | robustness, counts failures not time |

Sources: the pdf.js and PDFBox test corpora (file-by-file licence check; both
already feed the sweeps and hayro's own suite, which is a bias to state in the
report), self-authored fixtures (`corpus/shadings`), public-domain scans from
archive.org (record identifiers), and the 13-page `corpus/perf/pages.tsv`
set once its files are licence-cleared.

**Protocol.**

- Scales 1.0 and 2.0 (72 and 144 DPI); RGB8 opaque white; identical pixel
  dimensions by `floor(dim × scale)` for everyone; dimension mismatch voids
  the row.
- Per page, single-threaded: `RAYON_NUM_THREADS=1`,
  `PDF_RENDERER_JPX_CONCURRENCY=1`, `PDF_RENDERER_IMAGE_ROW_PAR=0`, MuPDF
  one thread, hayro as is. Cold (fresh open, first render) and warm (second
  render of the same page) are separate series. 5 warmups, 20 timed samples,
  report p50/p90/p99 in ms.
- Whole document, parallel: lege through the scheduler at 1, 2, 4, 8, N
  workers; hayro over `rayon::par_iter` on pages if its `Pdf` is `Sync` (check
  at implementation time, else sequential); PDFium sequential; MuPDF
  per-thread contexts. Report pages/s and peak RSS.
- **Correctness gate.** Every timed page is compared with the PDFium raster
  using `pdfium-diff`'s existing `ink Δ` / `gross` / continuous-ink metrics
  (`compare.rs:23-61`). A renderer's page is `ok`, `suspect`, `failed`, or
  `unsupported`. Throughput is reported twice: over all pages, and over the
  pages **every** renderer got `ok`. A blank raster is never fast.
- Machine capture: `lscpu`, governor, `rustc -Vv`, `RUSTFLAGS`,
  `target-cpu`, every renderer's version and the lege commit, written to
  `machine.json` beside `pages.csv` and `summary.md`.
- Repeatability: refuse to run with load average above 1.0 or with another
  bench process alive; pin to a core set.

**Output.** `bench/results/<date>-<host>/` with the three files above;
`summary.md` is generated and contains the exact table the README embeds.

### 9.3 What the README shows

One table per corpus class, five columns, plus a link to the full report:

```text
Scanned books (JPEG), 144 DPI, single thread       p50 ms/page   pages/s (8 threads)   pages ok
lege-pdf 0.4.0                                          …               …                 …/…
hayro 0.9.0                                             …               …                 …/…
PDFium (pdfium-binaries …)                              …               …                 …/…
MuPDF 1.2x                                              …               …                 …/…
```

Followed by one sentence naming the machine and date, and one sentence
stating the correctness gate. No "N× faster" claim appears anywhere unless
the table supports it on the `pages ok` subset.

Effort: the harness and corpus curation are 1–2 weeks; running and writing
the report is two days. It runs against the crates as they exist today, so it
starts first.

### 9.4 Text extraction (optional second table)

Same corpus, `lege-pdf` vs PDFium vs MuPDF (hayro has no text API;
pdf-toolkit's `text` feature can be included). Metric: ms/page and a
character-level agreement with PDFium from `pdfium-text-diff`. Worth doing
because text is the second thing a viewer needs and PDFium parity on it is a
real differentiator.

---

## 10. Documentation deliverables

- `lege-pdf/README.md` (the crates.io README, also `#![doc = include_str!]`):
  one paragraph of what it is, the five-line example, the feature table, the
  benchmark table from §9.3, the guarantees (never-panic boundary,
  determinism, memory budgets, cancellation, no global state), the
  verification paragraph (differential sweeps against PDFium and MuPDF,
  fuzzing, chaos gate, test counts), the licence line, and a short "not
  included" list: no editing, no GPU yet, no forms, no JavaScript.
- `render/README.md`: updated in this session (intro kept, roadmap replaced
  with the engine's coverage, guarantees, verification, performance, and
  limits).
- `docs.rs` metadata: `all-features` except `print` on non-Windows targets,
  `rustdoc-args = ["--cfg", "docsrs"]`.
- `CHANGELOG.md` from 0.1.0; `SECURITY.md` pointing at the fuzz targets and
  the never-panic contract.
- Three examples that compile outside the workspace: render a page to PNG,
  extract words with boxes, render a whole document in parallel.

---

## 11. Release mechanics

1. `cargo package -p lege-pdf --list` and inspect; `exclude` the truth
   fixture and anything over 100 KB that is not needed at build time.
2. `cargo publish --dry-run` on Linux, Windows, macOS.
3. **The archive test:** unpack the `.crate` into an empty directory with no
   `[patch]`, no sibling checkouts, and build `cargo build --all-features`,
   `cargo test`, `cargo doc --no-deps`, then build the three examples from a
   fresh `cargo new` that depends on the unpacked path. This is what catches
   the sibling-checkout dependencies the ecosystem workspace tolerates.
4. MSRV job at 1.97; clippy with the workspace denies; `cargo deny` for
   licences and duplicate versions.
5. Version 0.4.0 (already set in the workspace), then `lege-pdf-write` 0.1.0 after §7.
6. The in-repo consumers (`lege-process`, `lege-document-ocr`,
   `lege-render-ffi`, `agent`, `pdf-integrity`, the Liminaldoc viewer) switch
   to `lege-pdf = { path = "../lege-pdf", features = [...] }` in the same
   change as the consolidation, so the monorepo never has two copies of the
   engine.

---

## 12. Phases and gates

| phase | work | effort | exit gate |
|---|---|---|---|
| 0 | decisions in §13 | 1 day | licence and split recorded |
| 1 | benchmark harness + corpus (§9), run against today's crates | 1–2 weeks | `summary.md` exists with all six renderers and the correctness gate |
| 2 | consolidation into `lege-pdf` (§4), feature flags, lege-gpu split (§5), layering test, in-repo consumers migrated | 1–2 weeks | all 1,200+ tests green in the consolidated layout; ecosystem builds; archive test passes |
| 3 | facade API (§6.2), examples, rustdoc | 1 week | the three examples compile from a fresh `cargo new`; docs build with no warnings |
| 4 | print fixes (§8.2), `print` feature wired, Windows queue run or "experimental" label | 3–5 days | print tests green; one real print on Linux |
| 5 | README with benchmark table, THIRD-PARTY-NOTICES, LICENSE, CHANGELOG, docs.rs metadata, CI jobs (§11) | 3–5 days | dry-run clean on three OSes |
| 6 | `cargo publish lege-pdf 0.4.0` | — | |
| 7 | `lege-pdf-write` fixes (§7, items 1–8, 11–12), publish 0.1.0 | 3 days | qpdf/pypdf round-trip test green |
| 8 | 0.2: impose-to-PDF (§8.3), balanced page tree and dedup in write, codec slimming (§6.4), text benchmark (§9.4) | 4–6 weeks | |

Six to eight weeks of focused work to 0.1.0 of both crates, with phase 1
and phase 2 parallelisable.

---

## 13. Decisions that are yours

1. **Licence** (§6.1). This is the one that changes what publishing means.
2. **Consolidate (A) vs rename (B)** (§4). I recommend A.
3. **Split write out** (§3). I recommend yes.
4. **Default `print` on or off.** I recommend off (the `windows` dependency
   and the untested Windows spooler argue for opt-in).
5. **Foxit faces as the bundled standard 14** or an `embed-fonts`-style
   feature with Liberation/URW. Foxit keeps PDFium parity, which is the
   whole verification story; the licence basis is PDFium's BSD-3 and Chrome
   ships them, so I would keep them and say so.
6. **Whether phase 1 blocks the README's speed claims.** I say it does.

---

## 14. Devil's advocacy, and where hayro is ahead

### 14.1 Reasons not to do this

- **A public crate is a maintenance promise.** Issues, semver, MSRV, a
  changelog, responses to "it renders my PDF wrong". Hayro's author is a
  known maintainer in the Rust graphics ecosystem (krilla, resvg work); the
  trust that earns is part of what you called inertia. A crate published and
  then left for six months is a worse signal than no crate.
- **Under AGPL the gauntlet mostly will not engage.** Hayro's 4.6 M downloads
  are overwhelmingly Typst's (`typst-svg` and `typst-render` are its top
  dependents). Typst is Apache-2.0 and will never take an AGPL renderer. If
  you keep AGPL you are competing with MuPDF for applications and services,
  which is a legitimate market, but it is not the one hayro's numbers come
  from.
- **The speed claim is unverified** and hayro just shipped glyph caching,
  hinting, and SIMD-always. If the README says 5× and a reader measures 1.5×
  on text pages, the whole crate loses credibility. Benchmark first.
- **The public API does not exist yet.** 0.1 will be followed by breaking
  0.2s; every one costs early adopters.
- **Font and data provenance will be read by strangers.** The Foxit faces
  ride on PDFium's BSD-3 LICENSE, which does not mention fonts; the renderer
  describes itself as "a semantic port of PDFium's behaviour". Both are fine
  and both need a sentence in THIRD-PARTY-NOTICES so nobody has to guess.
- **It is six to eight weeks** that are not spent on renderer refinement or
  on the processor. The counter-argument is that publication forces the
  hygiene (self-contained build, stable facade, single-pass compile,
  benchmark) that the viewer and the agent tool need anyway.
- **Windows printing has never been run.** Shipping it as a default feature
  invites the first bug report to be "it printed half a job".

### 14.2 Where hayro is ahead today (checked on 2026-11-03)

| area | hayro 0.9.0 | lege-pdf |
|---|---|---|
| licence and reach | Apache-2.0 OR MIT; 63 dependents; Typst | AGPL; zero dependents |
| crates.io presence | 11 crates, releases roughly monthly, changelog, MSRV 1.92 | none |
| API simplicity | `Pdf::new(bytes)`, `hayro::render(page, cache, settings)`; `render_into` an existing `vello_cpu` context | facade pending (§6.2) |
| portability | compiles to WASM (web demo); `hayro-syntax` is a lightweight reader | not tested on wasm; `memmap2`, `rayon`, `tempfile` via jp2lam make it unlikely without features |
| outputs | SVG (`hayro-svg`); PDF-to-PDF flattening (`hayro-write`, what Typst uses to embed PDFs) | raster only; no SVG; no PDF-to-PDF |
| reusable parts | `hayro-font`, `hayro-cmap`, `hayro-postscript` (a scanner for Type 4 functions and CMaps, **not** printing), `hayro-jbig2`, `hayro-jpeg2000`, `hayro-ccitt` | jp2lam and jbig2enc-rust are separate crates; fonts, CMaps, and CCITT are internal |
| no-unsafe | `#![forbid(unsafe_code)]` everywhere | 36 unsafe sites (AVX2/SSE kernels, mmap, uninit buffers), documented |
| community | issues, contributors, a web demo | one maintainer plus agents |

PostScript printing support is not one of them: `hayro-postscript` is a token
scanner for PostScript calculator functions, and hayro has no printing at all.

### 14.3 Where lege-pdf is ahead (checked the same way)

Encryption (R2–R6, user and owner passwords; hayro's docs still list
encrypted files as unsupported at 0.9.0), knockout groups and blend modes on
images/shadings/tilings (hayro lists knockout, blending/isolation, and colour
key masking as unsupported), optional content, annotation appearance streams
with synthesized markup fallbacks, mesh shadings 1 and 4–7, a parallel
scheduler with memory permits, cancellation inside long fills, per-codec
input/output/pixel budgets, a never-panic boundary with a mutation gate,
native Type 1 and bare CFF, system-font substitution with PDFium's CJK
preference lists, text extraction with PDFium-parity geometry, a 19,443-file
two-oracle differential sweep, printing, and (unverified against hayro)
speed. The JPX and JBIG2 decoders are yours to compare in the same benchmark.

---

## 15. Appendix: standalone test run

`cargo test --workspace` in the scratch workspace of §2.4 (render crates minus
`pdf-render-wgpu` and `pdf-cli`, plus `read`, `write`, `print`, with every
dependency other than the two codecs fetched from crates.io; the codecs by
path with `jpxl-perceptual` from the registry), Rust 1.97.1, Linux x86_64,
2026-11-03:

| | |
|---|---|
| test binaries (unit, integration, doc) | 116 |
| passed | 1,167 |
| failed | 0 |
| ignored | 0 |

Longest suites: `pdf-render-cpu` integration (20.6 s), the JPX fixture suite
(7.6 s), `pdf-content` image integration (7.3 s). The run needed no corpus
mount: every test that depends on external corpora already skips or is
confined to the tools. This is the baseline the consolidation in phase 2 must
reproduce exactly.

`cargo tree -p lege-pdf-read -e normal` in the same workspace: 85 unique
crates (§4.3).
