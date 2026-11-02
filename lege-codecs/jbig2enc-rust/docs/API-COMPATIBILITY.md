# API and packaging audit for the 0.7 work

Existing constructors and entry points remain, including
`encode_single_image_lossless()`, `encode_document()`, `Jbig2Encoder::new()` and
`flush_pdf_split()`. The generic lossless API retains its previous coding mode.
`Jbig2Config::text()` and the legacy unify preset keep their settings. Exact and
measured lossy behavior is explicitly selected through new constructors.

New fidelity presets and `text_symbol_unify_safe()` enable
`canonical_arithmetic_termination`; legacy constructors and the direct
arithmetic `flush()` API keep their old bytes. Existing callers can select the
option explicitly. It removes a redundant stop-marker prefix rather than
changing pixels; see [termination evidence](ARITHMETIC-TERMINATION.md).

The original byte-hash manifest has not been regenerated. It passes across
generic, lossless, symbol, unify and refinement fixtures in standalone and PDF
forms. The short-reference interoperability fix changes the predictor only in
the affected offset case; a regression fails on historical source and passes
with the fix. No claim of byte identity for every possible buggy legacy stream
is made. Previously affected output pixels now agree across decoders.

The connected-component public structs and staged methods are retained. The
default `nregularccs` value and ordering preserve historical behavior, including
the old default that did not activate its optional reading-order sorter.
The independent pixel-graph oracle and staged API tests cover the replacement.

`PdfSplitOutput` remains available. Grouping is additive: `PdfGroupEncoder`
emits independent `EncodedPdfGroup` values with explicit page/globals association.
Existing callers need not use grouping, residual packing or resource limits.
New config and metric fields do require updating downstream exhaustive struct
literals; constructors or struct update syntax avoid that source-compatibility
issue. No exhaustive `Jbig2Config` literals were found in the scanned Lege
processor, bindings and PDF Rust sources. Downstream applications were not all
rebuilt by this audit.

Default/parallel library and integration tests, decode-only all-target tests,
encode-only checks, and CLI/parallel/profiling all-target compilation are recorded
under `validation/2026-11-01`. The optional C comparison feature requires an
external bridge and was not linked. Platform-conditional dependencies were
inventoried, not compiled on every platform.

The broad external-oracle run exposes a pre-existing decoder-only template-1
TPGRON disagreement with jbig2dec 0.20. The remaining functional sweep uses an
explicit filter; the independently executed PDFium control agrees with native.
This is not reported as an unqualified all-target pass.

The package remains version 0.6.0 during development. The licensing publication
block has been removed after acceptance of the DjVu-code replacement. Existing version-reporting helpers still include legacy hardcoded tags
(`document::get_version()` returns 0.2.0 and `build.rs` supplies a 0.29 upstream
tag). Those strings were not used as source-provenance evidence. Reconciling them
belongs with the eventual reviewed release/version transition.

Cargo's package build/verification succeeds locally. Its include list excludes
papers, the new PDF/PBM benchmark corpus, historical source snapshots and oracle
binaries. The two old PBM fixtures remain unchanged in the checkout and are now
excluded from the package. Default tests use three original generated fixtures
and a separate historical byte baseline; the unchanged legacy hash gate and
round-trip matrix run explicitly as documented in `tests/fixtures/README.md`.
Successful packaging is a build check and grants no distribution rights;
the [licensing disposition](../PROVENANCE.md) records the MIT/Apache components.
