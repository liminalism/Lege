Current disposition (user clarification): the DjVu replacement closes the GPL issue. Project contributions are MIT and retained arithmetic components are Apache-2.0. This dated investigation is historical; its proposed extra review/publication gate is superseded by PROVENANCE.md.

# Follow-up source review — 2026-11-01

The recovered archive narrows the provenance questions. It does not supply an
independent rights grant for the old DjVuLibre port. `publish = false` remains
in force. This is a technical provenance review with an explicit evidence
boundary, not a legal clearance or clean-room certification.

## Archive and module origins

[archive-production-paths.json](archive-production-paths.json) records every one
of the archive's 58 Rust production/build paths at
`e7a46300882d8c59ef014eb63e13378be134d8b3`. Two rename commits connect the
`encode/` names through the intermediate layout to the earlier root names.
There are 110 distinct path histories, each shorter than the requested
100-entry page. The archive's complete 95-commit list is separately retained in
[upstream-history.json](upstream-history.json).

| Source group | Earliest evidence in the recovered history | Assessment and boundary |
| --- | --- | --- |
| Old component extractor | `src/jbig2cc.rs`, upload `adcfb2ee` on 2026-02-02; the opening documentation already describes the DjVuLibre/DJVULibRust port | The upload does not establish independent origin. Its production bodies and inherited tests have been replaced. API/cleanup compatibility and author exposure are disclosed below. |
| Arithmetic encoder | `src/jbig2arith.rs` at root commit `3b743f3c`, 2026-01-15, identifying `jbig2arith.cc`/`.h` | The upstream implementation carries Apache-2.0. A comparable pre-upload snapshot, both source-header identities, COPYING and its referenced patent notice are pinned in [arithmetic-upstream-notices.json](arithmetic-upstream-notices.json). The precise translation revision remains unknown. |
| Bitmap, comparator, structures, halftone, utilities and build script | Already present at the root upload on 2026-01-15 | No earlier authorship history is recovered. The initial manifest declares MIT OR Apache-2.0; that declaration does not override the explicit upstream port licenses. The halftone header identifies the Valliappan/Evans/Tompkins/Kossentini paper as its algorithmic basis. |
| Classifier, cost/context models and unification | Root paths first appear in the 2026-03-18 next-generation sync | A sync timestamp is not a creation date or proof of rights. Current files and their origin comments are hashed in the local inventory. |
| Document and symbol submodules | Split into files by `118ae37c`, 2026-06-09 | Path additions represent a refactor, not newly established authorship. Older monolithic bodies are not automatically attributed by path history. |
| SIMD | Root path appears in `fc5c7fe6`, 2026-07-01 | Recorded introduction and subsequent renames; no independent upstream source established from that record alone. |
| Decoder | Added across the July 2026 decoder phases, then allocation/pooling follow-ups | Development history is recovered, but no claim that commit messages prove original authorship. The reviewed explicit source comments identify no additional external port. |
| Shared MQ table, integer identifiers and segment types | July extraction commits name their previous encoder roles | Treat extracted arithmetic data/identifiers with the coder's attribution; moving them to `shared/` does not remove those obligations. |
| New exact/planner/residual/topology/group/resource code and support tools | Local 2026-11-01 work; absent from archived tree | New implementation is covered by local source hashes and behavioral/oracle evidence. It does not inherit a clean-room claim for the surrounding library. |

The [local source inventory](source-history.json) covers 64 production/build
files and test, benchmark, fuzz and Python tool source (123 files at this
checkpoint). Local git history begins at the monorepo import. The inventory
records file hashes and explicit origin comments, not inferred copyrights.
The later original-fixture generator adds one Python source file; the generated
inventory records the current count and hashes rather than treating 123 as a
permanent total.

## Component replacement and shared-source comparison

`tools/audit_source_overlap.py` compared all 63 current `src/**/*.rs` files
against the local DjVu crate's 64 Rust source files. It finds exact sequences
of at least six meaningful normalized lines, ignoring imports, comments,
attributes and punctuation-only lines. It records file hashes and line ranges,
without reproducing reference bodies in the report.

The [before report](djvu-overlap-before.json) found seven blocks. The largest
was 44 meaningful lines spanning the old component-test helper and four tests.
Those inherited bodies have now been replaced with newly written contract
tests and independent test data. Nine component integration tests cover maximal
row spans, eight-connected partitioning, determinism, half-open bounds, bitmap
materialization, explicit cleanup, randomized pixel-graph equality and sparse
staged coordinates. The unchanged encoder hash manifest still passes.

