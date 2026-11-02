# Bounded 0.6 headroom and release-note checks

Both experimental implementations passed release context/hash tests and separate
17-page raw-byte comparisons against the previous Rust worker and pinned C.
Neither met the requested performance criterion; production arith.rs was restored
exactly to its starting hash.

Final restored-source verification: 261 functional tests pass across 37 binaries,
with eight intentional ignores and two existing template-one-prefix filters.
Independent jbig2dec 0.20 is configured. Release tests pass seven cases with one
ignore. No new PDFium/Poppler run is claimed. Formatting, Python tool syntax and
scoped whitespace checks pass.

Cargo package/build verification passes. The archive includes the draft changelog
and README summary, unchanged arithmetic source, both license texts/notices and
only the three original PBM fixtures. Raw private corpus, external implementations,
oracles and perf data are excluded. The existing yanked spin0.10.0 lock warning
remains. No publication occurred. Later documentation/ledger changes are outside
the recorded archive digest.

See [experiment disposition](../../GENERIC-06-HEADROOM.md) and
[crates.io notes](../../CRATES-IO-RELEASE-NOTES.md).
