# Runtime lifecycle implementation audit — 2026-11-01

Implemented the worthwhile changes from the implicit-state-machine review in
`lege-pdf/render` and `lege-process`. This audit covers the working tree based on
`5ca4565f60b48a157bcbd9797c88aab7f805bacb`; it does not imply that the changes
have been committed. Existing GUI migration and concurrent codec work were
preserved. AKR work: `lege-ecosystem.work.explicit-runtime-lifecycles`.

The changes use Rust enums, ownership and scoped frames. No statechart runtime
was added. The scheduler keeps its bounded channel and RAII permit design.

## Disposition of all reviewed candidates

| # | Candidate | Final implementation and coverage |
|---|---|---|
| 1 | Nested PDF execution scopes | [`NestedStreamFrame`](../lege-pdf/render/crates/pdf-content/src/interpret.rs) centralizes entry/restoration for forms, soft masks and Type 3 glyphs. Graphics and marked-content floors prevent nested closers from consuming caller scopes; unmatched saves unwind before a group closes. Paths, pending clips, text objects, operands, resources and caches are scoped together. Six new [stream-scope regressions](../lege-pdf/render/crates/pdf-content/tests/stream_scope_tests.rs) cover malformed nesting, transparency-group wrappers, hidden spans, paths and text. |
| 2 | Worker subprocess completion | [`WorkerHandle`, `WorkerProtocolState` and the child owner](../lege-process/GUI/liminalUI/src/worker_process.rs) separate protocol completion from process exit. One terminal outcome is published after stdout/stderr drainage and exit validation; unsuccessful exit overrides protocol success. Stop requests go through the owner rather than a cached PID. Dropping ownership stops a worker; termination escalates after a grace period. Shell-fixture regressions cover completion before exit, final stderr, unsuccessful/missing completion and cancellation. |
| 3 | Pipeline failure/shutdown | [`supervise_pipeline_stages`](../lege-process/pipeline/helper_functions.rs) owns DjVu stages, observes failures in completion order, cancels peers and drains outstanding blocking work. Ownership is established before the returned future's first poll. Shared stage helpers clean up errors as well as cancellation. `PipelineLifetime` covers dropped PDF, DjVu, EPUB and reflow futures. Blocking-work accounting begins before submission, including queued closures. `shutdown_signal` distinguishes an actual request from a closed or lagged broadcast source. Regressions cover sibling failure, downstream failure, unpolled supervisor drop, closed shutdown sources and queued blocking work. |
| 4 | PDF writer publication | [`PdfWriterState` and `PdfWriterFinalization`](../lege-process/pipeline/helper_functions.rs) enforce collecting → finalizing → published/aborted/failed. Finalization consumes a producer and returns an owned completion; all producer clones reject late messages. Closing without finalization is an error. Cancellation and atomic publication share a commit gate; the completion waits for the blocking writer's actual outcome. Both PDF and PDF reflow callers use it. Tests pause immediately before publication, preserve a previous destination on cancellation, reject late producers and reconcile a shutdown arriving after publication. |
| 5 | GPU initialization/recovery | [`GpuLifecycle`](../lege-pdf/render/crates/pdf-render-wgpu/src/lib.rs) replaces the optional backend/recovery flag/permanent-reason combination with `CpuOnly`, `Cold`, `Ready`, `NeedsRecovery` and `Disabled`, under one mutex. Initial automatic unavailability remains permanent; recovery failures remain retryable. Pointer identity still protects a recovered device from an older generation's failure. Transactional CPU fallback remains. The existing rendering/recovery suite and a new pre-initialization quarantine regression pass. |
| 6 | GUI batch/document lifecycle | [`BatchState` and `DocumentRunState`](../lege-process/GUI/liminalUI/src/app.rs) replace running/cancelling flags, translated row statuses and the separate handled-ID set. Events carry batch generations; stale events cannot mutate another run. Cancellation settles unstarted rows and has its own final outcome. Progress aggregates every document and its stages, reserving completion for a terminal event. Obsolete lifecycle fields were removed from `DocumentItem`. Unit tests and real-worker preservation/cancel/two-file smoke tests pass. |
| 7 | Inference checkout | [`InferenceAdmission`](../lege-process/pipeline/inference.rs) moves session and VRAM permits into the blocking closure. Aborting the async waiter cannot admit another caller while inference still owns those resources. A gated-worker regression checks both permits before and after the real worker exits. No statechart is needed for this ownership problem. |
| 8 | Prepared page/encoding handoff | [`PreparedBase`](../lege-process/pipeline/pdf_tokio_pipeline.rs) carries one of cover, bilevel, JPEG, grayscale MRC, preserved RGB MRC or deferred work. The encoder matches the variant instead of reinterpreting optional buffers using configuration flags. MRC background/ink stay together; deferred work carries binarization options. A CPU preparation regression exercises all six variants, alongside the existing OCR, cover and planned-product tests. |
| 9 | GUI processing choices | [`WorkflowMode`, `PageTreatment`, `BinarizationKind`, `GeometryMode` and `ImageTreatment`](../lege-process/GUI/liminalUI/src/models.rs) centralize effective choices and selection changes. Historical saved fields remain a compatibility DTO; normalization repairs conflicting values before worker argument generation. Geometry and binarization are exclusive, and encoder/compatibility/halftone dependencies are canonicalized. The normalization precedence is documented. All 512 tested legacy flag combinations normalize idempotently and round-trip through the existing JSON shape; argument-generation and interaction tests pass. |
| 10 | PDF text objects | [`TextObjectState`](../lege-pdf/render/crates/pdf-content/src/interpret.rs) owns text matrices and accumulated clipping runs for an active text object. Nested streams park the whole object. Repeated `BT` commits the previous clip; unmatched `ET` is tolerated; text outside `BT..ET` explicitly starts a fresh recovery object. New nested/repeated-object tests and the existing text/clip renderer tests pass. |
| 11 | Scheduler admission/order | Kept [`RenderScheduler`](../lege-pdf/render/crates/pdf-render-scheduler/src/lib.rs), its sliding admission window, reorder buffer and memory permits. Those already express ownership and backpressure well. Request and pipeline cancellation are now linked; either source reaches budget waits and active rendering without cancelling the other input token. An active-backend regression covers a request with its own token. Cancellation's in-order, one-result-per-requested-page drain contract is documented. |
| 12 | Progress lifecycle/phases | [`TrackerLifecycle`](../lege-process/core/progress.rs), metrics mutation and event publication share one lock. There is exactly one terminal event, no later status, and no post-terminal metric mutation. Reflow stores its current phase, resets timing at phase boundaries and publishes phase changes even when counters are equal or throttling would suppress them. Concurrent terminal-publication and equal-counter phase regressions pass. |
| 13 | Input selection/probing | [`InputFlow`](../lege-process/GUI/liminalUI/src/app.rs) distinguishes idle, native selection and probing, with operation IDs and cancellation ownership. Old results/close events cannot finish a newer operation. Starting processing while input selection/probing is active is blocked. [`probe_output`](../lege-process/GUI/liminalUI/src/worker_process.rs) has a 30-second processing deadline and stops/reaps its child when its async owner is dropped. Tests cover stale events, owner drop, timeout and cancellation. Native dialog selection itself remains the existing `rfd` operation. |
| 14 | Resolution editing | [`ResolutionEdit`](../lege-process/GUI/liminalUI/src/app.rs) retains the committed value while the text field holds a draft. Valid edits commit on blur; invalid/empty drafts restore the previous value. Intermediate valid prefixes are no longer persisted while typing. Draft/commit/restore and existing focus interaction tests pass. |
| 15 | Theme preview/commit | Kept the existing modal/preview flow. [`persist_theme_choice`](../lege-process/GUI/liminalUI/src/appearance.rs) changes committed memory only after persistence succeeds; saving writes/syncs a temporary file and atomically replaces the settings file. Failed acceptance leaves the preview cancellable and the committed choice unchanged. The failure regression uses local settings, without changing the user's theme. Further modal-machine extraction was not warranted. |
| 16 | Render ticket polling | [`TicketState`](../lege-pdf/render/crates/pdf-render-api/src/lib.rs) makes pending/ready explicit and memoizes the terminal result. Repeated `try_wait` calls and a later consuming `wait` observe the same result rather than turning success into disconnection. Host pixels remain shared by `Arc`. Polling and disconnection tests pass. |
| 17 | Path construction | [`PathBuilder`](../lege-pdf/render/crates/pdf-content/src/state.rs) uses an optional current subpath instead of a validity flag plus inactive coordinates. Closing without a current subpath is ignored; the legacy tolerant curve fallback is retained. Deferred clipping stays in the interpreter and is isolated by the common stream frame. A larger path state machine was not warranted; content, adversarial and rendering suites pass. |

