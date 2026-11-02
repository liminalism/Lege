# Exact symbols, component replacement and measured planning

These are implementation-checkpoint experiments on the same 350-page version-1
synthetic geometric corpus. Input hashes, actual source hashes, toolchain and
per-worker RSS accompany each run. They do not establish scanned-book quality or
reader-device latency. The separate real-corpus report covers supplied scans.

`initial-replacement`, `row-directory-trial` and `grouping-trial` record successive
component-extraction implementations. Their timings are exploratory; they are
retained so the initial slower implementations are not hidden. `joint-planner`
records the bounded family planner, which retains the 82,460-byte exact result
on this corpus because no measured refinement proposal improves it.

`matched-reference` contains seven randomized/interleaved pairs of separately
built workers at the replacement checkpoint, with compilation excluded. The
reference differs only by using the historical `cc.rs`; it is not a complete
0.6.0-versus-final-release comparison. The median candidate/reference ratios are
1.066 for encoding, 0.987 for cold decoding and 0.998 for warm decoding.
`source-comparison.json` states the snapshot and reference scope. The historical
GPL-derived reference source stays in a temporary directory outside this package.

The original golden hashes remain unchanged. A pixel-graph flood-fill oracle
checks 216 generated component images plus staged API, sparse-coordinate and
regrouping edge cases. The production extractor is graph traversal over spans;
the author-exposure and remaining licensing review are described separately.

The refinement family regression (in `tests/exact_symbols.rs`) measures complete
streams: 763-byte dictionary baseline, 1,552-byte generic control and 492-byte
selected exact output with 24 refined instances. Individual changes can lose
bytes while the coordinated family wins. Estimated and measured gains are logged
separately; the estimate is coarse and is not claimed as a calibrated predictor
of every corpus. Trial count stays at or below 64 and discovery at or below the
configured examined-reference budget per symbol.

`topology` records legacy unification (56,119 bytes, 26,983 source differences),
guarded unification (56,719 bytes, 26,487 differences) and exact output (82,460
bytes, zero differences). The guard corrects 205 retained instances here, but
legacy extraction still omits marks. The focused one-pixel hole fixture changes
24 pixels in legacy mode and zero after the guard. The newer bounded measured
lossy family regression selects 438 bytes with 10 refined and 14 substituted
instances and 14 source differences. Topology does not prove character identity.

All fidelity claims refer to supplied bilevel pixels. Native, jbig2dec, PDFium
and Poppler regressions exercise exact/refined/local/shared streams; Poppler's
monochrome output is compared to an identically transformed uncompressed PDF.
No golden manifest was regenerated and no missing oracle counts as a pass.
