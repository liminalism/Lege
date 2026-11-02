# Source provenance and licensing

The DjVuLibre-derived component implementation and its inherited tests have
been removed and replaced. The user accepted that replacement as closing the
GPL issue on 2026-11-01. There is no remaining GPL provenance release gate for
this crate, and the publication-disable flag has been removed. No publication
has been performed.

Project contributions are licensed under **MIT**. The arithmetic coder in
`src/encode/arith.rs` and its extracted shared table/identifier files retain
**Apache-2.0** terms and Google/Adam Langley attribution. Cargo records the
combined source distribution as `MIT AND Apache-2.0`: this is a mix of permissive
licenses, not a requirement to use GPL. Both license texts and NOTICE are included.
Apache-2.0 section 4 permits different terms for modifications/the derivative
work while requiring retention of the upstream license and notices.

## Source record

The current component extractor traverses a graph of row spans. Its design,
API compatibility and independent pixel-graph tests are in
[COMPONENT-EXTRACTION.md](docs/COMPONENT-EXTRACTION.md). Historical Bottou/Le Cun
attribution is retained in NOTICE as history, not as a license on the replacement.
No clean-room process is claimed.

The recovered [original Rust archive](https://github.com/liminalism/jbig2enc-rust)
is pinned at `e7a46300882d8c59ef014eb63e13378be134d8b3`. Its 95 commits,
58 production/build paths and recorded rename chains are retained under
[the dated audit](docs/audits/2026-11-01). Those records describe the investigation;
the earlier recommendations to keep a licensing gate open are superseded by the
user's acceptance and the disposition above.

The arithmetic coder's comparable upstream source is
[agl/jbig2enc at d2b0bf46](https://github.com/agl/jbig2enc/tree/d2b0bf4647f193c10a599e36f1f9620fa5e4514a).
Its Apache license, copyright and referenced patent notice remain recorded in
NOTICE and [the upstream notice inventory](docs/audits/2026-11-01/arithmetic-upstream-notices.json).
The source revision used for the original Rust translation is not recorded;
that historical limitation is not treated as an additional release blocker.

## Package and dependencies

The original geometric test fixtures and generator are MIT. The two old external
PBMs remain in the checkout for an explicit unchanged-hash regression and are
excluded from Cargo packaging. No C code was imported from doc_coder; the user
identified it only as the source of the old French scan. See
[fixture provenance](tests/fixtures/README.md).

The all-feature dependency inventory includes optional CDDL profiling and
MIT/Apache alternatives in dependencies also offering LGPL. The bounded license
text scan found no additional mandatory GPL grant. Source packages do not bundle
oracle binaries, historical reference implementations, benchmark PDFs or papers.
The [repository inventory](../../docs/LICENSING-AUDIT.md) describes the surrounding
Lege components; root, application and DjVu AGPL declarations are unchanged.
