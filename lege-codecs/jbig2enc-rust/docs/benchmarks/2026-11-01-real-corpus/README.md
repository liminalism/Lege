# Fixed local scanned-book and mixed-document evidence

Follow-up: [canonical arithmetic termination](../../ARITHMETIC-TERMINATION.md) removes redundant stop-marker bytes in the new presets. The separate `canonical-mixed` and `canonical-encoder-memory` reports update exact/measured bytes and whole-book RSS, with zero trailing-byte diagnostics in all 102 mixed-corpus PDF checks. Tables below remain their original pre-fix checkpoints; they were not rewritten as if measured on the later source. Legacy shipping presets retain their bytes.

The user supplied `risefallofconfed01daviuoft.pdf` from the local `to-sort` folder.
The file has 782 pages and SHA-256
`b4426498d43666c576c54937f75abc42d4740c66000d9216345fb54e993d9d3e`.
Versioned specifications in `tools/corpus_specs` pin source hashes, page counts,
selection, 240 DPI grayscale Poppler rendering and threshold 128 without dithering,
cropping or cleanup. Preparation manifests pin every resulting P4 PBM hash and
dimension. Fidelity is against those PBMs, not the original grayscale PDF.

The whole book includes aged/speckled text, dense pages, front matter and sparse
pages (PDF page 404 has nine black prepared pixels). The 350-page text set is
fixed at PDF pages 31–380. The mixed 17-page set adds mathematical notation from
`Introduction_to_Airborne_Radar.pdf` and Chinese/Latin glyphs from a four-page
book review. Those supplements contain vector text and mixed content; they are
not additional aged scans. Page probes were visually inspected. Poppler prepared
these inputs; no Lege-rendering result is claimed.

No PDFs, raster pages or diagnostic encoded streams are included in this report.
Only specifications, manifests, metrics and logs are retained. Reproduction needs
the user's matching local source files. These are distinct inputs and preprocessing
from the old README's `confed`/`sahib2` benchmark numbers.

## Whole-book grouping

All 782 pages were decoded natively and by jbig2dec for every group size. Each
run uses the same incremental loader, one source page outside the encoder at a
time. The caller sink verifies and discards each emitted group. All source and
cross-decoder pixel differences are zero.

| Maximum group pages | Bytes | Peak worker RSS (MiB) | Encode (s) | Cold decode sum (s) | Warm sum (s) | Largest decoded globals (bytes) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 32 | 22,135,580 | 77.2 | 9.77 | 5.74 | 5.01 | 81,196 |
| 64 | 22,107,587 | 142.9 | 9.75 | 6.25 | 4.85 | 130,752 |
| 128 | 22,083,670 | 270.6 | 9.81 | 7.43 | 4.77 | 260,840 |
| Whole book | 22,020,242 | 1,210.5 | 19.85 | 23.08 | 4.88 | 1,334,224 |

These single desktop runs show a useful resource/size tradeoff, not a universal
optimal group size. Groups of 32 cost about 0.52% additional bytes here.
Encoded global bytes in the CSV are summed across groups; decoded globals are
the largest single retained dictionary. Logs retain per-group and per-page work,
instance and residual measurements. Peak RSS includes source loading and decode
verification and excludes compilation; it is not an encoder-only allocation bound.
The whole-book checkpoint uses exact hash/equality ingest, without refinement;
later lossy-planner and refinement-offset changes do not explain these gains.

## Mixed-corpus fidelity and the failure it exposed

`mixed-original-failure` retains the initial stopped run. Shipping symbol mode
agreed through native/jbig2dec/Poppler but differed from PDFium on a `confed` page
when the expanded dictionary changed reference choices. The diagnostic comparison
found 9,870 native/PDFium differing pixels on that page. The original failed row
is not presented as a passed full-matrix result.

