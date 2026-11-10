# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

Lege is a fully-automatic PDF processor for E-Ink ebook readers. It renders PDF pages, runs YOLO layout detection to classify regions (text, image, table, noise), then re-encodes each page with region-aware compression (JBIG2 for text, JPEG2000/JPEG for images) and writes a new PDF or DJVU file. There is both a CLI (`lege`) and a GUI (`lege-gui`) binary.

## Workspace layout

| Crate | Path | Purpose |
|---|---|---|
| Ecosystem workspace | `Cargo.toml` (root) | Virtual workspace, shared dependencies/profiles/patches, and build aliases |
| `lege` | `lege-process/` | CLI binary + core processing library |
| Process crate roots | `lege-process/core/{lib,main}.rs` | Rust library and binary roots |
| Process modules | `lege-process/{pipeline,encoding,color,colorquant,ocr,reflow,resize}/` | Major processing subsystems wired from `core/lib.rs` with `#[path]` |
| `lege-gpu` | `lege-gpu/` | wgpu GPU compute: binarization, resize, custom ONNX runtime |
| `lege-ocr` | `lege-ocr/` | Shared OCR library + `lege-ocr-debug` |
| `lege-pdf-write` | `lege-pdf/write/` | Typed append-only PDF writer |
| `lege-render-wgpu` | `lege-render-wgpu/` | wgpu image renderer + GPU postprocess executor over exprim (needs `lege-gpu`) |
| `exprim` | `../exprim/` (sibling repo) | PDF engine: reading, text, rendering, printing (`print` feature); the `exprim` agent CLI/MCP server is `../exprim/agent` |
| `jbig2enc-rust` | `../exprim/codecs/jbig2enc-rust/` | JBIG2 encoder/decoder (patched into `lege`) |
| `jp2lam` | `../exprim/codecs/jp2lam/` | JPEG2000 codec (patched into `lege`) |
| `djvu_encoder` | `../exprim/codecs/djvulibrust/` | AGPL DjVu library linked by Lege; standalone codec CLI retained |
| `lege-gui-liminalui` | `lege-process/GUI/liminalUI/` | liminalUI GUI frontend; produces bin `lege-gui` (the only frontend) |
| Liminaldoc reader and writer | `../liminaldoc/` | Separate native document product; old source copies remain here during migration |
| Shared assets/tools | `lege-misc/` | Icons, packaging, scripts, docs, dev utilities, and `xtask` |

The only workspace default member is `lege-process`. Its default `gui`
feature depends on `lege-gui-liminalui` and adds a `lege-gui` binary target, so
plain `cargo build` (or `cargo gui`, i.e. `cargo build -p lege`) builds both
the CLI and the GUI; `cargo cli` builds `--bin lege` alone.
The PDF engine (`exprim`) and the codecs live in the sibling `../exprim`
repository, which must be checked out beside this one; `exprim` is a path
dependency, and root patches make the processor use the sibling codec sources.
Standalone codec projects retain their own build graphs.

Never select `-p lege` and `-p lege-gui-liminalui` in one invocation: both
packages have a `lege-gui` binary, and their outputs collide in `target/`.
Build the GUI through `lege` (as above), or `-p lege-gui-liminalui` on its own.
Liminaldoc has its own Cargo workspace:

| Binary | Package | Path |
|---|---|---|
| `lege` | `lege` | `lege-process/` |
| `lege-gui` | `lege` (default `gui` feature), or `lege-gui-liminalui` alone | `lege-process/GUI/liminalUI/` |
| `liminaldoc-reader` | `liminaldoc-reader` | `../liminaldoc/crates/liminal-reader/` |
| `liminaldoc-writer` | `docwrite-app` | `../liminaldoc/crates/docwrite-app/` |

## Build commands

```sh
# Frontend aliases in .cargo/config.toml:
cargo cli                 # CLI only          -> lege
cargo gui                 # CLI + liminalUI GUI -> lege, lege-gui   (the default)
cargo viewer              # Liminaldoc reader -> liminaldoc-reader
cargo liminaldoc          # all Liminaldoc binaries

# Standard release build (CLI + liminalUI GUI, via workspace default-members)
cargo build --release

# Development build (incremental, 256 codegen units)
cargo build

# Fast release-optimized build for iteration/troubleshooting (PREFER THIS over
# --release when testing changes): the debug-fast profile inherits release but
# disables fat LTO, which alone takes up to ~15 minutes on Windows.
# Binary lands in target/debug-fast/.
cargo build --profile debug-fast -p lege --bin lege

# Debug-logging build (perf tracing, no LTO) — use alias from .cargo/config.toml
cargo build-debug-logging          # alias for: cargo build --profile debug-fast --features debug-logging
cargo run-debug-logging -- <args>  # alias for: cargo run --profile debug-fast --features debug-logging -- <args>

# Run CLI in dev mode
cargo run -- <pdf-file>

# Linux .deb package
cargo deb

# Single crate build
cargo build -p lege-gpu
```

