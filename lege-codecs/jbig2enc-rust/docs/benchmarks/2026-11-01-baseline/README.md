# Initial synthetic fidelity baseline

Command (release build, 350 generated pages):

```sh
JBIG2DEC=/path/to/jbig2dec python3 tools/quality_benchmark.py \
  --output /tmp/jbig2-quality-final-baseline-20261101 --pages 350 --require-oracle
```

`results.csv`, `metadata.json` and `corpus.json` retain measured output, tool and
source identities and all input hashes. jbig2dec 0.20 agrees with native output
on every pixel in every configuration. This is synthetic stress data, not a
representative scanned-book or e-reader hardware result.

| Configuration | Encoded bytes | Differing source pixels |
| --- | ---: | ---: |
| Generic lossless | 271,202 | 0 |
| Shipping symbol | 68,581 | 8,891 |
| Symbol, fixed threshold | 69,053 | 8,891 |
| Shipping unify | 56,119 | 26,983 |
| Unify, fixed threshold | 52,900 | 30,114 |
| Shipping text with refinement enabled | 85,468 | 8,891 |

The missing marks in the symbol configurations are an established input-fidelity
gap. Refinement does not restore components omitted by extraction. These data
do not satisfy the planned exact-symbol release gate.

The recorded `decoded_global_bitmap_bytes` excludes allocator/bookkeeping
overhead. Worker peak RSS includes input arrays and decode verification. Decode
timings are cold/warm native library calls, not physical-device latency.

`dependencies-linux-default.json` captures the 84-package resolved Cargo metadata
inventory for Linux/default features, including development dependencies. It is
not a substitute for source provenance or dependency license-text review.
