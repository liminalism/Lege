# jbig2enc-rust

The canonical source is the [Lege monorepo](https://github.com/liminalism/lege/tree/main/lege-codecs/jbig2enc-rust).

A JBIG2 encoder and decoder in Rust for bilevel (black-and-white) images —
scanned documents, PDF-embedded streams, and standalone JBIG2 files.

The crate is split into an `encode` half and a `decode` half over a shared
protocol core. Both halves are independent Cargo features; a consumer can build
either alone. The 0.6 release candidate includes the DjVu-code replacement described in
[PROVENANCE.md](PROVENANCE.md).

## 0.6.0 release notes (unreleased)

Since 0.5.4: faster byte-preserving generic encoding; explicit exact symbol
presets; measured compression planning, residual packing and PDF dictionary
policies; bounded PDF groups; refinement interoperability and opt-in canonical
termination fixes. The DjVu-derived component code/tests are replaced, with
MIT project contributions and retained Apache-2.0 arithmetic notices.
New configuration fields affect exhaustive struct literals; existing constructors
remain. The full [changelog](https://github.com/liminalism/lege/blob/main/lege-codecs/jbig2enc-rust/CHANGELOG.md)
records compatibility and validation details.

## Encoder

Encoding choices:

- **`generic`** — pages encoded as generic JBIG2 regions, no symbol
  dictionary. Pixel-exact with respect to the input bitmap; often larger on
  repeated text.
- **`symbol`** — connected components are extracted, repeated glyphs are
  grouped into a symbol dictionary, and page instances reference dictionary
  entries. Component selection can omit marks, so it has no input-pixel
  equality contract. Often much smaller than `generic` on text-heavy scans.
- **`sym_unify`** — the symbol pipeline plus a family-unification pass that
  merges symbol variants across pages (scanning noise, border variation,
  page-to-page drift) using exact dictionary-entry byte accounting and
  planner-side anchor remapping. Intentionally permits substitution and is
  slower than plain `symbol`.
- **`text_lossless`** — retains every supplied bilevel pixel, including isolated
  marks. Reuses identical component bitmaps and stores others separately or
  generically. This is the explicit exact symbol preset.
- **`text_lossless_max_compression`** — slower exact encoding that measures
  complete dictionary/refinement/generic candidate streams under bounded trial
  and matching budgets. Family refinement is accepted only for a byte saving.
- **`text_symbol_unify_safe`** — legacy lossy unification with final topology
  checks that force refinement for hole, connectivity or endpoint changes.
- **`text_lossy_max_compression(limit)`** — explicitly lossy measured planning;
  bounded same-size substitution competes with exact storage and refinement.
  `limit` is the per-instance pixel-error cap. Topology checks do not prove
  character identity; this mode reports actual source differences.

Mode selection is by config preset:

```rust
use jbig2enc_rust::Jbig2Config;

let generic   = Jbig2Config::lossless();
let symbol    = Jbig2Config::text();
let sym_unify = Jbig2Config::text_symbol_unify();
let exact     = Jbig2Config::text_lossless();
let compact   = Jbig2Config::text_lossless_max_compression();
let lossy     = Jbig2Config::text_lossy_max_compression(1);
```

For bounded book processing, `PdfGroupEncoder` emits each group's globals and
page streams together and releases retained input before calling the output
sink. `PdfGroupOptions` bounds pages and optionally source bitmap bytes.
`decoder_resource_limits` accepts application-measured dictionary/work caps;
the measured presets choose the smallest feasible candidate. No device budget
is assumed. See [contracts, resource definitions and benchmark commands](docs/QUALITY.md).

### Measurements

The 2026-11-02 hardware-profiled follow-up cuts generic-kernel time by a
further 50% on mixed17 and 42% on scanned text350, with exact byte equality.
Median new/C ratios are 0.467 and 0.561; procedural noise remains about 7%
slower than C. See [scope, variation, counters and raw measurements](docs/GENERIC-PERF-FOLLOWUP.md).

The 2026-11-01 generic-kernel optimization reaches C parity: median paired
Rust/C time ratios are 0.943 on 17 mixed pages and 0.973 on 350 scanned-book
pages, with every raw payload unchanged. This measures template-0 coding of
preloaded packed pixels, including coder initialization/finalization; see
[method, variation and raw results](docs/GENERIC-SPEED.md).

Historical measurements before this optimization follow. These include different
pipeline scopes and corpora and are retained as historical results.

In-process 350-page comparison against the original C `jbig2enc` (same
preloaded PBM page set, release builds both sides, page preparation outside
the timed region), corpus `sahib2/350p`:

- `c generic`: 5228.2 KB in 1.15 s
- `rust generic`: 5232.2 KB in 2.47 s
- `c symbol`: 2566.5 KB in 24.58 s
- `rust symbol`: 2227.1 KB in 6.05 s
- `rust sym_unify`: 2025.9 KB in 16.93 s

Relative: Rust `generic` is size-parity with the C encoder; Rust `symbol` is
13.2% smaller and about 4.1× faster than C symbol mode; `sym_unify` is 21.1%
smaller than C symbol mode. Within the Rust encoder, `symbol` is 57.4% smaller
than `generic`, and `sym_unify` is 61.3% smaller than `generic` (9.0% smaller
than `symbol`).

A `confed/10p` release benchmark after the 0.5.3 portable-SIMD comparator
change (`wide`-based `u32x8` kernels in symbol matching, scalar early-exit
preserved):

- `symbol`: 175.3 KB in 0.57 s
- `sym_unify`: 145.0 KB in 2.47 s (17.3% smaller than `symbol`)

Known encoder limits: the symbol modes leave more material in generic residual
regions than an ideal long-book encoder would. Per-instance text refinement
encoding exists, but dictionary-level refinement planning is not enabled in
the legacy production path. `text()` and `text_symbol_unify()` do not promise
pixel-exact output; even small substitutions can change text. Use the explicit
exact presets when equality to the supplied bilevel input is required.

The 0.7 work in progress adds opt-in measured residual packing and a reproducible
fidelity/performance harness. See [measurement contracts](docs/QUALITY.md).

## Decoder

A full-featured JBIG2 decoder covering both embedded (PDF `/JBIG2Decode`,
globals stream + page stream) and standalone file organisation:

- Generic regions: arithmetic templates 0–3, TPGDON, and MMR.
- Symbol dictionaries and text regions: arithmetic and Huffman variants,
  including refinement/aggregate coding and custom Huffman tables.
- Refinement regions, pattern dictionaries, and halftone regions.
- Striped pages, intermediate regions, and segment retention handling.
- `DecodeStrictness::Compatible` accepts the spec deviations real-world
  encoders produce and records each recovery as a typed `RecoveryEvent`;
  `Strict` rejects them. Inputs outside the supported surface return typed
  `Unsupported` errors — the decoder does not panic on malformed input.
- `decode_embedded_into` decodes into caller-owned buffers through a pooled
  `DecoderContext` (per-worker context, shared immutable `DecodedGlobals`),
  for zero-allocation steady state in multi-page workloads. Output is packed
  1-bit rows.

Decode benchmarks (release, single thread): A4 generic page ~31 ms (parity
with `jbig2dec` 0.20), symbol page ~4.4 ms, halftone page ~20 ms. `jbig2dec`
is used as the differential oracle in the test suite; some validation tools
expect it in `PATH`.

## Usage

```toml
[dependencies]
jbig2enc-rust = "0.6.0"            # encoder + decoder + symboldict (defaults)
```

Decoder only:

```toml
[dependencies.jbig2enc-rust]
version = "0.6.0"
default-features = false
features = ["decode"]
```

Encoding a single page:

```rust
use jbig2enc_rust::{Jbig2Config, encode_single_image};

let input = vec![0, 1, 0, 1, 1, 0, 1, 0];
let (width, height) = (2, 4);
let result = encode_single_image(&input, width, height, false)?;
```

PDF split output (global dictionary and page-local data separated for
embedding):

```rust
use jbig2enc_rust::{Jbig2Context, encode_single_image};

let _ctx = Jbig2Context::with_pdf_mode(true);
let result = encode_single_image(&input, width, height, true)?;
// result.global_data — global dictionary segments, if any
// result.page_data   — page-local segments
```

## Cargo features

- `encode` — the encoder half.
- `decode` — the decoder half. Either half builds without the other.
- `symboldict` — symbol-dictionary encoding (implies `encode`; on by default).
- `parallel` — Rayon-based parallel helpers where available.
- `tracing`, `trace_encoder`, `trace_arith` — logging/tracing hooks.
- `line_verify` — line verification helpers.
- `refine` — refinement-related configuration paths.
- `profiling` — profiling support.
- `halftone_bin` — image support for the halftone tooling binaries.

## Layout

```
src/
  encode/   encoder pipeline: api, cc (connected components), classify,
            comparator, simd, cost, unify planning, halftone, document assembly
  decode/   decoder: segment/page parsing, generic/text/halftone regions,
            symbol dictionaries, huffman, mmr, refinement, arith, context pooling
  shared/   protocol core shared by both halves
```

## Testing

```bash
cargo test                                   # full suite
cargo test --no-default-features --features decode   # decoder half alone
cargo test --test comparator                 # symbol comparator
cargo test --test perf_smoke -- --ignored --nocapture
```

The default byte-identical guard uses original generated PBMs and a separate
40-stream historical-encoder baseline. The old scan inputs are excluded from
the package; their original manifest is preserved and its explicit legacy gate
still passes. See [fixture provenance and both test commands](tests/fixtures/README.md).

## License

Project contributions are MIT. Retained arithmetic-coder files derived from
jbig2enc remain Apache-2.0, with their license and Google/Adam Langley notices.
Cargo describes this permissive mix as `MIT AND Apache-2.0`. The translated
DjVuLibre component code and inherited tests have been replaced; the GPL
licensing blocker is closed. See [source provenance](PROVENANCE.md).

## References

- ISO/IEC 14492:2001 (JBIG2)
- Original `jbig2enc`: https://github.com/agl/jbig2enc
- `djvulibre`: https://github.com/djvuzone/djvulibre
- Leptonica's JBIG2 classifier notes
- M. Valliappan, B. L. Evans, D. A. D. Tompkins, F. Kossentini, "Lossy
  Compression of Stochastic Halftones with JBIG2" (informed the halftone path)
- Symbol-dictionary design literature consulted for the `sym_unify` cost model:
  Ye/Schilling/Cosman/Koy, "Symbol Dictionary Design for the JBIG2 Standard";
  Ye/Cosman, "Fast and Memory Efficient JBIG2 Encoder"; Figuera/Yi/Bouman,
  "A New Approach to JBIG2 Binary Image Compression";
  Guo/Depalov/Bauer/Bradburn/Allebach/Bouman, "Binary Image Compression Using
  Conditional Entropy-Based Dictionary Design and Indexing"
