# Symbol-mode encoding speed — 2026-11-02

Nine byte-preserving changes cut symbol-mode encoding time on a scanned text
corpus to **0.34** of the baseline for `text()`, **0.58** for
`text_lossless()`, **0.40** for `text_lossless_max_compression()` and **0.39**
for `text_symbol_unify()` (median paired final/baseline time, one encoder per
page as the Lege PDF pipeline uses it). Every stream of every preset is
byte-identical to the baseline on both external corpora and the synthetic
fixtures, in per-page and whole-document scope, and the 40-stream
`original_encode_hashes.txt` manifest passes without its update switch. No
preset, default, dependency, thread or `unsafe` block is added, and
`src/encode/arith.rs` is unchanged.

Scanned corpus, 14 pages, one encoder per page (primary result):

| Preset | Baseline s/page | Final s/page | Median paired final/baseline (range) | Bytes (both) |
| --- | ---: | ---: | --- | ---: |
| `text()` | 0.1547 | 0.0519 | 0.337 (0.320–0.437) | 800,632 |
| `text_lossless()` | 0.0376 | 0.0220 | 0.584 (0.531–0.654) | 863,447 |
| `text_lossless_max_compression()` | 1.1179 | 0.4450 | 0.399 (0.386–0.423) | 813,198 |
| `text_symbol_unify()` | 0.2164 | 0.0837 | 0.387 (0.367–0.442) | 742,715 |
| `lossless()` (generic reference) | 0.0189 | 0.0142 | 0.763 (0.716–0.805) | 813,209 |

Other scopes and corpora:

| Corpus | Scope | Preset | Baseline s/page | Final s/page | Median paired ratio (range) | Bytes (both) |
| --- | --- | --- | ---: | ---: | --- | ---: |
| born-digital, 29 pages | page | `text()` | 0.0751 | 0.0280 | 0.379 (0.327–0.403) | 616,554 |
| born-digital, 29 pages | page | `text_lossless()` | 0.0227 | 0.0122 | 0.533 (0.503–0.560) | 622,223 |
| born-digital, 29 pages | page | `text_lossless_max_compression()` | 0.6629 | 0.2243 | 0.338 (0.331–0.354) | 621,128 |
| born-digital, 29 pages | page | `text_symbol_unify()` | 0.0738 | 0.0287 | 0.370 (0.363–0.453) | 514,755 |
| born-digital, 29 pages | page | `lossless()` | 0.0169 | 0.0122 | 0.719 (0.670–0.755) | 1,224,460 |
| scanned, 14 pages | document | `text()` | 0.2517 | 0.0935 | 0.371 (0.339–0.420) | 753,980 |
| scanned, 14 pages | document | `text_lossless()` | 0.0349 | 0.0181 | 0.506 (0.487–0.526) | 858,835 |
| scanned, 14 pages | document | `text_lossless_max_compression()` | 1.8199 | 0.8840 | 0.490 (0.480–0.491) | 811,696 |
| scanned, 14 pages | document | `text_symbol_unify()` | 0.5823 | 0.3521 | 0.602 (0.586–0.622) | 654,191 |
| scanned, 14 pages | document | `lossless()` | 0.0101 | 0.0078 | 0.772 (0.686–0.859) | 813,209 |

Times are medians of interleaved runs normalized per page. Ratios are
calculated within each pair and then medianed; a ratio need not equal the
ratio of the displayed medians. The three synthetic fixtures are also
byte-identical; at well under a millisecond per page their timings are noise
and are kept only in the raw data. `lossless()` gets faster only because
`first_black_pixel`, a self-check the generic coder runs before every region,
no longer reads pixels one at a time; the generic arithmetic kernel itself is
untouched.

## Where the time went

Profiles were taken with the `profiling` feature (pprof) on the scanned corpus,
one encoder per page, single Rayon thread. Inclusive sample shares:

| Share of samples | `text()` before | `text()` after | `text_symbol_unify()` before | after |
| --- | ---: | ---: | ---: | ---: |
| Hole counting in signatures | 47.5% | 7.2% | 45.7% | 10.0% |
| Clustering (`cluster_symbols`) | 30.7% | 14.9% | 21.9% | 9.1% |
| Text region with refinement (arithmetic) | 11.6% | 25.3% | 4.8% | 9.1% |
| Symbol dictionary (arithmetic) | 4.6% | 16.3% | 3.6% | 10.4% |
| CC extraction (`analyze`) | 4.6% | 9.3% | 3.4% | – |
| Unify class building | – | – | 19.3% | 37.0% |

