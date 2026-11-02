Current disposition: the user accepted this replacement as resolving the GPL issue on 2026-11-01. The technical/exposure record below is retained; its earlier requests for a further licensing gate are superseded. Project contributions are MIT.

# Component extraction replacement

The production bodies of `src/encode/cc.rs` were replaced on 2026-11-01. The
earlier file described a DjVuLibre `cjb2.cpp` translation. Its attribution is
retained in `NOTICE` and repository history; it has not been relabeled as
permissive code. The replacement author had access to the previous module.
This record does **not** claim a clean-room process or resolve licensing review.

## Design specification

The input is a set of foreground pixels on a rectangular integer grid. Two
foreground pixels are adjacent when both coordinate differences are at most
one. Components are the reachable sets under that relation. Extraction with
cleanup disabled must partition every supplied foreground pixel exactly once.

Maximal horizontal spans compress this graph: a span is a vertex; two spans
in neighboring rows have an edge when their inclusive horizontal intervals
overlap after expansion by one. A stack traversal labels reachable vertices.
Neighbors are discovered with interval binary searches; edges are not retained.
This replaces the previous single-pass union-find implementation.

The row directory uses direct indexing only for bounded, sufficiently occupied
heights (at most 65,536 rows); otherwise it indexes occupied rows sparsely.
This avoids allocating a table proportional to arbitrary staged-API coordinates.
Dense labels are grouped by stable bucket placement; arbitrary sparse public
labels use sorting instead of allocating by the largest label ID. Descriptors
and bitmaps are derived from the resulting span groups.

The public `Run`, `BBox`, `CC`, `ShapeRef`, `CCImage` and convenience functions
remain available. Compatibility includes half-open bounding boxes, cleanup
threshold values and an initially zero `nregularccs`. That last detail matters:
the old pipeline did not normally activate its reading-order sorter. Enabling
it changes legacy output bytes. Optional staged grid regrouping is implemented
separately and does not run in `analyze(0)`.

The implementation was written from the graph definition above and the existing
public API/behavioral contract; it does not incorporate an external component
implementation. The generic word-scanning bitmap helpers remain existing crate
code and are part of the broader source-history review.

## Evidence

- The four tests inherited alongside the old extractor were found verbatim in
  the DjVu crate and replaced with newly written contract tests. The nine
  component integration tests preserve run coverage, component partitioning,
  materialized geometry, deterministic ordering, explicit cleanup and staged
  API checks using independent test data and the pixel-graph oracle. The
  [before/after overlap reports](audits/2026-11-01/SOURCE-REVIEW.md) record the
  removed test block without reproducing its source.
- An independent pixel-by-pixel flood-fill oracle checks 216 deterministic
  random inputs over word boundaries and odd widths, plus blank, solid and
  diagonal images.
- Staged tests cover overlapping/unsorted/clipped spans, optional grid regrouping,
  and large sparse coordinates/labels. Regrouping preserves every foreground
  pixel once and keeps bitmap descriptor offsets valid.
- The original encoder byte-hash manifest passes unchanged. It was not refreshed
  to accommodate this replacement.
- The explicit exact preset bypasses component rejection and fuzzy matching;
  its own native/external decoder equality tests cover isolated marks, rules,
  dense regions and near-identical glyphs.

These are technical behavioral observations. The source diff, prior exposure,
retained API/constants, notices and wider module history still need the
appropriate provenance/licensing review before publishing is enabled.
