# lege-process 1.5.2 — PixelKit GUI scope

Status: accepted and in progress (2026-10-27). A runnable preview lives in `lege-process/GUI/PixelKit` (`lege-gui-pixelkit`), with Preserve included from the start. Processor 1.5.2 is the release target. The shipping GUI stays `lege-process/GUI/Freya` (`lege-gui`, package `lege-gui-freya`) until the migration acceptance checks pass.

Current processor version is 1.5.1 (`lege-process/external_version.txt`); keep it until the release change.

The goal is visual parity with Freya and more flexible resizing. Measure memory on comparable workloads: a reduction is preferred, while comparable memory may still be justified by a more flexible design. The first empty-queue comparison measured about 33% lower RSS; this is not a processing-peak claim. See [the checkpoint report](../docs/PIXELKIT-GUI-PREVIEW.md) for commands, evidence and remaining gates.

PixelKit is the sibling checkout at `../pixelkit` (crates `pixelkit-ui`, `pixelkit-raster`, `pixelkit-text`, `pixelkit-windowing`, `pixelkit-shell`). PagePress already depends on those crates by path. That is the dependency shape to copy. PixelKit is immediate-mode: the app owns state, places rects, and draws every frame into a CPU `WindowBuffer` presented with winit and softbuffer.

The port target is the main processor GUI. `lege-process/GUI/musicsheet` (`lege-music-gui`) is the same shell with a smaller settings card and a different About page. Port it after `lege-gui` matches this scope, reusing the same toolkit wiring. Do not let the two copies drift further during the first port.

## What the new UI has to keep

### 1. Tooltips, and how they appear

Hovering a control puts a note in the status panel. It does not float next to the control. The card is 250px wide, at most 6 lines, 13px type, line-height 1.3, padding 6×8, 4px corners, panel fill, 1px border. There is no delay and no animation. Pointer-over sets the note immediately. Pointer-out clears it only when the stored string is still that control's text. Notes are hidden while a job is running.

Left-side notes (status top-left) cover Add File, Add Folder, resolution, and the left-card checkboxes, OCR choices, and margin tiles. Right-side notes (status top-right) cover Output Directory, the image-type buttons, Layout, Binarized, Grayscale, and the binarization choices. Copy lives in `lege-process/language_service/{en,de}/gui_text.json` under `interactive.tooltips`. Two grayscale MRC strings are still hardcoded in `app.rs` and should move into that JSON during the port so both languages have them.

PixelKit's `Tooltip` is a different widget: one cursor-anchored square box per frame, drawn last, with a hardcoded shadow and no theme lookup, no delay, and no dock to a control. Do not use it for these notes. Draw the existing card in the status band with the app's theme colors. PagePress's truncation tooltip can stay as the pattern for a one-line overflow label if a path is clipped. It is not a substitute for the status note.

### 2. Theming

About → Theme opens a chooser on the About card. The user sees one name (`Default`, `Sanzo Light 3`, `Sanzo Dark 3`, and the rest) plus the source color names. Controls are previous, next, Light, Dark, Accept, and Cancel. Left and right walk the ring. Up and down flip the light and dark variant of the same Sanzo combo. Enter accepts. Escape cancels. The preview is live and is stored only on Accept. Closing About without Accept restores the previous theme.

The set is Default (day only; on Linux the panel, card, and selected colors are a slightly warmer tint) plus 28 Sanzo Wada combos, each with a light and a dark variant (`appearance.rs`, `sanzowada.rs`). Light surfaces near white are pulled toward the accent. Dark text and muted text are forced to white and `(220,220,220)`. The app does not follow the operating-system theme.

Persistence stays `{data_dir}/Lege/appearance.json`: `{"theme":{"kind":"default"}}` or `{"theme":{"kind":"sanzo","colors":[...],"index":N,"dark":bool}}`. Identity is the color-name list. An unknown file falls back to Default.

