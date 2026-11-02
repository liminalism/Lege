# Hardware-profiled generic optimization validation

The full checkout run passes 261 tests across 37 binaries, with eight intentional
ignores and two existing template-one-prefix filters. Independent jbig2dec 0.20
is configured. No fresh PDFium/Poppler result is claimed. Decoder code is unchanged.
Release context/original-byte/termination checks pass seven tests with one ignore;
the explicit old byte-manifest gate passes separately. Encode-only passes four
tests with one ignore. Formatting, Python AST and scoped whitespace checks pass.

The new scalar case compares million-pixel blanks and sparse boundary islands to
per-pixel arithmetic. The older dirty-tail and odd-width cases remain. All 367
real page payloads and dense/noise controls compare raw-byte identical to the
previous verified Rust worker and match pinned C after known padding normalization.

Cargo packaging and its extracted build verify. Inspection confirms only the
three original PBMs and the original MIT C++ harness, with external PDFs/rasters,
C sources, oracle binaries and perf.data absent. No API, dependencies, licenses,
stream modes or hidden parallelism were changed. No publication occurred.
The existing yanked spin0.10.0 lock warning remains. The archive digest describes
that build; later documentation/ledger changes are outside it.

See summary.json and package-inspection.json for counts/scopes and
[the performance report](../../GENERIC-PERF-FOLLOWUP.md) for timings and counters.
