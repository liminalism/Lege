# PDF dictionary policy experiment

Release worker, jbig2dec 0.20 required. Commands:

```sh
JBIG2DEC=/path/to/jbig2dec python3 tools/quality_benchmark.py \
  --output /tmp/jbig2-quality-dictionary-policies-20261101 --pages 350 --require-oracle \
  --modes symbol_shipping symbol_shared symbol_local
JBIG2DEC=/path/to/jbig2dec python3 tools/quality_benchmark.py \
  --output /tmp/jbig2-quality-short-policies-20261101 --pages 3 --require-oracle \
  --modes symbol_shipping symbol_shared symbol_local
```

The short corpus exposes one-page variants. The 350-page corpus repeats its
finite synthetic vocabulary across pages and therefore provides no advantage
for selective globals. All native and jbig2dec decoded outputs agree.

| Corpus | All-global bytes / source differences | Shared-global bytes / source differences | Shared-global + local bytes / source differences |
| --- | ---: | ---: | ---: |
| 3 pages | 881 / 120 | 1,837 / 115 | 964 / 120 |
| 350 pages | 68,581 / 8,891 | 68,581 / 8,891 | 68,581 / 8,891 |

The shared-global policy's generic fallback restores some isolated source marks
on the short corpus. Its fidelity still is not exact. No compression improvement
is claimed from these results. A cost selector must retain all-global when it
is cheaper at the required fidelity. CSVs include timing and memory measurements;
the corpora and source identities are retained alongside them.

Separate strict interoperability tests cover all three policies with and without
globals, odd page widths, and unchanged standalone behavior. Tested renderers:
Poppler 26.01.0 and pypdfium2 5.14.0 (PDFium version captured by the oracle script).
Poppler's monochrome render is compared with its equivalent uncompressed PDF
control; PDFium matches source pixels directly. Poppler's existing trailing-byte
warnings remain unresolved release-audit findings.

The residual microbenchmark (`cargo test --release --lib residual::tests -- --nocapture`)
measured 128 bytes for a single 4097×4097 sparse bounding region versus 92 bytes
for two packed regions. Both include headers and reproduce every source pixel
in native and jbig2dec checks. A recorded run took 65,935 µs for the baseline
and 64,175 µs for the measured packer; these single-run timings have normal
measurement noise. Dense 33×31 and 273×337 cases stayed at 58 and 241 bytes;
their candidate trials cost additional CPU (208→334 µs and 700→1,721 µs).
