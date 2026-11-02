# Generic headroom re-run on an unloaded cloud machine — 2026-11-02

The two hypotheses that the [loaded-machine pass](GENERIC-06-HEADROOM.md)
rejected were re-measured on an idle 4-core cloud VM, together with one new
hypothesis suggested by the per-word blank batching. **Cached probability
estimates are retained**: byte-identical on every input, faster on every
input measured here. Batched renormalization is rejected again. The new
partial-word run batching saves instructions on pages but its benefit
depends on a compiler inlining decision that three near-identical variants
lost, so it is deferred and documented rather than shipped.

| Change | Status | Wall text29 | Wall scan14 | Wall noise | Instr. text29 | Instr. scan14 | Instr. noise | Instr. checker |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Cache Qe with each bitmap context | **retained** | 0.947 | 0.945 | 0.918 | 0.945 | 0.953 | 1.052 | 0.901 |
| Batch renormalization shifts | rejected | 0.974 | 0.981 | — | 1.028 | 1.036 | 1.181 | 1.000 |
| Zero-context runs inside partial words | deferred | 0.996 | 0.945 | — | 0.921 | 0.922 | 1.076 | 1.191 |
| Runs + cached Qe | deferred | 0.952 | 0.919 | 0.924 | 0.861 | 0.870 | 1.113 | 1.041 |

Ratios are candidate over the restored 0.6 baseline kernel; lower is better.
Wall columns are medians of interleaved pairs pinned to one core (eight
pairs for the retained change and the combination, five for the others);
instruction columns are cachegrind counts for one complete worker process
and are exact and repeatable. Another agent compiled and measured on the
other cores throughout, so individual pairs vary by about ±5%; every pair,
round count and raw time is in
[the measurement directory](benchmarks/2026-11-02-06-cloud). This machine
exposes no hardware counters, so cycles were not measured; cachegrind's
cache and branch figures are its model, not this CPU's.

## Inputs

No private corpus transferred to the cloud. The pages are two public arXiv
papers rendered by pdftoppm at 300 DPI to 2550×3300 bilevel PBM (29 pages,
"text29"), and 14 of those pages degraded with a 0.4° rotation, blur,
Gaussian noise and a re-threshold to resemble a scan (2574×3320, "scan14").
A one-page 50% random raster ("noise") and a one-pixel checkerboard
("checker") are dense controls with no zero-context runs at all; they are
stress inputs, not pages. PDF hashes, page hashes, preparation commands and
every worker binary's hash are in `environment.json`; the packed inputs use
the `GREGION1` layout of the `generic_kernel_worker` test, and every
candidate's output was compared byte-for-byte with the baseline worker's on
all four inputs.

## Cached Qe (retained)

Each bitmap context stores its probability-table index in the high half of
a 32-bit word and that state's Qe in the low half, so the common MPS path
reads no table and the context bank shrinks from 512 KiB to 256 KiB. State
transitions refresh both halves in the out-of-line renormalization routine.
This is the earlier candidate patch unchanged apart from formatting. Every
one of the eight text29 pairs and six of eight scan14 pairs are below one,
and all eight noise pairs are below one even though the noise raster
executes 5% more instructions: cachegrind reports 22–27% fewer simulated L1
data misses on the pages and 10% fewer on noise, and the smaller bank is
what the dense input is paying for. The loaded-machine pass saw a 1.2% wall
gain on its mixed corpus with a small cycle increase it could not separate
from load; the direction on real pages agrees across both machines. Output
bytes are unchanged on all inputs and the original 40-stream fixture
manifest passes without an update switch.

## Batched renormalization (rejected)

Shifting A and C by the leading-zero count in chunks up to each BYTEOUT
boundary again shows no consistent wall gain and raises the instruction
count on every input, by 18% on noise. Rejected as before.

## Partial-word runs (deferred)

The release kernel batches a word only when all 32 pixels and all incoming
context bits are zero. The context stays zero for exactly as long as the
incoming pixel bit and the two row-above window bits stay zero, and those
three bits are the tops of the registers the per-pixel update already
shifts, so the run length is the minimum of three leading-zero counts. A
kernel that batches such runs from any bit position, then codes the bit
that ends them on the ordinary path, is exact by construction (it reuses
`encode_zero_run`) and removes 8% of the instructions on both page corpora,
14% together with cached Qe. A new scalar-oracle test that scatters single
pixels so runs are cut at every bit position by each of the three rows,
including in an odd-width tail word, is retained in `generic_contexts.rs`
because it holds for any correct kernel.

It is not shipped, for three reasons recorded in the measurement
directory:

- The wall gain is confined to the scan corpus (0.919 with cached Qe versus
  0.945 for cached Qe alone); on born-digital text, which already consists
  mostly of fully blank words, it is within noise.
- Dense inputs pay on every pixel for a batching that never happens: 8%
  more instructions on noise and 19% on the checkerboard for the run change
  alone.
- The saving depends on the compiler inlining the run encoder into the
  pixel loop. Three variants that differ only in that encoder's shape — the
  loop out of line (`runscache2`), an inlined fast path with an out-of-line
  remainder (`runscache3`), and a single-OR run computation (`runscache4`)
  — execute 40–50% *more* instructions than the baseline on text29 (7.36,
  6.84 and 7.17 billion against 5.69), because zero-context runs on text
  are numerous and short, so a call per run costs more than coding the
  pixels. A gain that a hint-level `#[inline]` can lose to a compiler
  revision is not an unambiguous 0.6 change. Pinning the codegen and
  removing the dense-input cost is the follow-up, if the scan-corpus gain
  justifies it.

## Scope

This is the template-0 generic kernel, timed on preloaded packed pages with
coder initialization, coding, finalization, output copying and destruction
inside the timer; input loading and verification are outside. It is not the
PDF pipeline, symbol mode or a reader-device measurement. The final kernel
source hash is recorded in `environment.json` as `final_retained`; the
candidate patches are the `.diff` files beside it.