The tiny-reference regression fails on historical HEAD source too
(`tiny-refinement-before-fix.log`, first mismatch at pixel 4,2). Investigation found
that PDFium normalizes out-of-reference-height vertical refinement offsets.
The encoder now chooses the equivalent predictor alignment explicitly and recodes
the same target. `mixed-portable-shipping` passes all 17 pages with native,
jbig2dec, PDFium and Poppler for shipping/fixed-threshold text and unify modes and
the guarded unify preset. Existing golden hashes are unchanged.

| Initial mixed checkpoint mode | Bytes | Source pixel differences |
| --- | ---: | ---: |
| Generic | 381,082 | 0 |
| Shipping symbol | 292,954 | 2,864,894 |
| Fixed-threshold symbol | 273,047 | 2,864,894 |
| Shipping unify | 268,851 | 2,905,571 |
| Fixed-threshold unify | 245,901 | 2,917,502 |
| Guarded legacy unify | 276,758 | 2,894,208 |
| Exact symbols | 278,560 | 0 |
| Measured exact | 272,253 | 0 |
| Measured lossy, one-pixel instance cap | 272,253 | 0 |

At that checkpoint the lossy planner chose no substitution; enabling loss does
not force it. All new-mode rows in `mixed-exact` pass the three external oracles.
That checkpoint predates the short-reference predictor adjustment; the retained
source hashes state exactly what ran. The exact/refined regression suite also
passes after the adjustment. Large legacy source differences include omitted
cover/large components and small marks; these presets promise no input equality.

The pilot directories retain the earlier ten-page experiments. The first pilot's
parent session stopped after six CSV rows although its last worker finished;
`confed-optimized-rerun` supplies the complete isolated final-mode measurement.
On that sample the measured exact planner chose whole-page generic (195,093 bytes)
over direct exact symbols (207,609 bytes). Native and all three external oracles
passed the separate exact PDF pilot.

## Fixed 350-page text subset

`text350-groups` uses the same incremental loading strategy for every row.
Exact/generic rows use one 350-page group; legacy symbol/unify rows use groups of
32 with shipping `auto_thresh = true` and separate fixed-threshold controls.
Every completed row passes native/jbig2dec output equality. PDF renderer coverage
for these paths is the mixed/pilot corpus and synthetic policy regressions,
not every page of this 350-page matrix.

| Mode | Bytes | Encode (s) | Source differences | Peak worker RSS (MiB) |
| --- | ---: | ---: | ---: | ---: |
| Generic, whole group | 9,229,165 | 4.89 | 0 | 111.7 |
| Exact, whole group | 9,715,152 | 4.67 | 0 | 528.9 |
| Shipping symbol, groups of 32 | 9,284,877 | 64.73 | 2,458,881 | 58.6 |
| Fixed-threshold symbol, groups of 32 | 8,830,119 | 17.55 | 2,458,881 | 55.6 |
| Shipping unify, groups of 32 | 8,672,011 | 134.62 | 3,338,923 | 1,435.8 |
| Fixed-threshold unify, groups of 32 | 7,770,457 | 87.89 | 4,208,324 | 1,438.0 |

The CSV is authoritative for raw timings and RSS. `summary.json` adds per-page
median/p95/max cold/warm latency, selected instance/residual counts and comparison
ratios. Dictionary-group boundaries can change legacy matching choices, so lossy
rows must not be compared as though only their output container changed.
Grouping source pages alone does not cap the legacy unify pair cache: the high
RSS in that row is an observed limit of that legacy algorithm. The new measured
lossy planner uses bounded candidate discovery instead.

`text350-unbounded-stopped` retains an earlier unbounded worker that held unpacked
PBMs and array copies as well as encoder state. Its completed generic row reached
about 1,658 MiB RSS. The shipping-symbol worker logged 52,542,136 ingest comparisons,
41.37 s matching, 330.14 s clustering, 3,556,555 encoded global bytes and 20,633,788
decoded global bitmap bytes. It was explicitly interrupted during verification;
there is no completed shipping CSV row or full fidelity claim. Its planning timer
includes clustering, so those stage durations must not be added together.
The incomplete run is preserved rather than used as a performance pass.

