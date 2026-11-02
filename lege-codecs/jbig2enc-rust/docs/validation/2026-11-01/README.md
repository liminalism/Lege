# Validation scope

The final canonical code package builds/verifies as 427 files, 1,516,422
compressed bytes, SHA-256
`474db68d2b8cc8b1ae22a6a4d98557dc6f9445a0e21c80c130f2a0a2798f3c14`.
Its fresh extraction contains only the three original PBMs. The generator and
35 functional binaries pass 259 tests, with seven intentional ignores and two
documented template-one filters; the PDFium control is separately rerun and
passes on this same unpacked source. See `canonical-termination/unpacked-tests.json`,
`package-inspection.json`, the full logs and `template1-control.log`.
Subsequent report/ledger edits are outside that archive digest. Packaging and
codec fidelity do not establish the remaining source-rights clearance.

The later [canonical-termination fix](../../ARITHMETIC-TERMINATION.md) independently
reproduces and removes seven redundant bytes/Poppler diagnostics without changing
pixels. The new exact/measured/safe presets enable it; legacy presets preserve
their hashes and can opt in. `canonical-termination/` retains the byte-transform
probe, final standalone/PDF regression, old hash gate, broad 258-pass checkpoint,
59 decode-only passes and reduced/optional feature checks. The final focused
regression adds preset-policy and canonical standalone checks. The historical
template-one/TPGRON filter remains explicit.

Fixture selection now lives in a dependency-free helper. Encode-only tests
compare the 12 generic/lossless entries of the same 40-stream manifest; full
builds retain all five modes and shared-global cases. Neither manifest was
regenerated, and reduced builds are prohibited from overwriting them.

All 17 mixed pages pass the four decoders in exact/measured exact/measured lossy
modes with 0/0/20 source differences, and all 102 PDF page/backend checks report
zero trailing-byte warnings. All 782 pages in each 32/64/128/whole-book group run
retain zero jbig2dec differences after canonical termination. Updated bytes and
isolated encoder RSS are in the separate `canonical-mixed` and
`canonical-encoder-memory` benchmark directories; earlier checkpoints below
retain their original identities and figures.

The subsequent fixture-distribution check uses three original geometric PBMs,
with an included generator and manifest. Forty emitted-stream hashes captured
with the verified historical HEAD encoder match current output; the baseline
source/harness identities are in `original-fixture-baseline.json`. The original
legacy fixture and hash bytes remain unchanged, and the explicit old hash gate
and legacy encoder/decoder matrix pass (`retained-legacy-*.log`). The legacy
hash test is intentionally ignored by default because its external inputs are
excluded from the package; it was run explicitly, not counted as a default pass.

The updated default parallel library/integration suite passes 257 executions,
with seven intentional ignores and two prefix-filtered names
(`original-fixture-functional.log`). This is the same documented template-one
oracle exception described below. The original geometric corpus also passes
native, jbig2dec, PDFium and Poppler for generic, exact and measured-exact modes:
847/468/433 output bytes and zero source differences. Reports are in
`original-fixture-oracles/`; these small synthetic results are regression
coverage, not scanned-book compression claims. The ten Python tools parse,
and the generator's `--check` reproduces all included raster/inventory bytes.

The separated-fixture Cargo archive builds and verifies (390 files,
1,297,617 compressed bytes, SHA-256
`63ebe21b6a61bb6bb8dbf84676726fb0c828ef809097141b214d0301b02e08f4`).
A fresh extraction contains only the three original PBMs; both legacy images
are absent. Its own generator check and parallel library/integration suite pass
with the same 257/7/2 counts (`original-fixture-unpacked-tests.json` and log).
This exercises the distributed artifact without relying on checkout assets.
`original-fixture-package-inspection.json` pins the old/new manifest hashes and
archive contents. Subsequent report/comment edits are outside that digest.
The old `spin` yanked-version warning remains visible in the package log;
no dependency version change is hidden in this work.

