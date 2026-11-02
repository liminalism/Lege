# MIT and generic-speed validation — 2026-11-02

The source checkout and a freshly verified/unpacked Cargo archive each pass
260 tests in 37 functional binaries, with eight intentional ignores and two
explicit pre-existing template-one-prefix filters. Independent jbig2dec 0.20 is
configured; PDFium/Poppler are not configured in this continuation. Their older
strict results remain separately documented rather than being claimed anew.

Release checks pass the independent scalar context oracle, historical original
fixture byte manifest and arithmetic termination tests (six tests, one ignore).
The explicit old unchanged-manifest regression also passes. Encode-only checks
and its generic/hash tests pass; decode-only has 59 passing tests. Fixture
reproduction, formatting, Python syntax and scoped whitespace checks pass.

The package verifies and inspection confirms only three original MIT PBMs,
MIT AND Apache-2.0 metadata, publication enabled, both license texts/notices and
no benchmark rasters, external C implementation, papers or oracle binaries.
Only the original MIT C++ benchmark harness is included. No publication occurred.
The existing yanked spin0.10.0 package warning remains.

See summary.json for counts/scopes and package-inspection.json for the exact
archive digest. Later report/ledger edits are outside that archive digest.
The 367-page C comparison, byte equality and speed timings are retained in
[the generic report](../../GENERIC-SPEED.md).