PixelKit's `Theme` is a `Copy` bag of colors plus `row_height`, `padding`, `corner_radius`, `border_width`, `focus_width`, and two `TextStyle`s. `Appearance::IndustrialLight` / `IndustrialDark` are PixelKit's own palettes, not Lege's. The app keeps the logical Lege palette and copies it into `Theme` at the start of each frame. Store the unscaled theme. PixelKit scales body and heading sizes from `Ui::new`; PagePress also pre-scales `row_height`, padding, radius, and border because some widgets consume those numbers raw and others pass them through `Ui::px`. The port has to pick one convention and use it for every control, or the compact layout will double-scale on HiDPI.

### 3. Layout and every control, in a tighter window

Today the window is titled "Lege", 990×726, minimum the same, maximum 1980×1452, aspect about 1.30–1.45, resizable, with minimize and close and no maximize. The shell is a 6px outer pad, an 8px-radius panel, and four bands: file actions (56px), settings (300px, two equal columns), process (102px), and a status panel that fills the rest.

1.5.2 should use a more compact window. Shrink padding, type size, and row height through the theme and the rects. Keep the same four bands, the same two settings columns, and every control below. Do not drop a control to make the window smaller. Mutual exclusion rules are part of the control, not decoration.

The current PixelKit revision uses a 280px settings band and assigns the recovered
110px to the queue. Page range and a compact resolution field share a row;
checkboxes read across two columns. Center margins, Crop margins and Reflow share
one bottom tray with equal button sizes and allow all three to be off. Right-card
settings are label/control rows, with equally sized buttons within each group.
Binarization has three equal buttons and a separate smaller parameter field,
dimmed in place under Heavy model. Process/Cancel is the primary action; About,
Queue and Log are a separate group of equal smaller utility buttons. These
requirements supersede any suggestion to size sibling buttons by label length.

File row:

- Add File. File dialog, extensions `pdf` and `zip`.
- Add Folder. Accepted only when the folder contains page images (`png jpg jpeg ppm pbm pgm pnm tiff tif bmp jp2`).
- Output Directory. Shows a truncated path (tail, 34 characters). The first add with no output directory defaults the output to the input's folder and says so.
- File drop of one path: PDF, ZIP, or a non-empty image folder.

Left card:

- Page range, placeholder `e.g., 1-10`.
- Target resolution, default display `1200`. Accepts `1200`, `1200x800`, `1200 800`, and a times sign. A valid edit is written immediately to `resolution_preset.json`. Free-aspect crop ignores a typed width and warns.
- Inverted colors. Disables layout and reflow.
- OCR text layer. Turning it off clears the EPUB sidecar.
- Make an EPUB also. Forces OCR on and Thorough.
- Symbol mode. Shown for PDF when the text encoder is JBIG2, compatibility mode is off, and grayscale is off.
- High quality.
- TrueTyping. PDF only. Turns grayscale on and clears symbol mode. Turning it off returns to JBIG2. Exclusive with compatibility mode.
- Compatibility mode. Hidden in grayscale. Selects CCITT Group 4 and clears halftone and symbol mode. Turning it off returns to JBIG2.
- Fast / Thorough. Shown while OCR is on. Fast clears the EPUB sidecar.
- Center margins. Clears crop, footnotes, free-aspect, and reflow.
- Crop margins. Clears centering and reflow, sets free-aspect.
- Reflow. Enables layout and clears incompatible geometry.

Right card:

- Page mode: Preparation or Preserve. Preserve selects colour MRC, clears incompatible dithering, grayscale, compatibility, symbol and reflow state, and keeps TrueTyping only when explicitly selected. Bilevel controls are hidden in Preserve; its summary and tooltip explain that ink is currently black.
- Output format: PDF or DjVu. There is no EPUB format button. A loaded EPUB format value is rewritten to PDF.
- Text rendering: Binarized or Grayscale. Grayscale hides binarization and compatibility mode.
- Layout detection on/off. Off disables reflow and posts a short notice. The image-type row hides while layout is off.
- Image output type: Original, Dithered, and JBIG2 halftone. The third button shows when the effective text format is JBIG2 or TrueTyping.
- Binarization, hidden in grayscale. One of custom adaptive (K, default 0.05, range 0–1), threshold (initial value 180, range 0–255), or heavy model. The smaller parameter field remains in place and is dimmed under heavy model. A new session starts on custom adaptive. The card must not change height when switching modes.

