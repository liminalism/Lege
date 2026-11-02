# Changelog

## 0.6.0 — Unreleased

Changes since the last published version, 0.5.4. This is a release draft;
no 0.6.0 publication is implied.

### Encoding and compression

- Faster generic encoding through packed-word context windows, native-width
  arithmetic-register storage, blank-span batching and deferred transition-table
  loads. A cloud re-run then retained two further byte-identical changes:
  zero-context runs are batched inside partially blank words, not only in
  blank words, and each bitmap context caches its probability estimate next
  to its state index. Together they remove about 13% of the kernel's
  instructions on text and scan-like pages. All of these optimizations
  preserve the existing generic stream bytes.
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

### Packaging

- The Python binding (`jbig2enc` on PyPI) moves into the crate as the
  `python` Cargo feature (`src/python.rs`), with its wheel metadata, stubs
  and tests under `python/`. Both ship in the crates.io package, so the
  published source carries the Rust and Python APIs. pyo3 is an optional
  dependency; `extension-module` is enabled only by maturin for the wheel.
  The wheel's license declaration now matches the crate (`MIT AND
  Apache-2.0`).
- The README is rewritten around capabilities, usage, fidelity contracts,
  features, compatibility and licensing; timing numbers live in the linked
  reports.

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
[hardware-profiled results](docs/GENERIC-PERF-FOLLOWUP.md), the bounded 0.6
[headroom assessment](docs/GENERIC-06-HEADROOM.md) and its
[cloud re-run](docs/GENERIC-06-CLOUD.md).
