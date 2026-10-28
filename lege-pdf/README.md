# lege-pdf

PDF reading, rendering, writing, printing, and agent access for the Lege workspace. The processor uses `read` and `write`. The sibling Liminaldoc reader uses the renderer crates directly. Each part has its own crate and README.

```text
lege-pdf/
├── read/            lege-pdf-read    intake, text, outlines, render sessions
├── write/           lege-pdf-write   typed append-only PDF emitter
├── render/          pdf-*            native document engine and CPU rasterizer
├── print/           lege-pdf-print   imposition, sheet composition, spooling
├── agent/           lege-pdf         structured CLI and local MCP server
└── pdf-integrity/   own workspace    read-only forensic triage
```

## read

`lege-pdf-read` is the owned seam over the renderer. Callers examine a document, pull metadata, outlines, and text evidence, and render pages or regions without naming renderer types. [read/README.md](read/README.md).

## write

`lege-pdf-write` accepts already-encoded page artifacts in any order and emits a PDF: JPEG, JPEG 2000, JBIG2, CCITT Group 4, and indexed images, plus an invisible text layer, outlines, and metadata. It does not parse or edit an existing PDF. [write/README.md](write/README.md).

## render

The `pdf-*` crates are a read-only, memory-bounded, concurrent PDF engine: immutable document snapshot, worker-local execution, and a compiled-page IR drawn by the CPU rasterizer. Experimental WGPU image rendering lives in `pdf-render-wgpu`. [render/README.md](render/README.md).

## print

`lege-pdf-print` sits above `read`. A job either spools the original PDF bytes or imposes sheets (N-up, booklet, margins, duplex) and spools rasters. Windows always takes the raster route. Prepress (CMYK separations, PDF/X) is outside this crate. [print/README.md](print/README.md).

## agent

Package `lege-pdf-agent`, binary `lege-pdf`. Inspect, text, images, content, render, search, print, and a persistent stdio service, with records on stdout. [agent/README.md](agent/README.md).

```sh
cargo lege-pdf
cargo pdf-tool-run -- --help
```

## pdf-integrity

A nested Cargo workspace, excluded from the root workspace, licensed Apache-2.0. `pdf-forensics` is the report model. `lege-pdf-audit` is the read-only `inspect` CLI. [pdf-integrity/CLAUDE.md](pdf-integrity/CLAUDE.md) points at its ledger. [pdf-integrity/README.md](pdf-integrity/README.md).

## Build

The root workspace already members `read`, `write`, `print`, `agent`, and `render/crates/*`.

```sh
cargo pdf-write
cargo lege-pdf
cargo check -p lege-pdf-read -p lege-pdf-print
```

`pdf-integrity` builds from its own manifest:

```sh
cargo check --manifest-path lege-pdf/pdf-integrity/Cargo.toml
```

Renderer sweep history and the older write/print plans are in [../dev-notes/lege-pdf](../dev-notes/lege-pdf).