Normal root Cargo commands already use the in-tree `jp2lam` and
`jbig2enc-rust` sources through root `[patch]` entries. The compatibility helper
is now `lege-misc/scripts/cargo-local.sh`.

## Important 

Do not treat repo-wide `cargo fmt` output as unwanted churn. If `cargo fmt` modifies files outside the files directly edited by the agent, keep those formatting changes unless the user explicitly asks for a minimal diff.

Never run `git checkout --`, `git restore`, `git reset`, or any other command that discards uncommitted changes unless the user explicitly requests it. If the working tree contains unexpected modified files, report them in the final summary instead of reverting them.

Before any destructive git operation, stop and ask. The only exception is deleting temporary files created by the agent itself, such as logs under `/tmp`.

## Running tests

```sh
# All workspace tests
cargo test

# Tests for a specific crate
cargo test --manifest-path ../exprim/codecs/djvulibrust/Cargo.toml
cargo test -p lege

# Single test by name
cargo test --manifest-path ../exprim/codecs/djvulibrust/Cargo.toml jb2_pipeline

# Tests with debug-logging output visible
cargo test --features debug-logging -- --nocapture
```

## Features

| Feature | Effect |
|---|---|
| `debug-logging` | Enables `perf_log!`, `info_log!`, `warn_log!` macros; activates per-region timing |
| `jp2-lam` (default) | Enables JPEG2000 encoding via the `jp2lam` library |
| `profiling` | Inherits `debug-logging`; use with `--profile profiling` for flamegraph-compatible builds |
| `debug-layers` | Propagated to `lege-gpu` for wgpu validation layer diagnostics |

## Architecture: PDF processing pipeline

The main processing flow lives in `lege-process/pipeline/`. The two concrete pipeline variants are:

- **`pdf_tokio_pipeline.rs`** — PDF → PDF pipeline (Tokio async, parallel stages)
- **`djvu_pipeline.rs`** — PDF → DJVU pipeline

Both pipelines share the same stage structure:

```
PdfPageSource (render pages at high-res + 1024×1024 inference size)
  → inference_stage_parallel (YOLO layout detection via InferenceActor)
  → page processing (classify_page, region masking, binarization, encoding)
  → spawn_pdf_writer_actor (assembles output PDF/DJVU with hOCR text layer)
```

Key files per concern:

| Concern | File |
|---|---|
| Pipeline orchestration | `lege-process/pipeline/pdf_tokio_pipeline.rs`, `djvu_pipeline.rs` |
| Config / shared types | `lege-process/pipeline/config.rs` |
| YOLO inference actor (batching) | `lege-process/pipeline/inference.rs` |
| Page classification & blank detection | `lege-process/pipeline/page_analysis.rs` |
| Region/resize policies | `lege-process/pipeline/policies.rs` |
| Margin analysis | `lege-process/core/margin.rs` |
| Page rendering | `lege-process/core/pagerender.rs` |
| Content categories | `lege-process/core/types.rs` — `ContentCategory` drives downstream decisions |

## Layout detection

`lege-process/core/engine.rs` is a thin `include!("engine_impl.rs")` redirect to
`lege-process/core/engine_impl.rs` (the `LayoutEngine` wrapping
`lege_gpu::vision::LayoutDetector`, PP-DocLayout PicoDet on wgpu).

`ContentCategory` (in `lege-process/core/types.rs`) is the single source of truth for what a detection means downstream — never use raw layout class IDs outside of `types.rs`.

## GPU backend (`lege-gpu`)

- **`vision/`** — Custom ONNX runtime: loads `.onnx` protobuf, folds constants, lowers ops to wgpu compute shaders. No `ort`/`tract` dependency.
- **`binarization/`** — wgpu Sauvola binarization kernel.
- **`resize/`** — wgpu image resize (letterbox + direct).

WGSL shaders are either embedded at compile time (`include_str!`) or, with the `no-include-shaders` feature, loaded at runtime.

## Encoding

