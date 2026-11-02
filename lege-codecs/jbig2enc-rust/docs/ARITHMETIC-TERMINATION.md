# Arithmetic stop-marker compatibility

The legacy arithmetic finalizer can emit `FF FF AC` when the final pending
byte is already `FF`. The extra prefix byte is redundant: Poppler reports an
extraneous segment byte, while the tested decoders reconstruct the same pixels.
This was reproduced on the original geometric corpus without relying on a
scanned input or copying renderer implementation code.

The new `canonical_arithmetic_termination` config field emits a single `FF AC`
stop marker in that case. It applies to generic regions, symbol dictionaries,
plain/refined text, and arithmetic halftone dictionaries/planes. Segment lengths
and measured planner costs use the resulting actual payload bytes.

`text_lossless()`, its measured exact/lossy variants, and
`text_symbol_unify_safe()` enable it. Existing `default()`, `text()`,
`text_symbol_unify()`, `generic()`, `lossless()`,
`encode_single_image_lossless()` and direct low-level `flush()` calls retain
their byte baselines. To select canonical termination for an existing preset:

```rust
let mut config = jbig2enc_rust::Jbig2Config::generic();
config.canonical_arithmetic_termination = true;
```

The regression test first rewrites already-encoded test segments independently:
it removes one duplicated prefix byte and recomputes segment lengths, without
calling the new finalizer. New encoder output equals that transformed stream.
The six corpus/mode cases remove seven bytes and seven Poppler diagnostics;
native, jbig2dec, PDFium and Poppler retain identical pixels. Canonical standalone
output also passes native/jbig2dec checks. Both old byte-hash manifests remain
unchanged and pass; the external legacy gate is run explicitly.

On the fixed 17-page mixed corpus, exact, measured exact and measured lossy
outputs are 278,536 / 272,239 / 272,189 bytes. Source differences are 0 / 0 / 20.
All 102 PDF backend/page checks report zero trailing-byte diagnostics, and all
native/jbig2dec checks agree. See
[canonical-mixed](benchmarks/2026-11-01-real-corpus/canonical-mixed).
These are updated measurements, separate from earlier pre-fix checkpoints.

All 782 book pages also retain zero jbig2dec differences for 32/64/128/whole-book
groups. The isolated encoder measurements are:

| Group pages | Encoded bytes | Encoder process peak KiB |
| ---: | ---: | ---: |
| 32 | 22,134,352 | 83,160 |
| 64 | 22,106,393 | 146,160 |
| 128 | 22,082,511 | 275,724 |
| Whole book | 22,019,090 | 1,240,532 |

The loader and file sink are included in encoder RSS; external verification
occurs after the encoder child exits. These are desktop observations, not reader
capacity defaults. [Encoder-memory reports](benchmarks/2026-11-01-real-corpus/canonical-encoder-memory)
retain source, worker and encoded-stream identities.

The comparable [Apache-licensed upstream finalizer](https://github.com/agl/jbig2enc/blob/d2b0bf4647f193c10a599e36f1f9620fa5e4514a/src/jbig2arith.cc)
also avoids duplicating the marker prefix. Its Google/Adam Langley notices
remain in this package. Poppler's warning handling was inspected through its
public source mirror, but that default-source snapshot is not represented as
the exact 26.01.0 binary source. The behavioral evidence above comes directly
from the recorded oracle binaries. No Poppler implementation is included here.

Legacy padding warnings remain reproducible when canonical termination is
disabled. This compatibility option does not settle the separate decoder-only
template-one/TPGRON oracle disagreement or the source-rights release gate.