## Verification

Final Linux results:

| Check | Result |
|---|---|
| Renderer/content/API/scheduler/CPU/WGPU/chaos suites | 451 passed, 0 failed |
| Processor library, default features | 385 passed, 0 failed; 1 existing corpus-dependent test ignored |
| English GUI with debug logging | 68 passed, 0 failed |
| German GUI with debug logging | 68 passed, 0 failed |
| Explicit real-worker smoke tests | Preservation, cancellation and two-file batch: 3 passed |
| All-target checking | `lege`, `lege-gui-liminalui`, `lege-render-ffi`: passed |
| Formatting/whitespace | Changed Rust files and scoped `git diff --check`: passed |

The three GUI worker smoke tests are ignored in ordinary GUI runs because they
require an explicitly built CLI and input/output paths; all three were executed
separately against the rebuilt CLI and a generated four-page PDF. Their final
run used temporary application data and output directories under `/tmp`.

The processor's remaining ignored test requires `LEGE_BINARIZATION_PARITY_CORPUS`.
An exploratory no-OCR-feature test run failed three existing OCR expectations;
the supported default-feature suite passes those expectations. Existing GPU
tests retain adapter-availability guards. No Windows/macOS run or manual native
window session was performed.

Commands used for final verification (offline dependency resolution):