Process band:

- Process / Cancel.
- About, Queue count, Log.
- With `debug-logging`, a Debug button in the status panel.
- Queue lines fill the available queue area (six at the minimum size): Queued, Processing, Completed, Failed, plus a shortened path. Overflow is `+N more`.

Status band:

- Empty: Ready; "No files queued - Add a file or folder to begin." appears in the queue area.
- Idle with work: item count, and a page total when every item has been probed.
- Running: stage title, detail line, `ETA`, and one progress bar titled Progress.

Other surfaces that stay:

- Popup rail, bottom-right, at most two cards, click to dismiss. Durations stay as they are (about 3s, 5s, 6s, and 8s depending on the notice).
- Queue dialog with Clear Queue, disabled while a job runs.
- Log dialog, newest first, at most 30 entries, Clear Log, and opening a run log in the system handler.
- About: title, `v` plus the external version, Theme, Readme, Licenses, Close.

What persists today is `appearance.json`, `resolution_preset.json`, and the processing log. Checkbox state resets unless an old `settings.json` is present. Keep that. Data directory stays `lege_ipc::data_dir()` (`%LOCALAPPDATA%\Lege` or `~/.local/share/Lege`).

PixelKit can build this from `Rect` splits, `Ui::panel`, `Ui::button`, `Ui::checkbox`, `Ui::toggle`, `Ui::text_field`, and `Ui::scroll_list`. There is no flexbox. File dialogs are not in PixelKit. Keep the vendored `rfd` crate, called off the UI thread, which is what PagePress does. One `TextFieldState` per field and the checkbox booleans stay on the app struct, which they already do in `AppState`.

### 4. Documentation

About → Readme and About → Licenses open the copies of `lege-misc/docs/documentation.html` and `lege-misc/docs/licenses.html` that were compiled into the binary. The GUI serves them on a loopback HTTP port (only those two names plus `LICENSE.txt`) and asks the system to open the URL. Failures stay on the About card and report which page failed. The sheet-music edition opens `documentation-sheetmusic.html` from disk. Keep both behaviors.

Chrome strings stay in `language_service/en/gui_text.json`, with `language_service/de/gui_text.json` behind the `german` feature. PixelKit has no HTML or markdown viewer, and its text API is one face, one size, one color per draw. Do not reimplement the manuals inside the window.

### 5. Progress reporting

The GUI does not link the processor. It runs `lege --gui-worker` one file at a time. The binary is `lege` beside the GUI executable, otherwise on `PATH`. Stdout is newline-delimited JSON. Logs go to stderr and to the run log from `lege-ipc`.

The wire types stay `ProgressUpdate` and `ProcessingStatus` in `lege-process/core/progress.rs`, mirrored by the GUI's `Worker*` serde types. Tags stay `type` = `status` | `completed` | `error`, status `kind` in snake_case, reflow stages `source_analysis` | `compose` | `output_pages`, and `ProgressMode` in PascalCase (`Unknown`, `NoLayout`, `Layout`, `Margin`, `HeavySequential`, `Reflow`, `Epub`). The bar fills from `rendered` / `detected` / `encoded` against `pages_total`. EPUB uses `pages_total * 3`.

Cancel is not a stdin message. The button sets a flag, the status line says "Cancelling...", and the supervisor signals the child: `SIGTERM` on Unix, `taskkill /F /T` on Windows. The CLI's existing handler turns that into shutdown. A worker silent for 15 seconds is killed. A clean exit with no terminal JSON event is an error string, not success.

`lege-ipc` stays the shared log and data-directory crate. It is not the progress socket. Do not replace this protocol with an in-process worker as part of the UI port.