Before, `text()` spent about half its time counting enclosed background holes
for signatures, with a per-pixel flood fill, and another third comparing symbol
pairs in clustering, almost none of which matched. After, the largest shares are
the arithmetic coding of refinement text regions and dictionaries, which lie in
`arith.rs`, outside this work. In `text_lossless_max_compression()` the
measured planner was 99% of the time before and after; per trial it repeated
the same whole-page generic, residual and dictionary encodes, and arithmetic
coding is now 66% of its samples (32% before). `text_lossless()` is now 61%
arithmetic coding and 19% CC extraction. Shares come from 137–4,366 samples per
profile (pprof delivered about 250 samples/s), so they are indicative to a few
points; the [attribution file](benchmarks/2026-11-02-symbol-speed/profile-attribution.json)
lists them per preset.

## What changed

Each change is a separate commit with its own tests. Rough single-change
effects come from paired runs during development and are less precise than the
final table.

1. **Run-based hole counting** (`classify.rs`). Enclosed white components are
   counted from the white runs of each packed row, joined between adjacent rows
   with a union-find; the per-pixel flood fill is kept as a test oracle over
   3,000 random bitmaps in both connectivities. `text()` and unify: about −40%.
2. **Word-level bitmap helpers** (`sym.rs`, `cc.rs`, `serialize.rs`,
   `document.rs`). `trim`, `crop`, `from_sub_image`, CC bitmap extraction,
   residual composition and `first_black_pixel` work on 32-pixel windows instead
   of `get_xy`/`set_xy`. A randomized test compares each with a per-pixel
   reference. `text()` about −17%, generic reference about −20%.
3. **Trial memo for the measured planner** (`symbol/memo.rs`). During
   `plan_optimized_exact_document`, whole-page generic payloads are reused by
   page index, residual generic payloads by full bitmap (compared exactly on
   every hit, bounded to 16M retained words), and plain dictionary encodings by
   their ordered symbol indices. These inputs cannot change while the planner
   runs and the memo is dropped afterwards. `text_lossless_max_compression()`
   about −53%.
4. **Lazy local-gain scan** (`planning.rs`). A per-page sum of
   `estimate_local_symbol_gain`, quadratic in symbols × instances, is now
   evaluated only when a page has at most two instances, the only case that
   reads it. `text_lossless()` about −33%.
5. **Holes only where read** (`classify.rs`, `document.rs`, `clustering.rs`).
   Only the family compatibility test reads `hole_count`. Plain ingest matches
   with the hole-free signature and completes it once for each new symbol;
   clustering carries prototype signatures over instead of recomputing them (a
   debug assertion checks them). `text()` about −12%.
6. **Narrow alignment search** (`comparator.rs`). For bitmaps up to 64 pixels
   wide each row is one `u64`; the shifted, zero-filled, word-masked XOR equals
   the general kernel's. Same offsets, order, early exits and tie rule; a test
   compares it with the general path for all width combinations. Measured −3%
   (`text()`) and −4% to −9% (unify); the extension from 32 to 64 pixels gave no
   further measurable gain on this corpus.
7. **Clustering pair bounds** (`clustering.rs`, `comparator.rs`). Buckets are
   visited in ink-count order so only pairs within the largest usable error
   limit are enumerated, and for bitmaps up to 32 pixels wide with `dy = 0`,
   `|dx| <= 1` the XOR error of every alignment is bounded below by row and
   column ink-count differences. When the bounds exceed the limit for every
   offset, the search is skipped. Clusters are the connected components of the
   accepted pairs and do not depend on visiting order. About 98% of searched
   pairs are now settled by the bound; clustering time halves.
8. **Row-sweep CC labelling** (`cc.rs`). Canonical spans of consecutive rows
   are joined by one merge pass into a union-find and numbered by first span,
   which is the numbering of the previous traversal; a test compares them on
   random bitmaps. CC extraction about −35%.
9. **Unify prescreen bound and cache** (`unify.rs`, `comparator.rs`). The bound
   now covers `dy` in −1..=1 and settles unify prescreens whose window cannot
   contain an acceptable offset. The pair cache treated cached rejections as
   misses and did not record final comparison failures; both are now cached.
   Unify about −8%.

The CC module note previously said the labelling used no union-find; it now
describes the sweep. The algorithm is a standard two-row run labelling written
for this change; it does not alter the licensing status recorded in
PROVENANCE.md.

## Byte identity and fidelity

