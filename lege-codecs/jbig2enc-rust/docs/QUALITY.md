# Fidelity and measurement contracts for 0.7

`encode_single_image_lossless()` and `Jbig2Config::lossless()` retain generic
region encoding and must preserve every supplied bilevel pixel.

`Jbig2Config::text_lossless()` is the explicit exact symbol preset. It retains
all components, including single pixels, without cleanup; dictionary reuse
requires full bitmap equality. Other components are stored separately or
generically. It bypasses fuzzy clustering/aliasing even if their tuning switches
are subsequently enabled. Conflicting substitution/composition settings fail
before ingest. The emitted exact text/generic regions use their lossless segment
types. Existing generic API byte baselines remain unchanged.

`Jbig2Config::text()` retains its existing settings and byte baselines. It does
not promise equality to the supplied input: component selection can omit marks.
Per-instance refinement (`SBREFINE`) is implemented and corrects retained
instances, but does not restore components omitted during extraction.
`text_symbol_unify()` intentionally permits substitution and must report actual
pixel differences. Pixel equality after optional preprocessing would be a
separate contract; it is not equality to the supplied input.

`text_symbol_unify_safe()` adds a final topology guard after legacy remapping:
changes in foreground connectivity, enclosed background holes or thin endpoints
force refinement. Background uses complementary four-connectivity. This legacy
mode still filters components and its candidate search is not fully bounded.

`text_lossy_max_compression(max_symbol_error)` explicitly adds direct substitution
to the bounded measured planner. It uses the same component-preserving ingest as
exact mode and a separate flag that exact presets reject. Substitution requires
equal dimensions, zero offset, a bounded pixel difference and equal topology.
Exact storage, refinement and generic coding remain available. Topology checks
reduce specific risks; they do not prove character identity. Actual instance
types and source differences must be reported. The one-pixel regression family
selects 10 refined and 14 substituted instances: 438 bytes versus the 763-byte
dictionary baseline, with 14 differing source pixels. The exact planner selects
492 bytes with zero differences for that fixture.

Run the reproducible benchmark from any directory:

```sh
python3 tools/quality_benchmark.py --output /tmp/jbig2-quality --pages 350
python3 tools/quality_benchmark.py --output /tmp/jbig2-scans --corpus /absolute/pbm-corpus --require-oracle
```

The first command generates version-1 synthetic geometric stress pages with
text-like shapes, noise, varied symbol shapes, mathematical marks, sparse
components, isolated dots, and odd widths. These are original generated shapes,
not scanned documents or a multilingual font corpus. They cannot establish
representative book compression or reader hardware performance. Release work
now also has fixed local PDF specifications in `tools/corpus_specs`: the user's
782-page `confed` scan, a fixed 350-page text subset, and a mixed supplement with
mathematics and Chinese/Latin glyphs. The latter supplements include vector text;
they are not represented as aged scans. Source PDFs and rasters are not shipped.
See the retained preparation manifests and benchmark reports for identities.

Each run saves SHA-256 identities of every input, configuration-specific logs,
per-page results, toolchain/source metadata and `results.csv`. Shipping text and
unify presets are separate from `auto_thresh = false` controls. Every stream is
decoded natively; generic lossless requires zero differing pixels. Where
`jbig2dec` is available, its decoded pixels must equal the native decoder's,
and its differences from the input are reported too. `--require-oracle` fails
if that tool is unavailable. Missing oracles are never reported as a pass.

Encoding time excludes decode verification. Cold decode parses globals anew
for every page; warm decode reuses decoded globals and a primed worker context.
These are library measurements without an OS cache flush. Dictionary bytes
count retained bitmap storage, not total allocator overhead. Linux `wait4`
reports each isolated worker's peak RSS, including source loading and
verification, excluding compilation. It is not encoder-only peak memory.
The PDF policy regression test also wraps odd-width streams in PDFs and checks
PDFium and Poppler. Poppler monochrome sampling is compared with a PDF of the
uncompressed source under the same transform; PDFium additionally matches
source pixels directly. This separates renderer sampling from codec fidelity.
`QUALITY_PAGE_RESOURCES` logs selected exact/refined/substituted/generic instances,
generic region area and source ink, and decoder work proxies. `QUALITY_MATCHING`
reports input-component counts and ingest/planner comparisons. Legacy post-ingest
clustering/unification comparisons remain unreported; the CSV comparator field
must not be described as a total across all stages. Encoder-only live memory
remains distinct from process RSS.

To require all three external oracles in the policy regression:

```sh
JBIG2DEC=/path/to/jbig2dec PDFTOPPM=/path/to/pdftoppm \
PDFIUM_PYTHON=/path/to/python-with-pypdfium2-and-Pillow \
JBIG2_REQUIRE_PDF_ORACLES=1 cargo test --test pdf_dictionary_policy
```

