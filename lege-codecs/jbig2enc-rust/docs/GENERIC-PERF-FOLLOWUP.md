# Generic encoding with hardware profiling — 2026-11-02

Two further optimizations reduce generic-kernel time by **50% on mixed17** and
**42% on scanned text350**, compared with the previously verified C-parity Rust
kernel. Every raw payload remains byte-identical. File size and coding modes
are unchanged; no API, dependency, license or threading change is introduced.

| Prepared input | Previous Rust seconds/round | New Rust | C | Median paired new/previous | Median paired new/C |
| --- | ---: | ---: | ---: | ---: | ---: |
| Mixed 17 pages | 0.1233 | 0.0618 | 0.1307 | 0.4993 | 0.4668 |
| Scanned text 350 pages | 2.6936 | 1.5733 | 2.9683 | 0.5841 | 0.5610 |
| Procedural 50% noise, one page | 0.0491 | 0.0461 | 0.0431 | 0.9297 | 1.0681 |

Times are medians normalized by timed rounds. Ratios are calculated within each
pair, then medianed; a ratio need not equal the ratio of the displayed medians.
The random image is a stress input, not a book. It improves about 7% but remains
about 7% slower than C, so these results do not establish universal C parity.

## What changed

About 70% of pixels in the prepared mixed corpus fall in words whose pixels and
incoming template context bits are all zero. The encoder now batches those
words instead of executing the context/pixel loop 32 times. It retains the exact
per-bit probability model. For an MPS-zero state with probability estimate Qe,
a run of n normalized MPS steps changes A to A − n×Qe and C to C + n×Qe, with
the context state unchanged. Batching stops before A would fall below 0x8000;
the ordinary coder then performs that bit's state transition and renormalization.
MPS-one states also use the ordinary path until eligible. Runs are bounded to
one input word, and unused input padding stays invisible.

The hot arithmetic path also loads only Qe. Transition fields are fetched in
the uncommon transition routine. Seven paired mixed-corpus comparisons against
blank batching alone show a median ratio of 0.9873, with every pair below 1.
An attempt to avoid unchanged context writes gave a negligible/noisy result
and was reverted. The earlier byte-sized context-bank experiment is also not
part of this change.

The scalar coordinate oracle now includes million-pixel blank runs and sparse
interruptions at word/row boundaries, exercising low-Qe states and repeated
renormalization. Existing odd-width/dirty-padding tests and full-stream byte
manifests are retained.

## Hardware evidence

Linux perf counters are now available with perf_event_paranoid=0. Sampling
cpu_core/cycles/u at 997 Hz attributes 87.92% of the previous worker's samples
to the generic pixel loop, 10.40% to transitions/renormalization and 1.52% to
byte output. The new worker's corresponding shares are 73.75%, 21.96% and
3.75%. Neither capture reports lost samples. These are sampled attribution
shares, not precise latency measurements for individual annotated instructions.

| Mixed17 counter, same 40-round worker sequence | Previous | New |
| --- | ---: | ---: |
| User cycles | 22,619,811,056 | 11,313,912,430 |
| User instructions | 94,085,766,668 | 34,888,365,570 |
| User branches | 12,265,109,873 | 5,026,660,014 |
| User branch misses | 192,109,806 | 156,960,571 |

Cycles fall about 50% and instructions about 63%. The separate three-repeat
noise check reduces cycles from 3,937,029,781 to 3,657,672,308 (about 7.1%),
with reported cycle-count variation below 0.21%. Branch misses stay nearly
constant there. Counters cover the complete worker process, including one
warm-up and input loading, rather than only its internal encode timer. Sampling
and counter runs are separate from timing comparisons.

The remaining cost is chiefly nonblank context processing, followed by
renormalization and byte output. Batching renormalization shifts or caching
probability data may offer further headroom; neither is claimed as a measured
gain here.

## Measurement scope and reproducibility

The same template-0 packed kernel contract, pinned C revision
8673b046bac6441b0702dec30dfdf0af6023a3aa, default CPU targets and compiler options
as [the earlier report](GENERIC-SPEED.md) are used. Both duplicate-line prediction
and profiling are off in timing runs. Input preparation/loading and verification
are outside the timer; coder initialization, encoding, finalization, output
copying and destruction are included. This measures the kernel, not the full
application/PDF pipeline. The prior Rust source hash matches the previous
verified source inventory.

This continuation uses freshly prepared **Ghostscript 10.06.0 grayscale,
240-DPI, threshold-128** rasters, because the temporary Poppler tools/inputs
were absent. These are new input identities, not the earlier Poppler rasters.
The original PDF hashes and page selections are unchanged. The same new packed
inputs are shared by every compared worker. Procedural solid/checkerboard/noise
images separately exercise dense inputs without blank-span opportunities.

All processes use CPU 0 on the i7-13700H, with seeded interleaved ordering among
the previous Rust worker, new worker and C. Mixed17 uses five pairs of eight
rounds; text350 uses three pairs of two; noise uses three pairs of twelve. The
powersave governor, frequency and unrelated system activity are not controlled.
Text350 new/previous ratios range from 0.520 to 0.637; the dense three-image
aggregate is noisy (0.809–1.258), and is retained without claiming a consistent
improvement. No pairs are removed.

The runner now accepts --baseline-worker to check complete, unmodified Rust
payload equality as well as C equality. Only the known legacy duplicate FF
termination byte is normalized for C comparisons. All 367 real pages and the
dense/noise controls pass both checks. Raw images, payloads, C sources/binaries
and perf.data stay in ignored scratch; only hashes, timings, preparation records
and profiler text are retained in
[the measurement directory](benchmarks/2026-11-02-generic-perf).

For a repeat, build the worker as described in the earlier report, then use:

```sh
python3 tools/compare_generic_kernels.py --corpus /path/to/fixed-pbms --worker /path/to/new-worker --baseline-worker /path/to/previous-worker --reference /path/to/c-runner --output /path/to/new-results --rounds 2 --pairs 5
GENERIC_INPUT=/path/to/packed-input.bin GENERIC_ROUNDS=40 perf stat -e cpu_core/cycles/u,cpu_core/instructions/u,cpu_core/branches/u,cpu_core/branch-misses/u -- taskset -c 0 /path/to/new-worker --ignored --exact generic_kernel_worker --nocapture
```

The counter event names are specific to this Intel hybrid processor. The
[validation report](validation/2026-11-02-generic-perf) records regression and
package checks separately.