After the source-overlap review replaced four inherited component test bodies,
the nine newly written component integration tests and two byte-manifest test
executions pass (`provenance-contract-tests.log`). This includes the unchanged
original encoder hashes; the fixtures and hash manifest were not regenerated.
`cargo fmt --check`, all nine Python tools' AST syntax checks, and scoped
`git diff --check` also pass. Earlier execution counts below describe their
retained checkpoints, before the component tests moved to integration coverage.

The focused final run passes 30 test executions with one benchmark worker
intentionally ignored. It covers component extraction, unchanged byte hashes,
exact symbols, measured exact/lossy planning, topology corrections, PDF dictionary
policies and bounded groups. `JBIG2DEC`, `PDFTOPPM`, `PDFIUM_PYTHON` and the strict
PDF-oracle flag were set to the retained external tool versions. No golden
manifest was regenerated. See `focused.log`.

After adding online calibration, the exact/lossy/group/baseline suite passes
again (`calibrated-final.log`). The updated planner also passes all 17 real mixed
pages through native, jbig2dec, PDFium and Poppler, both unconstrained and with an
explicit global bitmap cap (`mixed-calibrated` and `mixed-constrained` reports).
The exported bitmap prediction is checked against native retained globals.

The broader parallel library/integration run passes 256 executions, with five
intentional ignores and two filtered names. Command:

```sh
cargo test --offline --features parallel --lib --tests -- --skip grtemplate1_tpgron
```

That name filter covers the old differential test and the new PDFium control;
the PDFium control was also executed independently and passed
(`template1-pdfium.log`). The unfiltered all-target run failed on the old
GRTEMPLATE-1/TPGRON disagreement with jbig2dec 0.20. Historical HEAD reproduces the
same mismatch at (6,4) (`historical-template1-oracle-failure.log`), and its source
already records the context-numbering distinction. The original test is retained.
Production encoding uses template zero without TPGRON. This is an explicit
exception, not an unqualified all-target/all-oracle pass. The upstream
[jbig2dec refinement procedure](https://github.com/ArtifexSoftware/jbig2dec/blob/master/jbig2_refinement.c)
and [PDFium refinement procedure](https://pdfium.googlesource.com/pdfium/+/refs/heads/chromium/7113/core/fxcodec/jbig2/JBig2_GrrdProc.cpp)
use different template-one SLTP context choices; independent rendered control
supports the native result for the retained fixture. No decoder code was copied.

Reduced/optional configuration checks also pass:

- Decode-only all-target tests: 59 passed.
- Encode-only all-target compilation.
- CLI, parallel and profiling all-target compilation on this Linux host.
- `cargo fmt -- --check` and Python tool syntax checks.
- Local `cargo package --allow-dirty --offline` build and verification.

The package inspection snapshot contains 279 files and only the two legacy PBMs
under test fixtures. No new corpus PDF/PBM, paper, historical source snapshot or
oracle binary is included. The package snapshot predates some final report
documents; its digest identifies that exact build, not a published release.
Current include rules explicitly allow Python source/specifications and exclude
Python bytecode. Packaging grants no license clearance. Optional C-bridge linking
and compilation on other targets were not performed.

The post-provenance-review package also builds and verifies: 368 files,
1,302,674 compressed bytes, SHA-256
`14d9ee2575df884c2071c0def63787316922ed8f299af7f2e65610a9216ecb78`.
`provenance-package-inspection.json` records its exact identity and confirms
only the two legacy PBMs, with no PDF, Python bytecode, paper or scratch files.
The two legacy PBMs' unresolved rights are explicitly documented; this is a
build check, not a distributable-release approval. Cargo also reports the
existing locked `spin` 0.10.0 as yanked; the warning is retained in
`provenance-package.log`. Report changes after that package snapshot are not
part of its digest.

The book-corpus and grouping runs have separate source/input hashes and tool
versions under `docs/benchmarks/2026-11-01-real-corpus`. Single-run timings there
are desktop observations, not reader-device acceptance. Historical Poppler
trailing-byte warnings are retained. The canonical-termination fix above resolves
them for the tested new modes; byte-compatible legacy modes still reproduce them
unless the option is selected.