`tools/pdfium_oracle.py --version` records the wrapper and PDFium versions.
Poppler currently reports one trailing arithmetic byte on some symbol segments;
the warning is emitted in test logs and remains part of the release audit.

## PDF dictionary policies

`config.pdf_dictionary_policy` offers `AllGlobal` (the unchanged preset default),
`SharedGlobal` (cross-page dictionary plus generic residuals), and
`SharedGlobalWithLocal` (cross-page dictionary plus page-associated dictionaries).
Pages with only local dictionaries need no globals stream. Dictionary references
use actual segment numbers. These settings leave standalone behavior unchanged.

These remain explicit policies in the ordinary presets. The slower
`text_lossless_max_compression()` preset measures all three, rather than assuming
local dictionaries win. [Recorded comparisons](benchmarks/2026-11-01-dictionary-policies/README.md)
show why merely enabling local dictionaries is not enough.

## Measured exact optimization

The maximum-compression preset starts from exact component ingest. Signature
buckets, usage-ranked anchors and the existing comparator discover near matches;
`symbol_candidate_limit` bounds examined references per symbol (default 16).
At most two near-match proposals per symbol reach the stream trials. Shapes
larger than 256 by 256 skip fuzzy discovery and remain eligible for dictionary
or generic coding. Exact hash/equality reuse remains independent of that search.

`symbol_trial_limit` bounds full-document encodes (default 64, minimum 4). The
controls are all-global, shared-global, globals-plus-locals and whole-page generic.
Trials compare exact dictionary reuse, storing variants separately, reference
plus per-instance refinement, and generic residuals. Family proposals amortize
fixed SBREFINE signaling costs before individual greedy changes. Each measured
proposal rebuilds dictionaries, symbol IDs, text regions and residual segments;
serialized headers and standalone/PDF stream boundaries are included in its cost.
Without resource limits, only strict byte savings are accepted. With explicit
limits, feasibility takes precedence and the smallest measured feasible candidate
is retained; no feasible result means an error. Stored references cannot be removed
underneath already accepted refinement assignments.

This is a bounded heuristic search, not a proof of the global minimum. The
shortlist estimate is deliberately coarse. `EncoderMetrics::optimization` and
`QUALITY_TRIAL` logs expose estimated gains alongside actual measured gains,
so calibration errors are visible. Predictions are corrected online by the
median error of earlier complete-stream trials, separated by dictionary policy
and family/single refinement/substitution representation. The corrected values
prioritize family and individual proposals within the same hard trial budget;
they never authorize an unmeasured substitution or size increase. Both raw and
corrected estimates remain in the report. On the mixed corpus, the exact search's
mean absolute prediction error falls from 7,191 to 2,374 bytes and positive
predictions from 21 to 6. This is observed within-run calibration, not a portable
accuracy guarantee or an additive arithmetic-coding cost model.
At the retained pre-canonical-termination checkpoint, in the regression family fixture, individual
changes lose bytes while a coordinated family shrinks 763 to 492 bytes. Selected
refinement reproduces every source pixel through native and external decoders.
On the retained pre-termination 350-page synthetic corpus the optimizer retains the 82,460-byte
exact dictionary result; it does not claim a compression win where none was
measured. The slower preset has a separate CPU budget from the shipping preset.

The new measured lossy preset uses this bounded discovery and trial budget too.
The legacy text/unify algorithms retain their previous search behavior.

## Bounded groups and caller resource limits

`PdfGroupEncoder` takes owned `BitImage` pages incrementally and emits an
`EncodedPdfGroup` to a caller sink. Each group contains its own globals and page
streams, `group_index` and zero-based `first_page`. Every page in that value must
use that value's globals object in the containing PDF. Groups are independently
embedded image streams, so segment numbers may repeat between groups.

`PdfGroupOptions` requires a positive `max_pages` and optionally bounds retained
source bitmap bytes. A page larger than that byte cap is rejected before changing
the accepted prefix. Source pages, symbols and instance bitmaps are dropped before
the sink runs. The sink must not accumulate outputs if bounded application memory
is required. Call `finish()` explicitly; drop does not flush. Encoding and sink
errors are terminal because a sink may already have written PDF objects.
At a byte-cap boundary, the arriving owned page can coexist with the preceding
group while it flushes; each is individually within the byte cap. The option
bounds one group's source bitmaps, not that transient sum or the caller's loader.

These bounds do not promise a maximum process RSS: extracted instance bitmaps,
trials, arithmetic contexts and output buffers also consume memory. The benchmark
group worker loads one source page at a time for both bounded and whole-book runs.
The 32/64/128/whole-book experiment therefore compares the same loading strategy.

