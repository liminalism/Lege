# Lege source licensing inventory — 2026-11-01

The agreed scope is a non-GPL `jbig2enc-rust`, with an accurate account of the
surrounding repository. The application and DjVu codec retain their existing
AGPL declarations. This inventory records declarations and unresolved provenance;
it does not grant rights or clear a release.

The root workspace and `LICENSE` declare AGPL-3.0-only. `lege-process` and
`lege-codecs/djvulibrust` also declare AGPL-3.0-only, and the processor directly
links the DjVu crate. Replacing the JBIG2 component extractor does not change
the licensing of those components.

The [manifest inventory](../lege-codecs/jbig2enc-rust/docs/audits/2026-11-01-mit-generic-speed/monorepo-manifests.json)
covers 89 Cargo packages: 37 declare AGPL-3.0-only, 32 MIT, two MIT OR
Apache-2.0, three Apache-2.0, one MIT AND Apache-2.0, and 14 have no resolved manifest license field.
The last category is a metadata gap, not evidence of public-domain or proprietary
status. Inherited workspace fields were resolved where present.

Missing manifest declarations were observed in:

- `lege-gpu`, `lege-ocr`, `lege-misc/xtask`, `lege-pdf/write`, and
  `lege-document-ocr/installer-winsafe`.
- The JBIG2, JPEG2000 and PDF renderer fuzz packages.
- The renderer's `pdfium-diff` and `pdfium-text-diff` tools.
- Three vendored `rfd` example packages.

No licenses were assigned to these packages on the strength of their location.
The root notice now distinguishes the JBIG2 manifest declaration and its accepted
[licensing disposition](../lege-codecs/jbig2enc-rust/PROVENANCE.md).

The JBIG2 all-feature Cargo metadata inventory contains 189 packages, including
development, build and target-conditional dependencies. Optional profiling pulls
in `inferno`, declared CDDL-1.0. Two target-specific versions of `r-efi` declare
MIT OR Apache-2.0 OR LGPL-2.1-or-later; that expression offers non-GPL alternatives.
There are no missing license expressions in this metadata snapshot. These facts
are broader than the earlier default Linux inventory and must not be reduced to
the claim that every feature is purely permissive.

The [dependency inventory](../lege-codecs/jbig2enc-rust/docs/audits/2026-11-01-mit-generic-speed/dependencies.json)
records available license-text hashes and flags text review as outstanding.
Some downloaded packages contain no top-level license text despite declaring
an SPDX expression. Release packaging still needs the appropriate notices and
license texts for the exact build. Target-conditional metadata was inventoried;
those targets were not all compiled.

The follow-up [dependency text scan](../lege-codecs/jbig2enc-rust/docs/audits/2026-11-01/dependency-license-markers.json)
classified 30 GNU-marker lines in 11,457 UTF-8 files: alternative `r-efi` grants,
Apache LLVM exception wording, a dual MIT/GPL kernel test asset, and two
`LOGPALETTE` identifier false positives. It found no additional mandatory GPL
grant within that scan's text scope. Binary files were counted and skipped;
absence of a marker does not establish independent source provenance.

This audit does not clear model weights, datasets, fonts, npm packages, bundled
executables, or every source file in the monorepo. In particular the renderer's
`THIRD-PARTY-NOTICES.md`, PDFium portions, vendored Freya/rfd notices, SymSpell
notices and application license bundle retain their own obligations. No raw
new benchmark PDFs, page rasters or external decoder binaries enter the JBIG2
package. Two pre-existing PBM test fixtures remain in the checkout. The user traced
the larger French page to [doc_coder](../lege-codecs/jbig2enc-rust/docs/audits/2026-11-01/fixture-provenance.json)
and confirmed that only the image was used, not its C codec. That repository's
codec license restricts commercial use and supplies no separate unrestricted
image grant. The resized scan is not represented as Apache-2.0; its distribution
rights remain unresolved. The other fixture is a trivial all-white 2×2 image
with an IrfanView header and no separately recorded generator provenance.
Both external inputs are now excluded from Cargo packaging. Three original,
font-free geometric PBMs, a reproducible MIT generator and a separate
historical-encoder byte baseline replace them in the distributed test suite.
The old fixture/manifest bytes are unchanged, and explicit legacy regression
and round-trip runs still pass. This resolves the crate asset issue by excluding
the uncertain material; it does not establish a new grant for that material or
change the retained upstream notices.

The local source-history inventory starts at the monorepo import. The user
subsequently located the archived original Rust repository; its 95-commit
metadata and all 58 production/build path histories (110 paths through recorded
renames) are retained alongside the local inventory. The four inherited component
test bodies have also been replaced with independently written contract tests;
legacy encoder byte hashes remain unchanged.

The user accepted the independently replaced DjVu component implementation as
resolving the GPL issue. The crate now licenses project contributions MIT and
retains Apache-2.0 terms on the arithmetic-coder components, recorded as
MIT AND Apache-2.0. The earlier request for an additional licensing gate has
been withdrawn, and its publication-disable flag has been removed. Historical
audit limitations remain recorded rather than being treated as further blockers.
