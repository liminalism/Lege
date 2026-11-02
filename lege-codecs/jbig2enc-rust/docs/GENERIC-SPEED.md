# Generic-region speed — 2026-11-01

A [2026-11-02 follow-up](GENERIC-PERF-FOLLOWUP.md) records further byte-preserving
blank-span and arithmetic-load optimizations. The measurements below describe
the earlier checkpoint.

The template-0 packed generic kernel now reaches C parity on the two measured
corpora. The median paired Rust/C elapsed-time ratio is **0.943** on mixed17
and **0.973** on the 350-page scanned-book text corpus (lower is faster).
All 367 raw Rust page payloads are byte-for-byte unchanged from the pre-speed
worker. Compression size is unchanged.

| Corpus | Pre-speed Rust, seconds/round | Optimized Rust | C in final run | Median paired Rust/C |
| --- | ---: | ---: | ---: | ---: |
| Mixed 17 pages | 0.3018 | 0.1443 | 0.1518 | 0.943 |
| Scanned text 350 pages | 4.9073 | 2.5862 | 2.6114 | 0.973 |

Times are medians normalized by timed rounds. The Rust median time drops about
52% and 47%, respectively. Baseline and final runs are separate processes; the
paired ratio is calculated within each final C/Rust pair, then medianed.

The scan rolls a single 16-bit context through packed 32-pixel windows, rather
than sampling preceding rows for every pixel. Arithmetic registers are borrowed
separately from context statistics. The common MPS path returns before the
out-of-line state-transition/renormalization path. Range register A uses native
32-bit storage with explicit 16-bit semantics: this avoids the overlapping
16-bit stores and widened loads seen in the original generated loop. No unsafe
access, target-specific instructions, hidden threads or new stream mode is used.

The early byte-sized context-bank experiment was slower and was reverted.
The smaller scan-loop changes alone did not close the gap. The native-width
range-register change produced the largest measured improvement; see the
[experiment ratios](benchmarks/2026-11-01-generic-speed/experiments.json).

## Measurement scope

Both workers preload identical packed pages, warm up once, then time context
initialization, template-0 coding with duplicate-line prediction **off**,
finalization, output copying and destruction. Input conversion, loading,
verification and file writes are outside the timer. Both processes use the same
single CPU, with seeded interleaved C/Rust order. The 17-page run has five pairs
of six timed rounds; the 350-page run has five pairs of two rounds. Each timer
covers the entire corpus. These are generic-kernel results, not whole C/Rust PDF
pipelines, symbol-mode results or reader-device measurements.

The mixed corpus contains ten aged scanned-book pages plus math and
Chinese/Latin supplements. The larger corpus is fixed scanned-book pages 31–380
at 240 DPI and threshold 128. Every PBM identity is recorded in metadata;
the user-provided source and preparation are documented in
[the real-corpus report](benchmarks/2026-11-01-real-corpus).

The reference is Apache-licensed `agl/jbig2enc` arithmetic code pinned at
`8673b046bac6441b0702dec30dfdf0af6023a3aa`. External reference sources and benchmark
rasters are not bundled. The package contains only the original MIT harness.
Rust 1.97.1 uses release optimization, thin LTO, one codegen unit and debug info;
GCC 15.2.0 uses `-O3 -flto -std=c++17`. Both use the default CPU target, on an
Intel Core i7-13700H, Linux x86-64, pinned CPU 0. Frequency is not locked.

The 350-page paired ratios range from 0.945 to 1.070, so the small median lead
should be read as parity with run-to-run variation. The additional pre-speed
350-page C comparison was noisy (C times 2.67–4.79 seconds per round); it is
retained without filtering. The mixed17 pre-speed median ratio was 1.995.
Raw logs, worker/reference/source hashes, all timings, C byte checks and exact
before/after Rust hashes are retained in
[the measurement directory](benchmarks/2026-11-01-generic-speed).

C uses canonical `FF AC` termination; the legacy Rust low-level flush can emit
one extra `FF` before it. C equality checks remove only that known padding byte.
Before/after Rust checks compare the complete unmodified payloads. The optimized
encoder preserves both legacy and opt-in canonical termination behavior.

## Reproduce

Build the Rust worker:

```sh
CARGO_PROFILE_RELEASE_DEBUG=1 cargo test --offline --release --features profiling --test generic_speed --no-run --message-format=json
```

Use the emitted executable path. Fetch `src/jbig2arith.cc` and `src/jbig2arith.h`
at the pinned C revision into an external directory, retaining their notices:

```sh
c++ -O3 -flto -std=c++17 -I/path/to/reference /path/to/reference/jbig2arith.cc tools/generic_reference_runner.cc -o /path/to/reference/runner
python3 tools/compare_generic_kernels.py --corpus /path/to/pbms --worker /path/to/rust-worker --reference /path/to/reference/runner --output /path/to/new-results --rounds 2 --pairs 5
```

`GENERIC_PROFILE=/path/to/profile.svg` enables the worker's optional profiler;
profiling runs are separate from comparison timings. The independent scalar
context oracle covers zero/odd/word-boundary widths, short/tall images, blank,
solid, checkerboard and seeded random pixels, including nonzero unused padding.
Historical full-stream hashes and the broader encoder/decoder suites cover the
shared arithmetic paths.