The harness hashes every globals and page stream (FNV-1a 64). All 20
preset/corpus/scope combinations have the same hashes and byte totals before
and after (see `summary.json`). Decoding every per-page stream with the crate's
own decoder gives identical differing-pixel counts: zero for both exact presets
and `lossless()`; for `text()` 274,639 (scanned) and 475,025 (born-digital); for
`text_symbol_unify()` 365,783 and 626,423. These counts are properties of the
unchanged lossy presets, not of this work. `encode_byte_identical` passes
without `UPDATE_ORIGINAL_ENCODE_HASHES`; the legacy external gate stays ignored
as before.

## Measurement scope and caveats

The machine is a shared four-vCPU cloud VM (Intel Xeon at 2.1 GHz). Another
agent measured on CPU 0, so runs were pinned with `taskset -c 2,3`; nothing was
compiled during the final runs, but other tenants were not controlled. The
baseline is the branch point b81b173 plus the harness commit, with no encoder
change; the final build is ff9a6bd. Both were built with the release profile and
`--features parallel` (Lege enables `parallel`); with two pinned CPUs the
per-page scope has no page-level parallelism to use. Scanned per-page runs use
seven interleaved, shuffled pairs; the other scopes use five. No run was
removed. PBM loading, input cloning, hashing and decode verification are outside
the timer; encoder construction, `add_page_bitimage` and `flush_pdf_split` are
inside.

The corpora are two arXiv papers rendered at 300 dpi with pdftoppm (29 pages,
2550×3300) and 14 of those pages degraded to resemble scans (slight rotation,
blur, noise, re-thresholding; 2574×3320). They are not redistributable and are
not in the repository; `environment.json` records their SHA-256 identities.
They are small and English-only: they say nothing about long books, other
scripts or real scanner noise, and the measured planner's trial budget makes its
time depend strongly on the number of distinct symbols per page.

Reproduce with:

```sh
SYMBOL_SPEED_DIR=/path/to/pbms SYMBOL_SPEED_SCOPE=page SYMBOL_SPEED_ROUNDS=1 \
  taskset -c 2,3 cargo test --release --features parallel --test symbol_speed \
  -- --ignored --exact symbol_speed_worker --nocapture
```

`SYMBOL_SPEED_VERIFY=1` adds native decoding and differing-pixel counts;
`SYMBOL_SPEED_PROFILE=/path/prefix` with `--features parallel,profiling` writes a
flamegraph per preset.

## Tried and not kept

- **Row-profile bound alone.** It settled 53% of the clustering searches; with
  the column bound added the share rose to 96–98%, so both are used.
- **A first bound implementation** that copied and rebuilt column counts per
  pair cost about as much as the search it skipped (pair phase 6–27 ms per
  page, barely below the 8–37 ms before). Precomputed padded profiles fixed
  that.
- **Dictionary prefix reuse in the measured planner.** Most trials change one
  symbol, so the arithmetic state before it could be reused, but that needs a
  cloneable `Jbig2ArithCoder`, which lives in `arith.rs`. Not attempted.
- **Context-similarity caching in unify.** `jensen_shannon_divergence` sums
  floats in hash-set order; reorganizing it risks changing decisions and bytes.
  Left unchanged.

No change that alters output bytes was made or is proposed here.

## Remaining opportunities

In rough order of expected value:

1. Refinement-region and generic-region context formation in `arith.rs`: the
   refinement coder reads each context pixel with `get_pixel_safely`; it is now
   the largest share of `text()` (25%). Word-wise context construction there,
   like the generic kernel's, belongs to the arithmetic-kernel owner.
2. Dictionary encodes in the measured planner (50% of
   `text_lossless_max_compression()`): prefix checkpoints of the coder state,
   or fewer distinct trial dictionaries.
3. Unify class building in document scope: context similarity is 39% and the
   pair-cache inserts about 18% of `text_symbol_unify()` on a 14-page
   document, which is why that row improves least (0.60).
4. Text-region payload reuse between the all-global and globals-plus-locals
   trials of a single page (16% of the measured planner).
5. CC extraction for exact presets (19% of `text_lossless()`), mainly span
   grouping and descriptor construction.

Raw timings (`raw.csv`), the per-combination summary (`summary.json`),
fidelity counts (`fidelity.csv`), profile shares, environment, input hashes and
intermediate comparator runs are in
[the measurement directory](benchmarks/2026-11-02-symbol-speed).

## Follow-up: the three ranked opportunities — 2026-11-02, later the same day