No target reader was identified. Desktop cold decode includes globals parsing
on every page, without OS-cache flushing. Reader limits and release latency
guardrails still need measurements on the intended device. A 5–15% improvement
over the old release is not claimed from these figures.

`matched-head-pilot` compares the post-offset-hardening production checkpoint with the complete
historical HEAD production source on the ten-page `confed` pilot, using equivalent
runtime build settings and the same lockfile and timed loading/encode/decode
loops. Three randomized/interleaved pairs exclude compilation. Median candidate
ratios are 0.986 for encoding, 0.982 for cold decoding and 0.994 for warm decoding.
Source pixel-difference counts remain identical natively and in jbig2dec.
The reviewed predictor-compatibility adjustment changes output from 201,362 to
201,482 bytes; it is not hidden behind a regenerated golden manifest. Manifests
differ in licensing/packaging metadata, and their hashes are both retained.
This pilot passes the proposed local timing guardrails but does not establish
those guardrails for the unbounded whole-book legacy path or reader hardware.


## Online calibration and a measured resource-cap experiment

`mixed-calibrated` retains the later empirical correction of shortlist estimates.
It uses only earlier full-stream errors, separately by policy and representation,
to prioritize subsequent proposals. On exact trials, mean absolute prediction
error decreases from 7,191 raw bytes to 2,374 corrected bytes; positive predictions
decrease from 21 to 6. The estimate remains imperfect and every acceptance still
uses complete measured bytes. The explicit lossy search selects 20 substitutions,
saving 49 bytes while reporting 20 changed pixels. All native and external
oracles agree. These later results supersede the earlier no-substitution
observation for the current planner; the older checkpoint remains retained.

`mixed-constrained` applies an explicit 81,196-byte decoded-global bitmap cap,
taken from the measured largest globals footprint in the 32-page whole-book run.
This is an experimental cap, not a chosen reader capacity or shipping default.
The worker independently checks predicted bitmap storage against native retained
globals. Both measured presets choose a feasible 42,132-byte dictionary and zero
source differences under this cap. No numerical size/latency weighting is used.

| Planner / global bitmap cap | Output bytes | Actual global bitmap bytes | Source differences | Cold decode sum (s) |
| --- | ---: | ---: | ---: | ---: |
| Exact, unset | 272,253 | 1,416,552 | 0 | 0.603 |
| Explicit lossy, unset | 272,204 | 1,416,336 | 20 | 0.610 |
| Exact, 81,196 bytes | 278,560 | 42,132 | 0 | 0.077 |
| Explicit lossy, 81,196 bytes | 278,560 | 42,132 | 0 | 0.077 |

Here a roughly 2.3% size increase buys a much smaller decoded global dictionary.
These are independent desktop observations, not a reader latency guarantee. The
lossy option does not force substitution, and resource feasibility takes priority
over raw byte count. The timed maximum-compression encodes take about 12–13 s on
these 17 pages; the fast exact preset takes about 0.2 s. The slower search is
explicit and bounded; no default-preset performance benefit is claimed from it.


## Encoder process isolated from verification

`isolated-encoder-memory` measures a fresh encoding child for each group size,
then decodes every page with jbig2dec in the parent after the encoder has exited.
This keeps decoder verification memory out of the encoder process measurement.
The child loads one source page at a time and writes every encoded stream to a
temporary file sink. Stream hashes are retained; temporary streams are removed
by the measurement tool after verification. All 782 pages again have zero source
differences, with the same byte counts as the earlier group experiment.

| Group pages | Encoding-child peak RSS (MiB) | Verified source differences |
| --- | ---: | ---: |
| 32 | 79.2 | 0 |
| 64 | 143.1 | 0 |
| 128 | 269.9 | 0 |
| Whole book | 1,212.2 | 0 |

This process peak includes the source loader and output sink; it does not claim
library-only allocator accounting. These are independent observations, so small
RSS differences from the earlier combined worker are expected. The report does
not turn desktop memory observations into a device capacity or shipping budget.
