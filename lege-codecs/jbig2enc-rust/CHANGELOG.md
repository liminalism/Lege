# Changelog

## 0.6.0 — Unreleased

Changes since the last published version, 0.5.4. This is a release draft;
no 0.6.0 publication is implied.

### Encoding and compression

- Faster generic encoding through packed-word context windows, native-width
  arithmetic-register storage, blank-span batching and deferred transition-table
  loads. These optimizations preserve the existing generic stream bytes.
- Explicit pixel-exact symbol presets retain isolated marks; legacy text and
  unify presets remain lossy and retain their settings.
- Optional residual packing, selective global/local PDF dictionaries and bounded
  measured planning compare actual encoded sizes. Safe lossy substitution uses
  topology safeguards; exact presets preserve the supplied bilevel pixels.
- Grouped PDF encoding bounds retained source pages/bitmap bytes and associates
  each group's pages with its globals. Measured resource limits remain explicit.

### Correctness and compatibility

- Correct short-reference refinement predictors for independent decoder
  interoperability, including extreme signed offsets.
- New exact/measured/safe presets use canonical arithmetic termination. Legacy
  constructors and low-level flush retain their historical bytes; callers can
  explicitly opt into canonical termination.
- Existing constructors and entry points remain. New configuration/metric fields
  require changes to exhaustive downstream struct literals; prefer constructors
  or struct-update syntax. See [API compatibility](docs/API-COMPATIBILITY.md).

### Licensing and testing

- Replace the DjVu-derived component extractor and inherited tests. Project
  contributions and original fixtures are MIT; retained arithmetic components
  keep Apache-2.0 terms/notices (package: MIT AND Apache-2.0).
- Exclude the two legacy external PBMs from Cargo distribution; retain their
  explicit unchanged-hash regression in the checkout. Default tests use three
  original generated fixtures and an independent historical byte baseline.
- Retain independent-decoder tests, context/flood-fill oracles, fixed-corpus
  benchmark hashes and measured resource reports. The existing template-one
  jbig2dec disagreement remains documented rather than reported as an oracle pass.

Performance numbers in the linked reports are corpus/compiler-specific generic
kernel measurements, not a universal whole-application speed promise. See
[hardware-profiled results](docs/GENERIC-PERF-FOLLOWUP.md) and the bounded 0.6
headroom assessment under docs for this final pass.
