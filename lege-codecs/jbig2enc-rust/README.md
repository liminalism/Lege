# jbig2enc-rust

A JBIG2 encoder and decoder in pure Rust for bilevel (black-and-white)
images: scanned documents, PDF-embedded streams and standalone JBIG2 files.
No C library, no system codec, no runtime assets. The same crate also builds
the `jbig2enc` Python wheel.

The canonical source is the
[Lege monorepo](https://github.com/liminalism/lege/tree/main/lege-codecs/jbig2enc-rust).

## 0.6.0 (unreleased)

Since 0.5.4: faster byte-preserving generic encoding; explicit pixel-exact
symbol presets; measured compression planning, residual packing and PDF
dictionary policies; bounded multi-page PDF groups; refinement
interoperability and opt-in canonical arithmetic termination; the Python
binding ships inside the crate. The DjVu-derived component code and tests are
replaced, leaving MIT project contributions plus retained Apache-2.0
arithmetic-coder notices. Existing constructors and entry points remain; new
configuration fields affect exhaustive struct literals only. The
[changelog](https://github.com/liminalism/lege/blob/main/lege-codecs/jbig2enc-rust/CHANGELOG.md)
has the full list.

## What it does

**Encoder**

- Generic regions (arithmetic, template 0) with a packed-word kernel: the
  pixel-exact path for anything.
- Symbol dictionaries and text regions: connected components, glyph matching,
  shared dictionaries across pages, per-instance refinement.
- Explicit presets that say what they keep: pixel-exact symbol coding,
  measured maximum-compression planning (exact or bounded lossy), and the
  legacy fast lossy modes.
- PDF embedding: globals stream plus page streams, three dictionary
  placement policies, bounded page groups with caller resource limits.
- Halftone regions (pattern dictionary plus gray-code planes) for dithered
  images.
- Standalone `.jb2` files (sequential organisation) and embedded fragments.

**Decoder**

- Generic regions: arithmetic templates 0 to 3, TPGDON and MMR.
- Symbol dictionaries and text regions, arithmetic and Huffman, including
  refinement/aggregate coding and custom tables.
- Refinement regions, pattern dictionaries, halftone regions, striped pages,
  intermediate regions, segment retention.
- Embedded (PDF `/JBIG2Decode` with `/JBIG2Globals`) and standalone files.
- Typed errors, explicit resource limits, a strict mode and a compatible mode
  that records every recovery it performs. No panics on malformed input.
- A pooled, allocation-free steady state for multi-page rendering.

**Python**: `pip install jbig2enc` (built from this crate's `python` feature)
gives `encode`, `encode_for_pdf`, `encode_document`, `decode`,
`decode_document` and `decode_pdf_stream` over any buffer-protocol object.

## Quick start (Rust)

```toml
[dependencies]
jbig2enc-rust = "0.6.0"            # encoder + decoder + symbol mode
```

One page, pixel-exact, as a standalone JBIG2 file. Input is one byte per
pixel, row-major, non-zero means black:

```rust
use jbig2enc_rust::encode_single_image_lossless;

let (width, height) = (2550u32, 3300u32);
let pixels: Vec<u8> = vec![0; (width * height) as usize];
let result = encode_single_image_lossless(&pixels, width, height, false)?;
std::fs::write("page.jb2", &result.page_data)?;
```

The same page as a PDF image stream. `global_data` is the `/JBIG2Globals`
object; `page_data` goes in the image stream:

```rust
use jbig2enc_rust::{Jbig2Config, Jbig2Context, encode_single_image_with_config};

let context = Jbig2Context::with_config(Jbig2Config::text_lossless(), true);
let result = encode_single_image_with_config(&pixels, width, height, context)?;
// result.global_data: Option<Vec<u8>>   -> /DecodeParms << /JBIG2Globals N 0 R >>
// result.page_data:   Vec<u8>           -> the image stream
```

A multi-page document with one shared dictionary. Pages encoded separately
and stitched together are *not* decodable; one encoder has to plan them:

```rust
use jbig2enc_rust::{Array2, Jbig2Config, encode_document_pdf_split};

let pages: Vec<Array2<u8>> = load_pages()?;          // (height, width), 1 = black
let output = encode_document_pdf_split(&pages, &Jbig2Config::text_lossless())?;
// output.global_segments: Option<Vec<u8>>, output.page_streams: Vec<Vec<u8>>
```

Long books, bounded memory. `PdfGroupEncoder` emits each group's globals and
page streams to a sink and drops the source pages before calling it:

```rust
use jbig2enc_rust::{Jbig2Config, PdfGroupEncoder, PdfGroupOptions};
use jbig2enc_rust::jbig2sym::BitImage;

let config = Jbig2Config::text_lossless();
let options = PdfGroupOptions { max_pages: 64, max_source_bitmap_bytes: None };
let mut groups = PdfGroupEncoder::new(&config, options, |group| {
    // group.first_page, group.output.global_segments, group.output.page_streams
    write_group_to_pdf(group)
})?;
for image in pages {                                   // BitImage, owned
    groups.push_page(image)?;
}
groups.finish()?;                                      // explicit; drop does not flush
```

Decoding, standalone and embedded:

```rust
use jbig2enc_rust::decode::{DecodeOptions, decode_embedded, decode_file};

let document = decode_file(&std::fs::read("page.jb2")?, &DecodeOptions::default())?;
let page = document.first_page().expect("at least one page");
let black = page.get(10, 20);                          // (x, y) -> bool

let bitmap = decode_embedded(globals.as_deref(), &page_data, &DecodeOptions::default())?;
```

`DecodeOptions::default()` is strict. `DecodeStrictness::Compatible` accepts
the deviations real encoders produce and lists each one as a `RecoveryEvent`.
`DecodeLimits` caps bitmap sizes, pixels and segment counts. For a renderer,
`decode_embedded_into` reuses a caller-owned `MonoBitmap` and a pooled
`DecoderContext`, so same-size pages decode without allocating.

## Choosing a preset

Every preset is a `Jbig2Config` constructor. The contract column is what the
decoded page is guaranteed to be, relative to the pixels you supplied.

| Preset | What it does | Contract | Speed |
| --- | --- | --- | --- |
| `lossless()` / `generic()` | One generic region per page, no dictionary. | **Pixel-exact.** | Fastest |
| `text_lossless()` | Symbol coding; every component is kept, including isolated marks; dictionary reuse requires bitmap equality; everything else is stored separately or generically. | **Pixel-exact.** | Fast |
| `text_lossless_max_compression()` | Starts from the exact ingest and trial-encodes whole streams: dictionary reuse, stored variants, reference plus refinement, generic residuals, all three PDF dictionary policies. Keeps only measured byte savings. | **Pixel-exact.** | Slow, bounded by `symbol_trial_limit` |
| `text_lossy_max_compression(limit)` | The measured planner plus direct substitution of same-size glyphs within `limit` differing pixels and equal topology. | Lossy; reports actual source differences. | Slow, bounded |
| `text()` | Legacy fast symbol mode: components are filtered and near-identical glyphs share one bitmap. | **Lossy.** Can drop marks and change glyphs. | Fast |
| `text_symbol_unify()` | `text()` plus cross-page family unification. | **Lossy**, by design. | Slower than `text()` |
| `text_symbol_unify_safe()` | Unification with a topology guard that forces refinement when connectivity, holes or endpoints change. | Lossy, with fewer character-changing substitutions. | Slower |

Rules of thumb:

- Archival masters, line art, forms, anything where a substituted character
  is a bug: `text_lossless()`, or `lossless()` if symbol coding does not
  help.
- Smallest exact file and CPU to spare: `text_lossless_max_compression()`.
- Reader-constrained devices: set `decoder_resource_limits`; the measured
  presets choose the smallest *feasible* candidate and the fast presets fail
  loudly when their plan exceeds a cap. Defaults impose no budget.
- Measure on your own material. On born-digital text the exact symbol presets
  win by a wide margin; on noisy scans the gap narrows, and on some pages a
  generic region is smaller than any dictionary.

## Fidelity contracts

These hold across the native decoder and the external decoders in the test
suite (jbig2dec, PDFium and Poppler where configured).

- `lossless()`, `generic()`, `encode_single_image_lossless()` and
  `text_lossless()` reproduce every supplied bilevel pixel. The exact presets
  bypass fuzzy clustering even if its tuning fields are later enabled, and
  reject conflicting substitution settings before ingest.
- `text_lossless_max_compression()` accepts a refinement or layout only for a
  strict measured byte saving; the result stays pixel-exact.
- `text_lossy_max_compression(limit)` substitutes only same-dimension glyphs
  with zero offset, at most `limit` differing pixels and equal topology. The
  topology check limits a class of errors; it does not prove character
  identity. `EncoderMetrics` reports the instance types and source differences
  actually chosen.
- `text()` and `text_symbol_unify()` keep their 0.5 behaviour and byte
  baselines. They do not promise equality to the input: component selection
  can omit marks and substitution can change text. Use them when size matters
  more than exactness and verify on your corpus.
- The `pack_residuals`, `pdf_dictionary_policy` and
  `canonical_arithmetic_termination` options never change pixels; they change
  layout and bytes. Legacy presets leave them at their historical settings so
  existing streams are byte-identical to 0.5.4.

Details, the benchmark procedure and the resource-limit definitions are in
[docs/QUALITY.md](docs/QUALITY.md).

## Python

```sh
pip install jbig2enc
```

```python
import jbig2enc
from PIL import Image

page = Image.open("scan.png").convert("L")
pixels = jbig2enc.pack_grayscale(page.tobytes(), page.width, page.height, threshold=128)

data = jbig2enc.encode(pixels, page.width, page.height, symbol_mode=False)   # pixel-exact .jb2
globals_, page_data = jbig2enc.encode_for_pdf(pixels, page.width, page.height)
globals_, streams = jbig2enc.encode_document([(pixels, page.width, page.height)])

decoded, width, height = jbig2enc.decode(data)
assert decoded == pixels
```

The binding exposes `symbol_mode`, `refine` and the `lossless` preset; the
default `symbol_mode=True` is the lossy `text()` preset, so pass
`symbol_mode=False` when exactness matters. Calls release the GIL. The
binding lives in `src/python.rs` behind the `python` feature and is built
with [maturin](https://www.maturin.rs) from the `python/` directory, which
also holds the stubs and the test suite:

```sh
cd python && pip install maturin && maturin develop --release && pytest tests
```

See [python/README.md](python/README.md).

## Cargo features

| Feature | Default | Effect |
| --- | --- | --- |
| `encode` | yes | The encoder half. |
| `decode` | yes | The decoder half. Either half builds without the other; the PDF renderer uses `default-features = false, features = ["decode"]`. |
| `symboldict` | yes | Symbol-dictionary encoding (implies `encode`). Without it the encoder is generic-only. |
| `parallel` | no | Rayon in the measured symbol planner's trial encodes and page serialization. Never enabled implicitly; a document-parallel host should leave it off. |
| `python` | no | The PyO3 extension module. Built by maturin, not by `cargo build`. |
| `cli` | no | The `tester` and `halftone_tester` development binaries (`clap`, `env_logger`). They live in the repository only and are not part of the published crate, so `cargo install` does not offer them. |
| `halftone_bin` | no | Image loading for the halftone tool. |
| `refine` | no | Refinement-related configuration paths. |
| `trace_encoder`, `trace_arith`, `line_verify` | no | Diagnostics. |
| `profiling` | no | pprof flamegraphs in the benchmark workers (non-Windows). |
| `c-encoder-bench` | no | The opt-in C `jbig2enc` comparison harness; needs an external bridge. |

## Compatibility

- **Decoders.** Streams from the shipping presets are verified against the
  native decoder, jbig2dec, PDFium and Poppler in the test and benchmark
  tooling. New presets use canonical `FF AC` termination; legacy presets keep
  the historical extra `FF`, which every tested reader accepts. One known
  decoder-side difference remains: GRTEMPLATE 1 with TPGRON disagrees with
  jbig2dec 0.20 while agreeing with PDFium; production encoding never emits
  that combination.
- **Streams.** Template-0 generic coding (arithmetic, or MMR through
  `generic.mmr`), symbol/text regions with arithmetic coding, refinement
  template 0, halftone regions with MMR or arithmetic pattern dictionaries,
  standalone sequential organisation and PDF-embedded fragments are
  produced. Huffman-coded symbol dictionaries and text regions are decoded
  but not emitted.
- **API.** 0.6 keeps every 0.5 constructor and entry point, including the
  `jbig2enc_rust::jbig2*` module aliases. `Jbig2Config`, `PdfSplitOutput`
  and the metrics structs gained fields, so exhaustive struct literals
  downstream need updating; constructors and struct-update syntax do not.
  [docs/API-COMPATIBILITY.md](docs/API-COMPATIBILITY.md) records the audit.
- **Platforms.** Pure Rust, edition 2024, built and tested with Rust 1.97 on
  Linux, macOS and Windows. The only unconditional dependencies are
  `ndarray`, `anyhow`, `thiserror`, `log`, `rustc-hash`, `fax` and `wide`.
  SIMD uses portable `wide` kernels with scalar fallbacks; there is no
  target-specific code and a single `unsafe` block (an indexed read in the
  legacy symbol matcher).
- **Memory.** `PdfGroupOptions` bounds retained source pages per group and
  `DecodeLimits` bounds decoder allocations. Neither is a process RSS
  guarantee; [docs/QUALITY.md](docs/QUALITY.md) defines exactly what each
  counts.

## Performance

The generic kernel codes packed 32-pixel words with a single rolling context,
batches runs of zero-context pixels, and keeps state transitions out of the
common path, so most of the time goes to the pixels that carry information.
Symbol mode labels components in one sweep over span rows, counts signature
holes from packed runs, rejects most candidate pairs from ink-count bounds
before any alignment search, matches the rest with portable SIMD XOR/popcount
kernels, and memoizes the measured planner's repeated trial encodes; the 0.6
pass cut symbol-mode time to roughly a third of 0.5.4's on text pages with
every output byte unchanged. Numbers depend on the corpus and the machine,
so this README does not quote them; each report below states its inputs,
method and variation:

- [Generic kernel, hardware-profiled](docs/GENERIC-PERF-FOLLOWUP.md) and the
  [earlier C-parity measurement](docs/GENERIC-SPEED.md).
- [0.6 headroom pass](docs/GENERIC-06-HEADROOM.md) and its
  [re-run on an unloaded machine](docs/GENERIC-06-CLOUD.md).
- [Symbol-mode speed](docs/SYMBOL-SPEED.md).
- [Fidelity and size benchmarks](docs/QUALITY.md), with the raw data under
  `docs/benchmarks/`.

Decode benchmarks (`cargo bench`) synthesise an A4 page at 300 dpi and report
generic, symbol and halftone decode times on the current machine.

## Testing

```sh
cargo test                                             # full suite
cargo test --features parallel --lib --tests -- --skip grtemplate1_tpgron   # the release gate
cargo test --no-default-features --features decode     # decoder half alone
cargo test --test encode_byte_identical                # 40-stream byte manifest
SYMBOL_SPEED_DIR=/path/to/pbms cargo test --release --features parallel \
  --test symbol_speed -- --ignored --exact symbol_speed_worker --nocapture   # timing worker
```

The byte-identical guard encodes three original generated PBMs in every
shipping mode and compares against a manifest captured from the historical
encoder; it has not been regenerated since. Six round-trip tests and the
`--require-oracle` benchmarks need an external `jbig2dec` on `PATH` and
report themselves as skipped otherwise. Two legacy scan inputs are not
distributed; their hash gate runs explicitly in a checkout that has them. See
[tests/fixtures/README.md](tests/fixtures/README.md).

## Layout

```
src/
  encode/    api, arith (generic kernel), cc (components), classify,
             comparator, simd, cost, unify, halftone, document/ (symbol
             planning, dictionaries, text regions, residuals, PDF groups)
  decode/    segment/page parsing, generic/text/halftone/refinement regions,
             symbol dictionaries, huffman, mmr, arith, context pooling
  shared/    protocol core used by both halves (MQ table, bitmaps, limits)
  python.rs  the PyO3 module (feature `python`)
python/      wheel metadata, type stubs and Python tests
tools/       corpus preparation, benchmark and audit scripts
docs/        contracts, reports, benchmark data and validation logs
```

## License

Project contributions are MIT. The arithmetic coder in `src/encode/arith.rs`
and its extracted shared tables derive from `agl/jbig2enc` and remain
Apache-2.0 with their Google/Adam Langley notices. Cargo records the package
as `MIT AND Apache-2.0`: a mix of permissive terms, both texts included
(`LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE`). The Python wheel is declared the
same way because it links the crate. The DjVuLibre-derived component code
and its inherited tests have been replaced; there is no copyleft component.
[PROVENANCE.md](PROVENANCE.md) records the source history and audits.

## References

- ISO/IEC 14492:2001 / ITU-T T.88 (JBIG2)
- Original `jbig2enc`: https://github.com/agl/jbig2enc
- M. Valliappan, B. L. Evans, D. A. D. Tompkins, F. Kossentini, "Lossy
  Compression of Stochastic Halftones with JBIG2" (informed the halftone path)
- Symbol-dictionary design literature consulted for the unification cost
  model: Ye/Schilling/Cosman/Koy, "Symbol Dictionary Design for the JBIG2
  Standard"; Ye/Cosman, "Fast and Memory Efficient JBIG2 Encoder";
  Figuera/Yi/Bouman, "A New Approach to JBIG2 Binary Image Compression";
  Guo/Depalov/Bauer/Bradburn/Allebach/Bouman, "Binary Image Compression Using
  Conditional Entropy-Based Dictionary Design and Indexing"