`config.decoder_resource_limits` accepts optional caps on encoded globals, decoded
global bitmap storage, per-page global-plus-local bitmap storage, and cold page
work. Defaults impose no reader budget. Exact/measured lossy optimization chooses
the smallest measured feasible candidate; fast modes check their chosen plan and
fail when it exceeds a cap. The API does not silently split or retry a failed sink.

Bitmap storage uses the native decoder's word-aligned rows; it excludes allocator
and arithmetic-context overhead. The cold-work proxy sums global/local dictionary
areas, text source areas (plus reference area for refinement) and emitted generic
region areas. It counts repeated work and blank pixels, and is not milliseconds
or a total decoder-memory bound. Generic source ink is a per-instance sum, not an
overlap union. Reader measurements must determine caller limits; none are invented
from desktop timings.

To reproduce the grouping experiment after preparing the fixed corpus:

```sh
python3 tools/quality_benchmark.py --corpus /absolute/confed-pbms \
  --output /tmp/confed-groups --require-oracle \
  --modes exact_group32 exact_group64 exact_group128 exact_groupwhole
```

## Refinement interoperability

New presets also use [canonical arithmetic termination](ARITHMETIC-TERMINATION.md).
This removes a duplicated `FF` before the `FF AC` stop marker, resolving
Poppler's trailing-byte diagnostics in the tested new modes. Legacy presets
keep their byte baselines and can select it explicitly through
`canonical_arithmetic_termination`. Generic, dictionary, text/refinement and
arithmetic halftone payloads all honor the option; it does not change pixels.

The mixed corpus exposed a legacy PDFium disagreement on short refined symbols.
PDFium's optimized refinement path normalizes vertical offsets outside the
reference height to zero. The encoder now chooses that alignment explicitly and
codes the target pixels with it, so native, jbig2dec, PDFium and Poppler agree.
This is an encoder compatibility adjustment, not a change to source pixels or
decoder conformance rules. The original byte-hash fixtures remain unchanged.
The synthetic tiny-reference regression exercises both signs of the offset;
the original real-corpus failure is retained in the report rather than discarded.

Primary implementation evidence:
[PDFium refinement decoder](https://pdfium.googlesource.com/pdfium/+/refs/heads/chromium/7113/core/fxcodec/jbig2/JBig2_GrrdProc.cpp).
No implementation from that decoder is copied into the encoder.

The broader decoder suite retains a separate, pre-existing GRTEMPLATE-1/TPGRON
disagreement with jbig2dec 0.20. Historical HEAD reproduces it; independent PDFium
rendering matches the native result. The differential test is kept intact, and
the remaining functional suite is reported with its explicit filter rather than
called an unqualified all-oracle pass. Production symbol encoding emits template
zero without TPGRON. See the retained validation logs for the exact scope.

## Residual packing

Set `config.pack_residuals = true` to measure alternative generic residual
layouts. Existing presets leave it off to retain their byte baselines.
The encoder compares one bounding rectangle, individual component rectangles,
256 by 128 spatial cells and 64-row bands, including actual segment-header
bytes. It selects only a strict byte saving. At most 64 regions are emitted;
individual layout trials are skipped above that component count. OR composition
preserves overlapping component pixels. Other composition operators keep the
single-region path. Page planning reserves segment numbers before the end of
page, including for parallel page encoding.

Trial encoding takes CPU time even when no smaller candidate wins. The generic
stage timer includes layout construction and every candidate encode. The
release residual tests report bytes and CPU time, check dense and sparse
layouts, overlapping components, empty instances, odd widths, region limits,
four-byte page association and ordering in the page builder. Corpus performance
and independent-renderer checks remain required before choosing a shipping
optimization preset.


## Isolated encoding memory

`tools/measure_encode_memory.py` runs encoding in a separate child, measures that
child with Linux `wait4`, and verifies every emitted page through jbig2dec only
after the child exits. Source loading is incremental. The child's peak includes
its input loader, codec, output buffers and file sink; it excludes parent and
decoder/PDF verification allocations and compilation. It is still process RSS,
not an attribution of allocated bytes exclusively to the library.

The complete 782-page experiment has zero source differences in all four group
sizes. Encoder-worker peaks are 81,056 / 146,512 / 276,336 / 1,241,324 KiB for
32 / 64 / 128 / whole-book groups, respectively. These measurements are retained
separately from the earlier combined-worker RSS. Encoded streams are temporary;
SHA-256 stream manifests remain after parent verification. Missing jbig2dec is
an error, and no unverified memory-only run is presented as a fidelity pass.

```sh
JBIG2DEC=/path/to/jbig2dec python3 tools/measure_encode_memory.py \
  --corpus /absolute/confed-pbms --output /tmp/confed-encoder-memory --require-exact
```
