# PixelKit GUI checkpoint — 2026-10-27

The processor has a runnable PixelKit preview with Preserve mode wired through
the existing subprocess protocol. The migration remains active: this checkpoint
does not claim complete visual or platform parity, and Freya remains the default.

## Implemented

- Four bands, two settings cards, queue/status display, file/folder selection and
  drop handling, page range and resolution, processing choices and their exclusions.
- Preparation and Preserve modes. Preserve removes incompatible preparation flags,
  selects generic ink unless TrueTyping was explicitly chosen, and explains that
  ink is currently rendered black.
- Existing worker supervisor, progress metrics, ETA, sequential queue processing
  and cancellation. Discarding startup events also cancels their worker handles.
- Default and Sanzo themes, live preview, Accept/Cancel, embedded HTML documentation
  serving, queue/log dialogs, docked status notes and up to two timed notices.
- English/German chrome, keyboard traversal, text entry, lazy clipboard ownership
  and IME forwarding. Surface dimensions and font styles each scale once.
- A 620×680 logical minimum, no aspect lock or maximum size, and status space that
  grows with the window. The preview does not replace the shipping desktop launcher.

Build and launch with `cargo gui-pixelkit` followed by `cargo gui-pixelkit-run`.
The package and binary are `lege-gui-pixelkit`. Processor external version remains
1.5.1 pending the planned release.

## Layout revision

The settings band now uses 280 logical pixels instead of 390. Its recovered
110 pixels go to the queue, which can show six rows at the minimum window size.
Page range expands beside a compact 64-pixel resolution field. The checkboxes
retain their reading order across two columns, with the OCR choices above a
bottom tray of equally sized Center margins, Crop margins and Reflow buttons.
Each can be turned off; selecting one clears the other two.

Output settings use a label and control on each row. Every option within a
choice group has the same button dimensions, including image and binarization
modes. Long labels wrap or fit within those dimensions at narrow widths.
Binarization keeps a separate 52-pixel parameter field: K for adaptive, the
threshold value (initially 180) for Threshold, and a dimmed retained value for
Heavy model. Changing modes does not move the field or resize the card.

Process/Cancel is a larger primary action. About, Queue and Log form a separate
group of equally sized smaller buttons. The empty-queue message sits in the
queue area, while the status panel shows Ready.

The layout was reviewed at 620×680 and 940×700, in English and German, plus
scaled and dark/Preserve views. Geometry and input tests cover equal group
sizes, smaller parameter fields, margin exclusivity and the disabled Heavy
parameter. This is a layout checkpoint; the memory figures below were measured
before this revision and have not been remeasured for it.

## Memory comparison

Three interleaved runs per frontend, Linux/X11 on an i7-13700H, both built using
the root `debug-fast` profile. Both were idle with empty queues, fresh isolated XDG
data directories and the same 1980×1452 physical window at the host's 2× scale.
The baseline was the existing Freya **CPU** frontend. Values are process medians
from `/proc/PID/smaps_rollup` after settling and resizing.

| Metric | Freya | PixelKit | Reduction |
|---|---:|---:|---:|
| RSS | 47.57 MiB | 31.90 MiB | 32.94% |
| PSS | 37.35 MiB | 22.35 MiB | 40.16% |
| Idle threads | 5 | 1 | 4 threads |

These are frontend idle measurements, not the peak memory of a conversion, OCR
job or large queue. The processor still runs separately and its raster/model
allocations were excluded. The result supports continuing the port; it does not
establish a memory bound for the whole application.

[Raw measurements](measurements/pixelkit-idle-memory-2026-10-27.json) and the
[measurement helper](../lege-process/GUI/PixelKit/tools/measure_idle_memory.py)
are retained. Reproduce after building both frontends with the same profile:

```sh
python3 lege-process/GUI/PixelKit/tools/measure_idle_memory.py \
  --freya-bin /path/to/freya-baseline \
  --pixelkit-bin target/debug-fast/lege-gui-pixelkit \
  --output /tmp/gui-memory.json --profile debug-fast
```

## Validation

- `cargo gui-pixelkit --offline` built the current processor and preview.
- The all-features GUI suite passes; two environment-dependent worker tests are
  ignored by default. Coverage includes input-to-Preserve argument mapping, legacy
  settings, exclusions, cancellation, startup ownership, layout bounds, locale
  coverage and bounded-channel wakeup.
- Both ignored tests were run against the built worker using an isolated two-page
  colour PDF. Preserve produced two JPX backgrounds and two JBIG2 masks;
  cancellation retained the queued input without a successful GUI completion.
- Native X11 interaction accepted and persisted a Sanzo dark theme; cancelling a
  subsequent preview left the saved theme unchanged. Native resizing accepted
  1240×1360 and 2400×1400 physical windows at 2× scale.
- Reviewed English default, Preserve/dark, narrow, German and 1.5× snapshots.
  The frontend dependency graph has no Freya, Skia engine or processor crate.
  `tiny-skia` remains an indirect CPU window-decoration dependency of winit.
- An independent Terra review identified a bounded-channel wakeup deadlock in
  invalid-input probing; it was fixed and has a regression test.

## Remaining gates

Keep the PixelKit work active until the full control/tooltip/modal matrix and
visual parity have been checked against Freya, including native file dialogs,
multi-file/OCR/DjVu jobs, long-running ETA behavior and Windows/macOS packaging.
Measure queued/processing and resize-heavy memory as follow-up workloads. Before
switching the default, consolidate shared frontend support/assets and update the
shipping aliases/package integration. The sheet-music GUI follows afterward.

Preserve's GUI integration now exists in the preview. Its separate work still
retains DjVu visual decoding, the three-book size/time comparison, the older
grayscale identity gate and deferred fidelity work such as coloured ink and
colour source-MRC passthrough.

For other current work use the generated [outstanding-work index](generated/OUTSTANDING-WORK.md).
The [dated ledger review](OUTSTANDING-WORK-REVIEW-2026-10-27.md) records why old entries
were completed, narrowed or dispositioned; it is a historical snapshot.