Three further byte-identical changes, measured with the same harness and
`taskset -c 2,3` against the final build above as the baseline (interleaved
pairs, medians; raw lines in `followup-*.log` beside the other data):

| Change | Preset, scope | Ratio (range, pairs) |
| --- | --- | ---: |
| Dictionary prefix checkpoints in the measured planner | `text_lossless_max_compression()`, page | 0.828 (0.789–0.843, 3) |
| Context similarity after the bitmap screens | `text_symbol_unify()`, document | 0.598 (0.547–0.839, 3) |
| Context similarity after the bitmap screens | `text_symbol_unify()`, page | 0.807 (0.774–0.828, 3) |
| Windowed refinement contexts | `encode_refinement_region`, per pixel | 0.59 (0.56–0.68, 10 rounds) |
| Text-region payload memo in the measured planner | `text_lossless_max_compression()`, page | 0.890 (0.841–0.935, 4), on top of the checkpoints |
| Text-region payload memo in the measured planner | `text_lossless_max_compression()`, document | 0.897 (0.873–0.922, 2), on top of the checkpoints |

Every preset's output hash and byte total is unchanged on both corpora.

**Dictionary prefix checkpoints** (`dictionary.rs`, `memo.rs`, `arith.rs`).
The dictionary encoder is now a stream: symbols are pushed in canonical
order and height classes, width deltas and bitmaps are coded as each one
arrives, so the state after any symbol is a resumable prefix. During a
measured planning call the trial memo keeps, for the last three canonical
symbol sequences it coded, a clone of the stream every N/16 symbols (at least
every 8). A trial whose sequence shares a prefix with one of them resumes
from the last checkpoint inside the common prefix and codes only the
remainder; since the planner's proposals drop one symbol or one family from
the current best, that remainder is on average about half the dictionary.
Checkpoints are shared by reference between sequences, so memory stays
within about sixteen coder states per sequence. The arithmetic coder gained
`Clone` for this. A test encodes related symbol sets through the cache and
through the one-shot encoder and requires identical bytes.

**Context similarity after the bitmap screens** (`unify.rs`). The class
builder asked the context model for a merge decision on every bucket
neighbour before trying the pair comparison, and the decision walks six
token maps per pair (weighted Jaccard, cosine and Jensen–Shannon on both
sides). Pairs now go through the cached ink-profile screens and the
comparator first, and only survivors consult the context model. An accepted
edge needs both verdicts, so the classes are the same in either order; what
changes is the diagnostic attribution of pairs both checks would have
rejected, which now count as comparison rejections.

**Windowed refinement contexts** (`arith.rs`). The refinement coder read all
thirteen context pixels per target pixel through bounds-checked lookups. It
now keeps three-pixel windows on the three reference rows and a two-pixel
window on the target row above, and slides them one pixel per column; the
nominal adaptive pixel is the previous column's bit 11, and any other AT is
one bounded read. A 400-case oracle test (random sizes, offsets up to the
`i32` extremes, every AT variant and both templates) compares its bytes
with the per-pixel formation, which is kept as the oracle. This does not
move the `text()` row: that preset never refines, and the 25% share labelled
"text region with refinement" in the profiles above is the text region's
integer coding, not refinement bitmaps. It helps the presets that refine
instances (`text_symbol_unify_safe()`, the lossy and measured planners when
they choose refinement).

**Text-region payload memo** (`memo.rs`, `planning.rs`). The all-global and
globals-plus-locals controls of a one-page document assign every symbol the
same dictionary position, and most proposals leave most pages' instances
untouched, so trials keep coding identical text regions. The trial memo now
keys each region payload by everything its coder reads: page, coder
variant, the symbol-code length input and, per instance, its dictionary
position, symbol, placement and refinement plan; a hit is compared on the
full key, not only its hash. The two measured planner changes together
bring the preset to about 0.74 of the earlier build.

**Component extraction, not changed.** A pprof profile of `text_lossless()`
(`followup-text-lossless-profile.txt`) puts extraction at 26% of the
preset, split across span labelling (11%), descriptor grouping (8%), run
extraction (7%) and bitmap extraction (3%), with dictionary coding at 50%.
No single stage offers a byte-identical change large enough to show
through the measurement noise, so nothing was done there.

Noise note: a 3–6% wall-clock drift on `text_lossless()` appeared between
workers that do not touch its code path; cachegrind counts 523.6, 523.6 and
524.0 million instructions for the baseline, refinement and prefix-cache
workers on two pages, so it is layout or scheduling noise, not work.