```bash
export CARGO_TARGET_DIR=/tmp/lege-lifecycle-target
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
cargo test -p pdf-content -p pdf-render-api -p pdf-render-scheduler -p pdf-render-cpu -p pdf-render-wgpu -p pdf-chaos-tests --offline
cargo test -p lege --lib --offline
cargo test -p lege-gui-liminalui --lib --features debug-logging --offline
cargo test -p lege-gui-liminalui --lib --features german,debug-logging --offline
cargo build -p lege --bin lege --offline
XDG_DATA_HOME=/tmp/lege-lifecycle-smoke/appdata LEGE_GUI_SMOKE_INPUT=/tmp/lege-lifecycle-smoke/input.pdf LEGE_GUI_SMOKE_OUTPUT=/tmp/lege-lifecycle-smoke/output PATH=/tmp/lege-lifecycle-target/debug:$PATH cargo test -p lege-gui-liminalui --lib real_worker_smoke --offline -- --ignored --test-threads=1
cargo check -p lege -p lege-gui-liminalui -p lege-render-ffi --all-targets --offline
```

## Compatibility and review boundaries

Saved GUI JSON keeps its historical field names. Invalid combinations are
normalized; the runtime uses typed effective choices. The renderer remains
document-parallel, with no additional hidden worker pool.

Two intentional Rust API changes deserve caller attention:

- `PdfWriterHandle::finalize` consumes the handle and returns
  `PdfWriterFinalization`; callers await its `wait` method. Repository callers
  were migrated. The original writer-spawn entry point remains available.
- `ExperimentalImageRenderer::gpu_unavailable_reason` returns an owned
  `Option<Arc<str>>`, so the reason can safely outlive the lifecycle mutex guard.

Cancellation before the PDF publication gate preserves the previous output.
Publication that already won remains a successful writer outcome; a late
shutdown request still cancels subsequent sidecar work. This is a per-artifact
commit boundary, not a transaction spanning a PDF and its optional EPUB.

Tests, source inspection and this candidate-by-candidate audit cover the five
acceptance groups in the AKR work record: renderer, runtime, representations,
GUI and integration. Unrelated planning records retain their own acceptance
requirements.