`lege-process/encoding/streamline.rs` contains the single dispatch point for all
encoders. Choose an encoder with the `EncodingSettings` variants `Jpeg`,
`Jp2Lam`, `Jbig2`, and `Ccitt4`.

JBIG2 has two modes (`Jbig2Mode`): `Symbol` (symbol-substitution, best for clean text) and `Generic` (bitplane, required when `ContentCategory::Abandon` is present on a page).

## DjVu encoding

Lege links the AGPL `djvu_encoder` crate in `djvulibrust/` and builds typed
`Page` values directly in `lege-process/core/djvu.rs`. Page compression runs in
the existing parallel encode stage; the writer actor inserts encoded pages in
order, adds navigation, and atomically publishes the final document. There are
no manifest/page staging files and no runtime helper discovery.

- Build the encoder: `cargo build --manifest-path ../exprim/codecs/djvulibrust/Cargo.toml --features cli --bin djvu-encoder`
  (behind the `cli` feature; the library build stays lean). This independent
  diagnostic/user tool has a simple
  standalone mode: `djvu-encoder encode PAGE.png… -o out.djvu [--photo|--bilevel]`.

## Runtime assets

ONNX models are found at runtime via `runtime_asset_path` /
`runtime_asset_path_if_exists` in `lege-process/pipeline/config.rs`. Search order: exe
directory → `models/` subdirectory → `LEGE_DATA_DIR` / `LEGE_ASSET_DIR` env
vars → `/usr/share/lege/models` (Linux). Models required:
`yolo-layout.onnx`, `sauvola.onnx`.

## Platform notes

- **OCR backend**: on **Windows**, OCR (fast and best modes) is the system
  WinRT OCR (`Windows.Media.Ocr`) through `lege_ocr::WinOcrEngine`
  (`lege-ocr/src/engine.rs`; `default_engine()` returns it on Windows). Do not
  switch Windows to PP-OCR. Elsewhere the embedded PP-OCR (`paddle-ocr`, GPU,
  pure-Rust, via `lege-ocr`) is the default and needs no external binary or tessdata. The
  build script derives `cfg(lege_paddle_ocr)` for it on Linux/macOS (and
  Android with the `android` feature). `tesseract-ocr` remains an opt-in
  legacy backend on Linux/macOS only.
- **Windows**: Vulkan wgpu backend (Direct3D 12 was dropped on 2026-11-08:
  Vulkan matched or beat it on every Lege workload measured, and only Vulkan
  reaches tensor cores; see `lege-gpu/src/wgpu_setup.rs`), Windows API for
  system dirs. No DXC/ATL build requirement any more. WinRT OCR moved from
  `lege-process/ocr/winocr.rs` into `lege-ocr` in June 2026 (it was not
  removed); `lege-process/ocr/mod.rs` keeps only a `windows_ocr` availability
  stub because there is no Tesseract on Windows.
  Consequently `lege`'s `windows` crate features are only what it actually
  calls — `Win32_Foundation`, `Win32_System_Console` (console control in
  `core/main.rs` / `core/progress.rs`) and `Win32_System_SystemInformation`
  (`GlobalMemoryStatusEx` in `pipeline/helper_functions.rs`). Do not re-add
  `Media_Ocr`/`Graphics_Imaging`/Direct3D feature flags: wgpu requests its own.
- **Linux/macOS**: Vulkan/Metal wgpu backend.
- **Tensor cores**: on Vulkan adapters with cooperative matrices (NVIDIA RTX),
  lege-gpu runs MatMul/Gemm on tensor cores (f16 operands, f32 accumulation);
  `LEGE_GEMM_COOP=0` disables it, `LEGE_GEMM_KERNEL=tiled` selects the legacy
  f32 kernel.
- `lege-process/core/windows_dirs.rs` / `app_dirs.rs` — platform-specific config/data directory resolution.

## Desktop GUI

The only processor GUI is liminalUI (`lege-process/GUI/liminalUI`, package `lege-gui-liminalui`, binary `lege-gui`). It does not link Freya or Skia; the Freya GUI and the vendored `freya-main` tree have been removed. The GUI embeds Inter from `lege-process/GUI/liminalUI/fonts/` at compile time. File dialogs use the vendored `rfd` crate at `lege-process/GUI/rfd`.

## Debug tracing

- `LEGE_BBOX_TRACE=1` — prints bbox/region/placement lines to stderr.
- `LEGE_RAYON_THREADS=N` — override Rayon thread count (default: CPU count − 1).
- `perf_log!(start, "label")` — zero-cost timing macro; only active with `debug-logging` feature.