The [after report](djvu-overlap-after.json) has six blocks:

- Three in `cc.rs`: public bound/descriptor fields, `CCImage` fields and the
  constructor's dimension/empty-vector initialization. These are retained API
  and ordinary initialization expressions, not the former component algorithm.
- Two in `comparator.rs`: the public `distance` signature and a short
  best-error/optional-result expression. The archived JBIG2 comparator already
  exists at its January root upload; the DjVu source also explicitly describes
  its bitmap structures as coming from `jbig2sym.rs`. This does not establish
  the direction or rights of every shared expression.
- One in `sym.rs`: the public `Rect` coordinate/size declaration.

The fresh component algorithm traverses a graph of maximal row spans, rather
than the old union-find body. Its [design record](../../COMPONENT-EXTRACTION.md)
discloses access to the old source, retained public types, cleanup thresholds
and compatibility ordering. Notices remain. This comparison cannot detect all
translated, renamed, reordered or non-Rust copying; a lack of matches does not
prove independence. Appropriate review of that explicit boundary remains open.

To repeat the exact-overlap triage from the monorepo root:

```sh
python3 lege-codecs/jbig2enc-rust/tools/audit_source_overlap.py \
  --source lege-codecs/jbig2enc-rust/src \
  --reference lege-codecs/djvulibrust/src --output /tmp/jbig2-overlaps.json
```

## Dependency texts and included assets

The all-feature Cargo graph resolves 189 packages without a target filter.
The [text scan](dependency-license-markers.json) reads 11,457 UTF-8 files
(138,893,272 bytes) in the other 188 package directories. It counts and skips
2,795 binary files; no non-UTF-8 nonbinary files were encountered. Thirty GNU
marker lines were classified:

| Lines | Classification |
| ---: | --- |
| 14 | `r-efi` 5.3/6.0 alternative licenses, authors and manifest/README wording; MIT and Apache alternatives are explicit |
| 12 | GPL compatibility wording in six Apache-2.0 LLVM-exception license texts |
| 2 | `nix` kernel test asset expressly offering MIT as well as GPL |
| 2 | `lgpl` identifiers of type `LOGPALETTE`, not license statements |

No additional mandatory GPL grant was identified by this text scan. That finding
does not cover binary interpretation or establish source authorship. Optional
`inferno` profiling remains CDDL-1.0, so “every feature is purely permissive” is
not claimed. Fifteen resolved packages have no recognized license-text file in
the downloaded package; their declared expressions and that gap remain visible
in [dependencies.json](dependencies.json). A binary distributor must still
prepare notices for the exact features, targets and bundled dependencies.

The user identified the large legacy PBM as a resized image from
[`thrasibule/doc_coder/test`](https://github.com/thrasibule/doc_coder/tree/ad14a6ffa20833f6c47d9f4a43690f4366f7584c/test)
and confirmed that no C code from that repository was used. The pinned upstream
PBM is 1956×3186; the existing fixture is 956×1557. Visual inspection confirms
the same French prefatory page, with different raster size and bytes.
The repository's codec license has a commercial-use restriction, and no
separate image grant was found. [fixture-provenance.json](fixture-provenance.json)
records those facts; neither an old-looking page nor an available download
establishes permission to distribute this particular scan under Apache-2.0.
The old fixture and golden hashes are unchanged. The release corpus has now
been separated: Cargo packaging excludes both old PBMs, and default tests use
three original font-free geometric PBMs under Apache-2.0. Their complete
generator, byte hashes and 40-stream baseline captured with verified historical
production source are included. The current encoder matches that baseline.
The original legacy hash gate is explicitly ignored by default, remains intact,
and passes when run by name with `--ignored`. An explicit legacy round-trip
matrix also passes; it fails if the old inputs are absent and does not download
or substitute them. [Fixture instructions](../../../tests/fixtures/README.md)
record both commands. The crate avoids distributing the uncertain asset.

## Release disposition

Technical provenance evidence is now concrete and reviewable: archive paths,
source hashes, classified overlaps, replacement tests, notices, dependency
exceptions and fixture origins. The remaining decisions are the appropriateness
of the exposed-source replacement and retained compatibility surface, and source
comparison/rights for unestablished earlier origins. The scan-distribution issue
is addressed for the crate package by excluding the scan, not by changing its
license. Source-rights questions are not closed by changing manifest text. Publication remains
disabled and the crate is not advertised as a cleared non-GPL release.

The surrounding Lege application's and DjVu crate's AGPL declarations remain
unchanged; see the [monorepo inventory](../../../../../docs/LICENSING-AUDIT.md).
