# jpxbench — paired jp2lam vs hayro-jpeg2000 decode benchmark

Decodes the same raw `/JPXDecode` streams through `jp2lam` (this tree) and
`hayro-jpeg2000 0.5.0` (crates.io), serially, at full resolution and at one
reduction level, and prints per-layer milliseconds (min and median over
`--iters`), MB/s of packed RGB8 output, and the jp2lam/hayro ratio. With
`--stats` it adds jp2lam's stage breakdown (`decode_with_stats`); with the
`hayro-trace` feature it adds hayro's.

The reference numbers and the optimisation history are in
`dev-notes/lege-pdf/render-refinement/performance-history/optimization-jp2lam-serial-20261104.md`.

## Streams

The benchmark corpus is not checked in. Extract it from the test books
(`D:/pdf-test/Structures.pdf`, `D:/pdf-test/chinese-medicine.pdf`; two
full-page JPX layers per page):

```bash
python extract.py streams structures=D:/pdf-test/Structures.pdf:0,1,2,100,300 cm=D:/pdf-test/chinese-medicine.pdf:0,1,2,100,300
```

## Running

Whole-document renders decode every image serially (`Budgeted(1)` under
load), so serial throughput is what matters. Pin to P-cores and keep Rayon to
one thread, or Windows parks the process on an E-core and the numbers are a
coin toss:

```bash
RAYON_NUM_THREADS=1 cmd //c "start /affinity FFF /b /wait target/release/jpxbench.exe --iters 7 --levels 0,1 streams/*.jpx"
```

Options: `--iters N`, `--levels 0,1`, `--stats`, `--check` (compare the two
decoders' output bytes), `--only jp2|hay`.

`check.sh` wraps the usual loop: rebuild, decode every stream once through
jp2lam and compare the raster hashes with `streams/baseline-hashes.txt`
(`check.sh --baseline` records it), then print the timings. Decoded output
must stay bit-identical across jp2lam changes: every kernel flavour (scalar,
`wide`, AVX2) and the streaming DWT are checked against the reference
implementations in jp2lam's own tests, and this hash check covers the whole
decode on real streams.

`JP2LAM_PRIMITIVES=scalar|wide|avx2|auto` selects jp2lam's kernel flavour for
A/B runs.

## hayro stage timings

`hayro-trace.patch` adds a thread-local `TRACE` with per-stage timers to
hayro-jpeg2000 0.5.0. To use it, copy the crate out of
`~/.cargo/registry/src/*/hayro-jpeg2000-0.5.0`, apply the patch there, add

```toml
[patch.crates-io]
hayro-jpeg2000 = { path = "../hayro-patched" }
```

to this `Cargo.toml` (do not commit that), and build with
`--features hayro-trace`.
