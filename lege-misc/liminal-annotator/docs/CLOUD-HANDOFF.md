# Cloud handoff: Liminal Annotator pass 2 (measure, refine, optimize)

Paste the prompt below into a cloud Claude Code session (claude.ai/code) opened on `liminalism/Lege`, branch
`lege-pdf-agent-over-poppler` (commit `fd17bdd9` or later).

---

You are doing the second pass of `lege-misc/liminal-annotator`: measure it, then refine and optimize it. It is
a LiminalKit (CPU software-rendered) rectangle editor for layout-dataset review. The developer's laptop could
not give trustworthy timings because background jobs kept its CPU at 97–100%. Your machine is quiet, and
that is the point of running here.

**Rules**
- Create branch `annotator-pass2` from `lege-pdf-agent-over-poppler`, commit there, and push only that branch.
- Touch only `lege-misc/liminal-annotator/**`. Leave `.akr/` and `docs/generated/` alone.
- The crate is its own Cargo workspace. Run cargo inside that directory. The root `rust-toolchain.toml` pins
  Rust 1.97.1.
- Do not modify LiminalKit, which is a pinned git dependency.
- Do not change user-facing behaviour: keys, mouse semantics, persistence, export formats, and Ctrl+C page
  revert.

**Read first.** The crate's `README.md`. The cached page raster is in `src/canvas/draw.rs` (`render_page`,
`scroll_page`, with threads spawned through `std::thread::scope` on every render). The frame is in
`src/app.rs::render_frame`.

**Targets (from the spec).** Under 16 ms input-to-feedback at 1080p, and under 50 ms to show an
already-prefetched page. Also profile 1440p and 4K.

**Measurements so far.** Laptop, i7-13700H, Windows, quiet at the time, measured before the last
optimizations:

| | 1080p | 1440p | 4K |
|---|---|---|---|
| Drag frame p50 | 0.6 ms | 1.0–1.2 ms | 2.9–4.3 ms |
| Page re-raster, isolated, 8 threads | 1.1 ms | — | 3–4 ms |

- The re-raster time inside `bench` was erratic: 1.2 ms against 8.5 ms p50 at 1080p, with p95 around 10 ms.
  Per-render thread spawning is suspected.
- Not yet timed on a quiet machine: panning by shifting the raster and resampling only the exposed strips,
  skipping frames where nothing visible changed, a branch-free inner loop, and an exact copy path at 100%.

**Tasks**
1. Run `cargo build --release`, `cargo test` and `cargo clippy --all-targets`. Install any Linux system
   packages that winit or softbuffer need, and note which. All tests are headless.
   `real_regression_set_round_trips` skips without the external dataset; that is expected. Fix any failure that
   appears only on Linux, portably, and say what it was.
2. Run `cargo run --release -- bench --frames 240` at least 3 times, and
   `cargo run --release --example raster_profile`. Report the CPU model, core count and run-to-run variance.
3. Optimize only where it is measured to help, and give before/after numbers for each change. Look at p95 and
   variance, not only p50. Candidates:
   - a persistent worker pool, or a thread count chosen from the area being drawn;
   - a dirty-rectangle path for drag frames at 4K;
   - anything else the profile shows.
   Every new raster path needs a bit-exactness test against `render_page`, in the style of
   `scrolling_matches_a_full_render`.
4. Put the measured numbers, the machine description and the optimizations you kept in the README's
   Performance section.
5. Commit with conventional messages (`perf(annotator): …`), each ending with the developer's co-author line.
   Then push `annotator-pass2`.

**Report back:** test and clippy results, bench tables before and after, which changes you kept or rejected
(with numbers), anything that failed, and the commit hashes.
