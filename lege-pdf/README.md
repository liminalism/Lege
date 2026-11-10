# lege-pdf

PDF writing and forensic triage for the Lege workspace. Reading, text,
rendering, printing and the agent CLI moved to the sibling **exprim**
repository (`../exprim`, crate `exprim`), which Lege depends on by path.

```text
lege-pdf/
├── write/           lege-pdf-write   typed append-only PDF emitter
└── pdf-integrity/   own workspace    read-only forensic triage
```

## write

`lege-pdf-write` accepts already-encoded page artifacts in any order and emits a PDF: JPEG, JPEG 2000, JBIG2, CCITT Group 4, and indexed images, plus an invisible text layer, outlines, and metadata. It does not parse or edit an existing PDF. [write/README.md](write/README.md).

## pdf-integrity

A nested Cargo workspace, excluded from the root workspace, licensed Apache-2.0. `pdf-forensics` is the report model. `lege-pdf-audit` is the read-only `inspect` CLI. Both read documents through `exprim`. [pdf-integrity/CLAUDE.md](pdf-integrity/CLAUDE.md) points at its ledger. [pdf-integrity/README.md](pdf-integrity/README.md).

## Where the rest went

| was | now (in `../exprim`) |
|---|---|
| `render/crates/pdf-*` | modules of the `exprim` crate (`pdf-render-cpu` → `exprim::render_cpu`, …) |
| `read/` (`lege-pdf-read`) | the `exprim` crate root (`exprim::RenderSession`, …) |
| `print/` (`lege-pdf-print`) | `exprim::print`, behind the `print` feature |
| `agent/` (`lege-pdf`) | `agent/`, package `exprim-agent`, binary `exprim` |
| `pdf-render-wgpu`, the `pdf-postprocess` GPU executor | stayed in Lege as `../lege-render-wgpu` (they need `lege-gpu`) |

## Build

```sh
cargo pdf-write
cargo check --manifest-path lege-pdf/pdf-integrity/Cargo.toml
cargo pdf-tool        # the exprim agent CLI, from ../exprim/agent
```

The write and pdf-integrity plans are in [../dev-notes/lege-pdf](../dev-notes/lege-pdf).
The renderer's sweep history and the print and agent plans moved to
`../exprim/dev-notes/`.