PixelKit's `Ui::bar` and `Ui::meter` are static fractions. Use one of them for the progress fill. Stage titles, the ETA line, the queue rows, and the completion popups are labels the app draws. There is no spinner widget. Add one only if a stage has no fraction yet. The current UI uses text for that case.

### 6. Software renderer

`lege-gui` already refuses Skia. `lege-process/GUI/Freya/Cargo.toml` sets `freya` to `default-features = false` and `features = ["cpu-renderer"]`. That selects `freya-render-tiny-skia` and presents through softbuffer. `skia-engine` and `cpu-renderer` together do not compile. There is no runtime switch.

PixelKit's `pixelkit-raster` is the CPU rasterizer to link instead: `WindowBuffer` of `0x00RRGGBB`, `Painter` for clipped fills, strokes, rounded rects, and blits, presented by `SoftbufferPresenter` on winit. That is the desktop path. `run_kms` is a Linux console presenter and drops keyboard, IME, clipboard, and extra windows. Do not use it for `lege-gui`.

The Freya CPU renderer leaves `draw_image` and `draw_svg` empty. The processor GUI does not depend on those calls. PixelKit can draw bitmaps, which is enough for an icon if the compact layout wants one. Do not bring Skia back to get images.

After `lege-gui` builds and the checks below pass, drop the `freya` dependency from that package. Leave `freya-main` in the tree until both GUIs have moved. The fork's plans `freya-cpu-renderer-implementation-plan.md` and `freya-gui-renderer.md` describe the renderer being replaced. They are not a second implementation track.

## Explicitly unchanged

- Processing behavior, encoders, OCR, and the `--gui-worker` JSON.
- The external version file, until the 1.5.2 release commit.
- In-app HTML manuals, aside from whatever the release notes of 1.5.2 need.
- `rfd` as the file-dialog implementation.
- The sheet-music feature set (`music-sheet` still builds a layout-free worker into `target/music-sheet/`).

## Gaps the app has to cover

PixelKit provides immediate-mode buttons, toggles, checkboxes, radios, tabs, dropdowns, single-line fields, tables, scroll lists, a static meter, one toast card, and a cursor tooltip. The Lege GUI has to supply the rects, the Lege theme, the font, widget state, the docked status notes, file dialogs, the progress line, queue and log windows, popups, and the loopback document server.

Clipboard on Windows is unavailable in PixelKit today. This GUI does not depend on it. AccessKit is optional in PixelKit and off unless both `pixelkit-ui` and `pixelkit-windowing` enable `accessibility`. The current Freya shell is not a screen-reader UI. Turning AccessKit on is a follow-up, not a 1.5.2 requirement.

## Suggested order, once accepted

1. A PixelKit window with the Lege font and a `Theme` filled from `active_palette()`.
2. The four bands and every control, compact, with no processing behind them.
3. Settings state, mutual exclusions, and the two JSON files that already persist.
4. Status-panel tooltips from `gui_text.json`.
5. File dialogs, drop, and the queue.
6. The existing `--gui-worker` supervisor and progress bar.
7. About, theme chooser, Readme, and Licenses.
8. Remove `freya` from `lege-gui-freya`. Then port `lege-music-gui` the same way.

## Checks for the implementation, not for this document

- `cargo gui` produces `lege-gui` with no `freya` or Skia crate in that package's resolved dependency graph, and the window opens on the CPU presenter.
- Every control in section 3 is reachable, and the mutual exclusions match the current GUI.
- Hovering a control shows the same note, in the status panel, in the active theme's colors, and the note hides while a job runs.
- Accepting a Sanzo dark theme writes `appearance.json` and reloads it. Cancel restores the previous theme.
- Readme and Licenses open the embedded HTML in the system browser.
- A multi-page job shows the stage title, the bar, and an ETA, and Cancel stops the child without a successful completion record.
- English and `german` builds still take their chrome strings from `language_service`.
