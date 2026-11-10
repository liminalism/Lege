# Liminal Annotator

A native Windows rectangle editor for reviewing book-page layout boxes, written in Rust on
[LiminalKit](https://github.com/liminalism/liminalkit). It is the review tool for the layout-model dataset
that trains Lege's layout detector (`google-colab-work/layout-detection`; the spec is `annotator.md` there).
It is its own Cargo workspace, outside the Lege workspace, so builds at the repository root are unaffected.

You open a page, inspect the model's existing boxes, correct or add regions with the mouse, and press
**Right Arrow** to continue. Every edit is committed to SQLite when you release the mouse, so there
is no Save button. The original page pixels are authoritative. Boxes live in original image coordinates,
and downsampling to 640×640 (or any size) happens only at export.

## Quick start

```powershell
cd lege-misc\liminal-annotator
cargo build --release
$A = ".\target\release\liminal-annotator.exe"

# Open a dataset folder. The first time, the project is created inside it as
# annotator.db; later, only new detections are imported. Opens on the book manager.
& $A C:\layout-dataset-v2

# Or start the window with no arguments: it reopens the last project, or shows a start
# screen with "Open a dataset folder…" (Ctrl+O) and the recent projects. Folders and
# .db files can also be dropped onto the window at any time.
& $A

# The v1 review sets still work as before
& $A import-dataset work\review.db D:\projects\google-colab-work\layout-detection\data
& $A work\review.db --set regression

# Export a versioned dataset: original-resolution COCO + 640×640 COCO and JPEGs
& $A export work\review.db --set regression --images --reviewed-only
```

`target\release\liminal-annotator-gui.exe` is the same window without a console, for
Explorer shortcuts and "Open with"; it takes an optional folder or project. The console
binary keeps the command-line tools. `liminal-annotator --help` lists every command.

## Dataset folders and the book manager

A folder is opened as a project in one of three layouts, checked in this order:

| Layout | Recognised by | Imported as |
|---|---|---|
| Teacher pool (`layout-detection` dataset v2, `label-books.cmd`) | `teachers_raw.jsonl` beside `images/<book>/pNNNN.jpg` | One book per image folder. DocLayout-YOLO pre-fills the editable layer (`--prefill paddle` or `none` to change); the other teacher is the prediction layer shown by `C`. `books.jsonl` gives each book's split, `image_pages.jsonl` its review tier (queue reason and priority) |
| COCO data folder (dataset v1) | `annotations/*.json` | One set per file, as `import` does |
| Plain image folders | `.jpg`/`.png` pages anywhere below | One book per folder, no boxes |

Boxes are pre-filled from a teacher at score 0.30 or more (`--min-score`); near-duplicates
(IoU 0.8, any class) keep the highest score, since PP-DocLayout-L scores one region under
several classes. The prediction layer keeps 0.30 or more (`--layer-min-score`). Image files
are not opened during import, so a pool of 230,000 pages imports without touching 230,000
files: on the development laptop, `C:\layout-dataset-v2` (231,822 pages, 622 books,
1.79 M boxes) took 32 s and made a 1.7 GB project. A stopped import keeps what it
committed (every 2,000 pages) and the next open carries on. Opening a folder whose
`teachers_raw.jsonl` changed (size or time) imports only the pages not yet in the project,
so books labelled later appear on the next open, or at once with **Rescan folder** (F5).
`liminal-annotator create <folder>` does the same without the window.

**B** (or the **Books** button) shows the book manager, a table with one row per book:
import order, name, split, pages, % reviewed, pages still to do, picture pages (a figure
box, or a figure from the comparison model) and those of them still to do, pages needing
attention, paper tone, and a column for every label (figure, caption, table, ornament first, then text, heading, the
page furniture, the rare classes, and `abandon`), plus large `abandon` boxes.

- **Sorting.** Click a header to sort by it (numbers largest first, names A to Z), click it
  again to reverse; a small triangle marks the sorted column. Tab / Shift+Tab move the sort
  to the next / previous column and F4 reverses it.
- **What the label columns count** (F2 or the buttons): boxes, boxes per page, or the
  percentage of pages that have at least one. "Figure, per page" ranks books by how
  illustrated they are whatever their length.
- **Moving.** Up/Down, PgUp/PgDn, Home/End (also while filtering), the mouse wheel, or the
  scroll bar at the right: a slim grey thumb at rest that widens, with a track, when the
  pointer comes near; drag the thumb, or click the track to jump there. The
  columns that do not fit scroll sideways with Left/Right, Shift+wheel, a sideways
  touchpad swipe, or the « » marks; `#` and the name stay put.
- **Choosing.** Click a row to select it; a second click, a double click or Enter (or
  **Review selected book**) makes the book the queue and opens its first page to do. Type
  to filter by name; Escape clears the filter, then goes back to the page. The line under
  the table describes the selected book (figures, captions, tables, ornaments, abandon,
  paper colour).
- **Paper tone.** Yellowed and aged paper can be found without knowing where a book came
  from. When the book manager opens, books that have not been measured are measured in the
  background (five pages from the middle 70% of each book; the paper of a page is the
  median colour of its brightest fifth, the book's the median over its pages) and kept in
  the project's `book_tone` table. **Paper b\*** is CIE b\*, blue to yellow: about 0 for
  white or grey paper, 10 to 20 for cream, 25 to 45 for browned or yellowed scans, negative
  for blue-tinted pages; a swatch shows the colour. All 647 books of the pool took 2.6 s.
  Sort by it to start with the yellowed books.

- **Book flags.** Untick **Train** (click the box, or F6) to leave a book out of training:
  every export skips its pages and lists it in the manifest (`excluded_books`,
  `excluded_book_pages`); the book stays reviewable and its name is dimmed. **Needs work**
  (click, or F7) marks a book to come back to without excluding it. Both columns sort
  like any other, and **Show** (F8) lists all books, those for training, the excluded, those
  that need work, or those with many abandon boxes. In the editor the top bar says "not for
  training" or "needs work" for a flagged book. `liminal-annotator flag <folder> --book ID
  --exclude|--include --needs-work|--done` does the same from the command line.

- **Splits.** F9 moves the selected book to the next split (train, dev, final_test,
  regression, new). The 544 books imported after v1 are all in split `new`, which an export
  cannot keep apart from training: Shift+F9, pressed twice, gives every `new` book a split by
  a stable hash of its name (about 10% dev, 10% final_test, the rest train; whole books, so
  no book straddles two splits). On the pool that is 445 train, 45 dev, 54 final_test.
  `liminal-annotator split <folder> --assign-new [--dev 0.1] [--test 0.1] [--dry-run]` and
  `split <folder> --book ID --to SPLIT` do the same from the command line.

The project remembers the last book. **Review all books** clears the book filter.
`liminal-annotator books <folder|project> [--sort figure|paper|abandon|…] [--per-page]
[--paper] [--flagged] [--limit N]` prints the table; `--paper` measures missing tones first.

## YOLO `abandon` boxes

DocLayout-YOLO's `abandon` class is everything it chose not to call content. Where
PP-DocLayout-L boxes the same region it calls it a page number (39%), running head (35%),
running foot (9%), footnote (8%) or text (4%); almost never a picture.

**So an `abandon` box takes PP-DocLayout-L's label.** At import, each YOLO `abandon` box is
given the label of the PP box that overlaps it best (PP score 0.15 or more, IoU 0.3 or
more; the 0.15 is the v1 synthesis's PP threshold). The box keeps YOLO's geometry, and its
provenance records both (`teacher_class: abandon`, `label_from`, `label_class`,
`label_score`; the status bar shows them for the selected box). On the pool this labels
97.6% of them: 441,661 of 452,569, leaving 10,908 boxes on 9,581 of 236,801 pages.
Projects imported before this get the same pass once, the next time the folder is opened
(17 s for the pool), as one logged bulk change: Shift+A then U undoes it, and boxes already
converted by hand are left alone. PP's labels bring PP's mistakes with them (a running head
it called a paragraph title becomes a heading), which review corrects like any other label.

What PP does not cover stays `abandon` (magenta), a holding class outside the taxonomy.
The export leaves out every page that still has one (`unresolved_abandon_pages` in the
manifest), so none reaches the training set by accident.

- **Finding the books.** A running head and a page number on every page make about 1.5
  abandon boxes per page (the pool's median is 1.8). The book manager flags (magenta square)
  books with 3 or more left per page and 20 or more in all (`abandon_flag_per_page`,
  `abandon_flag_min_boxes` in the settings file; 106 of the pool's books before abandon
  boxes took PP's labels, none after). The **Large
  abandon** column counts boxes covering 4% of the page or more. Despite
  `prep/image_pages_v2.py` treating them as picture candidates, PP-DocLayout-L calls 81% of
  them footnotes and under 1% images (59,201-page sample of the pool), so convert them to
  *footnote* after a look, not to *figure*.
- **One at a time.** In a page, **A** selects the next abandon box; a class key converts
  it like any reclassification. **Q** has a "with abandon boxes" queue.
- **In bulk.** **Shift+A** opens the convert dialog: the scope is this page or the whole
  book (the queue's book, set or split, at any review status), narrowed by where the boxes
  sit: top band, bottom band, side margins, large (4%+ of the page), elsewhere, or
  everywhere, each with its count. A class key or class chip converts them. So: convert the
  odd ones by hand, then send the remaining top band to *running head* and the bottom band
  to *page number* (Shift+A, Tab, the region, the class key). A page-scope conversion is one undo step;
  a book-wide one is logged in `bulk_ops`, and **U** in the dialog undoes the last one
  (boxes edited since keep the edit). `liminal-annotator convert <folder> --book ID
  --region top --to running_header` does it from the command line, and
  `convert --undo-last` reverts it.

## Review queues (Q)

Most pages need no edit: of the first 846 reviewed, the 449 with no picture from either
teacher needed an edit 4 times; the others about one time in five. **Q** opens the queue
picker, with the number of pages each queue holds in the current book (or all books):

| Queue | Pages (pool, 2026-11-09) | What |
|---|---|---|
| To do | 235,955 | Every page not yet reviewed, in book order |
| Picture pages, to do | 46,633 | A figure box, or a figure from the comparison model at 0.3 or more |
| Large pictures, to do | | A figure box covering 40% of the page or more (`large_figure_percent` in the settings file): page-filling plates whose box is cut short or misaligned. 7,711 of the 70,502-page project's 60,251 pages to do (2026-11-10). Best reviewed six at a time (G) |
| Pictures only the model sees | 5,759 | A model figure at 0.5 or more that no figure box overlaps (IoU 0.3): pictures the prefill missed. The overlay shows only the model's figures here |
| With abandon boxes | 9,580 | Any status; export skips these pages |
| Empty pages | 3,041 | No box at all: blank, a cover, or a plate both models missed |
| Plain text sample | 8,776 | One page in 20 with no figure, table or `abandon`: enough to check the prefill |
| Needs attention, Excluded, Reviewed, All pages | | |

Press 1–9, 0 or A (the eleventh row, all pages), or click a row. The queue stays when you open another book (if it has
pages there). The imported tiers (A/B/C) are not a queue: they measure whether the teachers
agreed on pictures, and tier A pages needed edits as often as tier C. The counts come from
per-page columns (`model_fig`, `model_only_fig`, `fig_area`) kept with the class counts; `--queue
pictures` (etc.) opens a queue from the command line, and `liminal-annotator queues
<folder> [--book ID]` prints the counts. Adding a queue after the pictures moved the keys of
the ones below it by one (only the model sees is now 4) and put all pages on A.

**Large figures from the command line.** `--figure-min PCT` keeps only pages whose editable
layer has a figure box covering at least PCT% of the page (`(x1-x0)*(y1-y0) >= PCT/100 ×
width × height`), at any status unless `--status` or `--queue` narrows it, and combines
with every other queue option. `large-pictures` is the same rule on pages to do, at the
settings file's `large_figure_percent` (40 by default; it is added to an existing settings
file by hand). For example `liminal-annotator C:\layout-dataset-v2-70k\annotator.db --queue
large-pictures --grid`, or `--figure-min 80 --queue todo --grid` for the whole-page plates.
Both read `fig_area`, the largest figure box of each page in image pixels, kept with every
save. It is added to an older project on its first open (the first `queues` run on a copy of
the 70,502-page project took 1.5 s, the next ones 0.11 s), and
pages saved since the last open by an earlier build, which does not keep it, are measured
again on every open (one pass over `pages`, 38 ms for 70,502 pages). On that project the
queue's page list takes 29 ms and the Q picker's counts 65 ms (47 ms without this queue),
against 120 ms and 230 ms for the same rule as a correlated `EXISTS` over the boxes.

**Adopting a teacher's box.** With the comparison layer shown (C: the imported layer or a
model), a click inside one of its dashed boxes where none of your boxes is copies it into
your boxes as one undoable edit and selects it; a class key then changes its class. A
click on one of your boxes still selects it (a picture YOLO boxed as text: click, then 2),
and a drag still draws.

**Status rules.** Right does not mark a page reviewed while it has `abandon` boxes, since
the export would then drop it without a word: it stays and says how many are left; a
second Right moves on and leaves the page to do. X or N pressed again restores the status
from before (a reviewed page stays reviewed instead of rejoining the to-do queue).

**Unsure boxes** (the prefill's score, or PP's for a box that took PP's label, under 0.5)
are drawn with long dashes until you change them. The top bar shows the session's pace in
pages per hour once a few pages are done (gaps over five minutes do not count).

## Six pages at once (G)

Where the boxes are almost always right and only need a look (the large pictures above
all), **G** shows six pages of the queue at once, three across and two down, over the whole
window: one header line (the queue, which pages, the keys or the last message) and the
pages, with a margin round the window's edge and round every page, so a box lying exactly
on a page edge (a whole-page figure) is drawn in that margin and never under a neighbour or
the window border. Each page is fitted into its cell with its boxes in the class colours;
figure boxes get a light fill and a thick outline set half outside the box. The label above
each page gives its number (1–6), book, file, status and its largest figure as a share of
the page. `--grid` on the command line (also for `liminal-annotator-gui.exe`) opens the
window in this view.

| Input | In the six-page view |
|---|---|
| Right | Accept the six: every page shown that is still to do is marked reviewed, then the next six. Nothing is accepted while one of them is still loading |
| Shift+Right / Left | Next / previous six, statuses unchanged |
| Home / End, PgUp / PgDn | First / last six of the queue; sixty pages back / forward |
| Ctrl+Home | The six with the first page still to do |
| Click a page, `1`–`6` | Open it in the editor. **G** or **Escape** there (Escape once nothing is being dragged or selected) come back to the six that hold the page, showing its edits |
| X / N | Exclude / flag as needing attention the page under the pointer; again gives back the status from before |
| G | The editor, on the page pointed at, else the first of the six still to do |
| Q, B, F1, F11 | Queues, books, help, full screen, as in the editor |

**What Right leaves to do**, with the reason in the page's label and in the message:

- **Opened, not accepted.** A page you open from the view and leave without Right (G,
  Escape, or Shift+Right to the next page in the editor) is passed by: Right in the view
  does not accept it. Right in the editor still does. This lasts for the session; a page
  that is only `opened` from an earlier session is accepted like any page to do.
- **Its image did not load**, exactly as in the editor: a page is never marked reviewed
  unseen.
- **`abandon` boxes left**: the first Right says how many pages have them and accepts
  nothing; a second Right accepts the others and leaves those to do, as Right does twice on
  one page.

Reviewed, excluded and flagged pages keep their status. Pages are grouped by six from the
start of the queue, so a page opened from the view always comes back to the same six. The
six shown, the next six and the six before are decoded ahead and kept in the image cache
(18 pages of 850×1280 take about 105 MiB of the 256 MiB budget), so Right shows the next six
at once. Edits made in the editor are saved exactly as before.

## Same on every page (M)

The teachers label one running head a heading on one page, a running head on the next and
`abandon` (now PP's label) on a third, although it has the same size and position on every
page. Select one such box (click it, or Tab) and press **M**, or click **Same on every page**
in the status bar. Every box of the book in the same place is found: the same vertical
position relative to the page, a similar height, and a width in the same range, at any
horizontal position (running heads swap sides on facing pages, and titles differ in length).
Only headings, running heads, running feet, page numbers and `abandon` boxes, plus the
selected box's own class, are considered, so the page number beside a running head (a
tenth of its width) and the first line of text below it are not caught. The dialog lists
the matches by their current label; Enter makes them all the selected box's class, a class
key or chip makes them that class. **Match** (Up/Down) is tight, normal or loose. On
george-rude-revolutionary-europe the running head selected on page 41 found 185 boxes on 128
of 178 pages, labelled heading 180 times and running head 5 times. It is one logged change:
U in the same dialog (or in Shift+A) undoes it.

## Keyboard and mouse

| Input | Behaviour |
|---|---|
| Right / Shift+Right | Accept the page (mark reviewed) and advance / advance without accepting. A page with `abandon` boxes is not accepted (see Review queues) |
| Left | Previous page; its status is unchanged |
| `1`–`9`, Shift+`1`–`5` | Active class for new boxes; reclassifies the selected box (physical number row, any layout) |
| Drag on blank page | Draw a box. **Shift+drag** draws even on top of an existing box (for example a whole-page ignore region) |
| Drag on a box's edge | Resize: the half of the edge you grab picks the corner (left half of the top edge = top-left). The opposite corner stays fixed; a drag past it clamps, never flips |
| Alt + edge drag | Move only one edge |
| Drag inside a box | Move it |
| Double click a figure box | The box becomes the whole page (a full-page picture the detector cut short). One undo step. Boxes of other classes only get selected |
| Wheel, `+`/`-` | Zoom around the cursor |
| Space+drag, middle drag | Pan |
| `F` / Ctrl+`1` | Fit page / actual pixels (1 screen pixel = 1 image pixel, pixel-exact) |
| Ctrl+Z / Ctrl+Y | Undo / redo. One gesture is one step |
| Ctrl+C, **Revert page** button | Revert the page to how it was when you opened it; press again for the original imported boxes. Each revert is one undo step |
| Delete | Delete the selected box |
| `U` | Toggle the selected box as an ignore region (`iscrowd=1`) |
| `X` / `N` | Exclude the page / flag it as needing attention (both undoable); again: back to the status before |
| `B` | Book manager |
| `M` | Same on every page: relabel every box of the book in the selected box's place |
| `A` / Shift+`A` | Select the next `abandon` box / convert `abandon` boxes in bulk |
| Ctrl+`O` | Open a dataset folder (or drop a folder or `.db` onto the window) |
| Tab / Shift+Tab | Select the next / previous box in reading order |
| Ctrl+arrows (+Shift) | Nudge the selected box by 1 image px (resize) |
| `V` / `T` / `H` | Outlines / class tags / toolbars on or off |
| `C` | Compare: imported (teacher) layer → each model's predictions → differences → off. A click inside a comparison box adopts it |
| `Q` | Review queues: picture pages, pictures only the model sees, abandon, empty, a text sample, … |
| Home / End, PgUp/PgDn | First / last page of the queue; jump 10 |
| Ctrl+Home | First page still to do |
| Escape | Cancel the current drag, then clear the selection, then (on a page opened from the six-page view) back to the six pages |
| `G` | Six pages at once (see above); from a page opened there, back to them |
| F1 / F11 / F12 | Help / full screen / performance overlay |

Clicking a label chip in the toolbar does the same as its number key. The hit band around every outline
is 8 logical pixels on each side, scaled for DPI, so it is the same physical size at every zoom. Where boxes
overlap, a corner of the selected box wins, then the closest edge, then the innermost box. On a box too
narrow for two bands, the middle third stays a move target. While you hover, a white marker shows which
corner will move.

Hotkey order, class colours, cache budget, prefetch depth, hit-band width and the
large-picture share (`large_figure_percent`, 40) are in
`<project>.settings.json`, which is created next to the project on first run. Hotkeys map to class names,
so changing them never changes the training taxonomy.

## What is stored, and where

`<project>.db` (SQLite, WAL) is the only live state:

| Table | Contents |
|---|---|
| `pages` | One row per page: book, split, set, image path, size, queue reason, review status, revision, priority, and the original COCO `images[]` entry verbatim |
| `annotations` | The editable (human) layer, xyxy in original image pixels |
| `imported_annotations` | The layer as imported. Never written after import; diffs and the delta report join against it |
| `predictions` | Model outputs, by model name |
| `events` | Append-only log of every committed page revision (bulk conversions add one per page) |
| `bulk_ops`, `bulk_op_items` | Each bulk conversion and the boxes it changed, for undo |
| `meta` | Schema version, the dataset folder and pre-fill teacher of a folder project, the last book |
| `book_tone` | Each book's measured paper colour, L\* and b\* |
| `book_flags` | Per book: excluded from training, needs work |

Each page row also keeps its count of boxes of every class (`n0`…`n13`), of `abandon`
boxes (all, large, as imported) and of the comparison model's figures (`model_fig`, and
`model_only_fig` for those no figure box covers; added on first open, about 5 s once) and
the area of its largest figure box (`fig_area`, for the large-picture queue),
updated with every save, bulk change and import, so the
book manager is one pass over a covering index (about 180 ms for 237k pages) rather than a
join over every box. Projects without the per-class counts get them on first open (about
4 s for the pool project, once). Projects from before these columns and before the
`abandon` class (schema 1) are upgraded in place when opened: the class check is widened by
rebuilding `annotations` in one transaction, and the counts are filled in (about 4 s for the
1.7 GB pool project, once).

A dedicated writer thread commits page snapshots in order (coalescing repeated snapshots of the same page)
and acknowledges each revision. The status bar shows `saving…` until the acknowledgement arrives.
Navigating away never waits for, or loses, a save: the page's document stays in memory until its write is
acknowledged. Page statuses are `unseen`, `opened`, `reviewed`, `excluded` and `needs_attention`. A page whose
image failed to load is **never** marked reviewed.

Import streams the COCO file element by element (`train.json`: 16,473 pages and 118,142 boxes in 2.5 s), and
keeps every field it does not interpret: image extras (`label_meta`, `pdf_sha1`, …) and per-box provenance
(`source`, `score`, `teachers`, `evidence`, `repairs`, …), including each box's exact source `bbox`, `area` and
`segmentation`.

## Export

`export` writes `<out>/<name>/` and builds it as `<out>/.<name>.partial`, renaming it only when complete. An
existing export is never overwritten, and source JPEGs and annotation files are only read.

| File | Contents |
|---|---|
| `original/annotations.json` | COCO at original resolution. Unedited boxes keep their source numbers byte for byte |
| `s640/annotations.json`, `s640/images/…` | Pages **stretched** to 640×640, as Lege's runtime contract requires (no letterbox), with boxes scaled per axis, `area` recomputed, and a triangle filter equivalent to PIL `BILINEAR` |
| `review.json` | The human corrections only (modified, added, deleted, per page), in original coordinates |
| `delta.json` | Corrections against the imported layer, by class, with a from→to reclassification table |
| `manifest.json` | Filter, counts per split, clamped and dropped boxes, split-leak check, file hashes |

Conventions kept: `category_id = taxonomy_id + 1`, `iscrowd = 1` for ignore regions (never converted to
background), and a box polygon as `segmentation`. Each exported box carries
`annotator: {id, change, source_id}`. Excluded pages are left out. `--reviewed-only` exports only accepted
pages. The export refuses to run if any book appears in more than one split (`--allow-split-leak` overrides).

## Model review and active learning

```powershell
& $A import-preds work\review.db preds.json --model rfdetr-s [--scale-from 640] [--set dev_gold_prefill]
& $A priority work\review.db --model rfdetr-s      # rank pages, then open with --priority
& $A compare work\review.db --model rfdetr-s       # metrics on reviewed pages only
```

- Predictions are a COCO results list (`page_key` or `image_id`, `category_id`, `bbox`, `score`).
  `--scale-from 640` un-stretches boxes predicted on the 640×640 input.
- `priority` scores pages by teacher/student text-vs-figure disagreement, text inside figures, class
  conflicts, unmatched boxes, hard layouts (aged/dark scans, gutters), rare classes and ignore regions.
  `--priority` then orders the queue by that score, **diversified by book**: books take turns, so one
  unusual book cannot use up the review budget.
- `compare` reports per-class precision/recall at IoU 0.5, text↔figure group confusion, figure recall,
  figure and text clipping. Ground truth is the editable layer of reviewed pages. Predictions and the
  imported layer are only read.

## Performance

`liminal-annotator bench [--image page.jpg] [--frames N]` drives the real render path headlessly at 1080p,
1440p and 4K. `cargo run --release --example raster_profile` times the page raster alone by zoom, strip size
and thread count. The **F12** overlay and `LIMINAL_PERF=1` (which logs every page switch to stderr) measure
the live window.

The spec's targets are < 16 ms input-to-feedback at 1080p and < 50 ms to show an already-prefetched page.
Both are met at every size, including 4K, with the margins below.

### Measured on a quiet machine (pass 2)

| | |
|---|---|
| CPU | Intel Xeon @ 2.10 GHz, 4 vCPU (4 cores × 1 thread, one socket), 16 GB. The guest has AVX2 and AVX-512; the build targets baseline x86-64 and uses neither |
| System | Linux 6.18 virtual machine, Rust 1.97.1, release profile (thin LTO), 4 render threads (`available_parallelism`) |
| Quietness | 60% idle, 0.4% steal over the final runs; no other jobs |
| Workload | `bench --frames 240`, built-in synthetic 850×1280 page, 3 runs per column, run back to back |

Frame times in ms. Each cell is **p50 range / p95 range** over the 3 runs, so the width of a range is the
run-to-run variance. *Before* is `lege-pdf-agent-over-poppler` at fd17bdd, *after* is the head of `annotator-pass2`.

| | | page raster (zoom frames) | corner-drag frame | wheel-zoom frame | pan frame | switch to prefetched page |
|---|---|---|---|---|---|---|
| 1080p | before | 1.18–1.84 / 2.20–2.79 | 1.14–1.16 / 1.30–1.42 | 2.16–2.81 / 3.27–3.72 | 2.07–2.49 / 3.00–3.15 | 4.0–6.1 / 4.8–7.1 |
| | after | 1.10–1.31 / 1.30–1.75 | 1.13–1.20 / 1.23–1.47 | 2.04–2.32 / 2.21–2.91 | 1.25–1.52 / 1.45–2.11 | 3.1–4.7 / 3.5–6.6 |
| 1440p | before | 2.55–3.05 / 3.75–4.03 | 1.75–1.86 / 1.98–2.26 | 4.22–4.69 / 5.57–5.95 | 3.48–4.44 / 4.61–5.51 | 5.7–6.8 / 7.4–8.1 |
| | after | 1.45–1.83 / 1.76–2.80 | 1.73–1.80 / 2.17–2.91 | 3.07–3.54 / 3.50–4.78 | 2.15–2.19 / 2.38–2.60 | 4.5–4.8 / 4.9–5.3 |
| 4K | before | 5.52–6.21 / 7.92–9.52 | 3.60–3.77 / 4.52–4.72 | 10.05–10.75 / 13.32–14.02 | 8.01–10.64 / 11.48–13.46 | 11.3–15.4 / 11.6–15.6 |
| | after | 2.46–2.80 / 2.90–3.50 | 3.56–3.68 / 4.20–4.45 | 6.24–6.62 / 6.92–8.45 | 4.89–4.93 / 5.69–5.84 | 9.1–9.7 / 9.1–11.4 |

The drag frame is unchanged: it only copies the cached raster and strokes outlines, and no pass-2 change
touches it. The switch column has four samples per size, so its p95 is the worst of four. The headless bench
does not include the window system: the host copies the whole window buffer to the compositor on every
present (33 MB at 4K, with softbuffer), and that cost is not in these numbers.

`raster_profile` isolates the page raster (1920×1016 and 3840×2032 canvases; p50 min–median over 5 interleaved
runs of before and after, and median p95), 4 threads:

| canvas, zoom | before p50 / p95 | after p50 / p95 |
|---|---|---|
| 1080p, 0.78 (zoomed out) | 1.64–1.66 / 2.20 | 0.82–1.00 / 1.44 |
| 1080p, 1.00 (not the exact-copy case) | 1.16–1.89 / 2.41 | 1.01–1.12 / 1.44 |
| 1080p, exactly 100% | 0.33–0.36 / 0.54 | 0.23–0.24 / 0.36 |
| 4K, 1.57 | 5.63–6.34 / 8.33 | 2.42–2.72 / 3.35 |
| 4K, 1.89 | 5.24–6.98 / 8.43 | 2.18–2.63 / 3.40 |
| 4K, 1.00 (not the exact-copy case) | 2.63–4.03 / 4.55 | 1.51–1.68 / 2.28 |
| 4K, 3× (nearest) | 0.98–1.45 / 1.76 | 0.88–1.08 / 1.32 |

Single-threaded the same rows go from 3.20 to 2.57 ms (1080p, 0.78), 3.94 to 2.76 (1080p, 1.00), 14.8 to 8.2
(4K, 1.57) and 17.3 to 8.4 (4K, 1.89). Single-threaded runs agreed to within a few percent;
the threaded ones are where the variance was.

With two busy-loop processes holding half the cores, the old threading degraded sharply and the new one holds
up (4 threads, p50 / p95): 4K at 1.57 went from 11.7 / 18.5 ms to 3.9 / 4.9 ms, and 1080p at 0.78 from
2.7 / 4.7 ms to 1.45 / 2.0 ms. That is the closest stand-in here for the development laptop, whose background
jobs kept the CPU at 97–100%.

### Measured on Windows (pass 3)

| | |
|---|---|
| CPU | Intel Core i7-13700H: 6 performance cores (12 threads) and 8 efficiency cores, 20 threads; 32 GB |
| System | Windows 11 Pro 26300, Rust 1.97.1, release profile, 8 render threads, mains power, browser and terminal open (about 30% load) |
| Method | old and new binaries alternated run by run (`bench --frames 240` three times each, `raster_profile` twice each), so background load falls on both |

The open item from pass 2 was whether thread creation, dearer on Windows, should change the
raster threading. It does: here the per-render `std::thread::scope` spawns showed as a p95
two to three times the p50 in the first, busier run, and the strip rows showed splitting
paid only from about 0.5 Mpx. A **persistent pool** (rayon, one pool per thread count, the
caller still works and bands are still claimed from a queue) replaces the spawns.
`raster_profile`, 8 threads, p50 / p95 in ms, each cell the range over two runs:

| canvas, zoom | spawn per render | persistent pool |
|---|---|---|
| 1080p, 0.78 | 0.42–0.43 / 0.48–0.51 | 0.32–0.33 / 0.41–0.47 |
| 1080p, 1.00 (not the exact-copy case) | 0.44 / 0.53–0.54 | 0.34–0.35 / 0.41–0.48 |
| 1080p, exactly 100% | 0.21–0.22 / 0.27–0.28 | 0.06–0.07 / 0.10 |
| 4K, 1.57 | 1.05–1.09 / 1.24–1.38 | 0.82–0.86 / 0.98–1.10 |
| 4K, 1.89 | 1.07–1.19 / 1.21–1.37 | 0.90–0.92 / 1.10–1.13 |
| 4K, 1.00 (not the exact-copy case) | 0.72–0.75 / 0.85–0.94 | 0.55–0.59 / 0.71–0.75 |
| 4K, 3× (nearest) | 0.56–0.62 / 0.72–0.78 | 0.38–0.39 / 0.49–0.51 |
| strip 1920×256 (491 kpx) | 0.25–0.27 / 0.32–0.45 | 0.13–0.14 / 0.15–0.18 |

In the whole frame the gain is smaller, because the blit of the cached raster and the
overlays do not change (`bench`, p50 ranges over three runs): page raster at 4K 1.16–1.19 →
1.06–1.10 ms, at 1440p 0.69–0.73 → 0.61–0.65 ms; switch to a prefetched page at 4K
4.5–5.6 → 4.0–4.3 ms. Drag frames are unchanged (1080p 0.55, 1440p 1.15, 4K 2.8 ms p50).
Every frame is far inside the 16 ms budget at every size. Single-threaded rows are unchanged,
as they should be: the pool changes scheduling, not arithmetic, and the bit-exactness tests
run through it.

With spawning gone, splitting pays on smaller areas, so a region is now split from 100k px
instead of 250k (a 4K pan strip is about 230k). Second interleaved pair, 8 threads, p50 in
ms, 250k → 100k: strip 1920×64 (122 kpx) 0.22 → 0.10, strip 1920×128 (245 kpx) 0.42 →
0.14–0.15; 61 kpx strips stay on one thread. At the frame level the difference was inside
the noise of that run (the machine was busier: single-threaded rows drifted by about 5%), so
it is kept for the raster numbers alone.

Also for Windows in this pass:

- `liminal-annotator-gui.exe`, the window without a console (GUI subsystem), for shortcuts,
  Explorer's "Open with" and pinning; errors that stop it from starting appear in a message box.
- The native folder dialog (`IFileOpenDialog`, through the `rfd` copy lege-gui vendors) on
  Ctrl+O, run off the UI thread so the window keeps painting; folders and `.db` files dropped
  onto the window open too.
- Recent projects in `%APPDATA%\liminal-annotator\recent.json`, compared case-insensitively.
- Paths are stored absolute but never canonicalised, so no `\\?\` prefixes reach the project
  or the export manifest.
- A pool import never opens or stats the image files (NTFS metadata for 230,000 files is
  the slow part on Windows); image paths come from the teacher rows.

### What was kept

1. **Pan by shifting the raster actually runs now.** The code existed but was dead: in the running app 0 of
   240 pan frames used it. `after_edit` cleared the raster key on every view change, and after any zoom the pan
   origin was an arbitrary float, so two consecutive origins never differed by an exact integer. The key
   already holds image, origin, scale and canvas rect, so the explicit clear is gone, and a pan starts by
   snapping the origin to a multiple of 2⁻²⁰ px (under a millionth of a pixel). From that grid, origin plus a
   whole number is exact, so a shifted raster is bit-identical to a full render. Now every pan frame shifts
   except the first after a zoom, which is a full render (239 of 239, and 239 of 240 at 1440p). Pan frame p50: 1080p 2.07–2.49 → 1.25–1.52 ms, 1440p
   3.48–4.44 → 2.15–2.19, 4K 8.01–10.64 → 4.89–4.93.
2. **Each source row is resampled horizontally once.** Bilinear output is a horizontal blend of two source
   rows followed by a vertical blend, and neighbouring output rows share source rows, so a two-row cache
   replaces three blends and four gathers per pixel with one vertical blend plus an amortised horizontal one.
   Output is bit-identical. At exactly 100% on whole pixels, rows are straight copies (1080p single thread
   0.60 → 0.47 ms; neutral at 4K, where it is memory-bound). The row kernels are separate functions: inlined into
   one `render_rows`, the nearest loop measured 5–47% slower from register spills alone.
3. **Row bands claimed from a queue, caller thread works.** The old code spawned one thread per equal chunk and
   the caller only joined, so a render took as long as its slowest chunk. It now spawns n − 1 threads, cuts
   rows into 4 bands per thread, and every thread claims bands from a shared queue. Raster p95 at 4K: 3.86–5.20
   → 2.90–3.23 ms, and the gap between runs narrows. It is also much less sensitive to a busy machine (above).

### What was measured and not kept

- **Persistent worker pool (rayon, same 4-band split).** On the 4-vCPU Linux machine it matched the scoped
  version and did not beat it (4K 1.00 p95 2.38 vs 1.84 ms; equal or slightly worse on every other row). The
  crate forbids `unsafe`, so a scoped persistent pool is not possible in std alone. Scoped spawn there costs
  about 25 µs a thread (100 µs p50 and 200 µs p95 for four, with rare 1–2 ms outliers). On Windows with
  8 threads it wins clearly and is now used; see pass 3 above.
- **Caller works, equal static chunks.** Nearest-neighbour got slower (1080p 3×: 0.34 → 0.45 ms).
- **Dirty rectangle for 4K drag frames.** A drag frame at 4K is 3.6 ms; about 1.9 ms of it is the blit of the
  cached raster, so the best case saves about half. It was not built: selection fills are alpha blends (not
  idempotent over painted pixels), and `draw_tag` places each tag around every other box's rectangle, so
  dragging one box can move other boxes' tags and the damage area is not local. A stale-pixel bug in an
  annotation tool costs more than 1.9 ms. The frame is also already about 4× under budget, and the host's
  whole-buffer present copy is untouched either way.
- **Thread count from the area drawn.** `raster_profile` now prints strips of 32–512 rows at 1, 2, 4 and 8
  threads. On 4 cores splitting pays from about 120k px per thread (1920×256: 0.76 ms on 1 thread, 0.33 on
  4) and the existing rule (1 thread below 250k px, otherwise all) already gives that. No change was
  measurable here.

### Open items

- The remaining cost at 4K is memory traffic: shifting the raster, blitting it, and the host's present copy.
  Drawing the page straight into the window buffer, or a ring-buffered raster, are the next steps if 4K ever
  needs more headroom.

The earlier laptop figures (i7-13700H, Windows, 8 threads, before the last optimizations) were drag frame p50
0.6 ms at 1080p, 1.0–1.2 ms at 1440p and 2.9–4.3 ms at 4K, and an isolated re-raster of 1.1 ms (1080p) and
3–4 ms (4K).

How the render path works:

- The page is resampled into a canvas-sized raster that is cached, so a drag only copies it and strokes
  outlines.
- Zoomed out, the page is drawn from a mip chain. From 1:1 to 2:1 it is bilinear on the original pixels.
  From 2:1 up it is nearest-neighbour, so every source pixel shows as a crisp square. At exactly 100% it is a
  straight copy.
- A pan by whole pixels shifts the cached raster and resamples only the newly exposed strips. This is
  bit-identical to a full render, and tests check it.
- The host repaints on every cursor move. Frames that would draw identical pixels are skipped.
- The current page and the 2 next and 1 previous pages are decoded in the background into a 256 MiB LRU.
  Every load carries a generation, so a slow decode never replaces a newer page.

## Tests

`cargo test` runs 98 tests:

- **`interaction`**: press and release inside one frame; edge-to-corner resize; Escape; pointer capture
  across the toolbar; zooming mid-drag; DPI-scaled hit bands; overlap priority; Shift-draw; skipped idle
  frames; page revert; a pan after a zoom that must shift the raster and match a forced full render pixel
  for pixel.
- **`coordinate_roundtrip`**: property tests over image↔screen mapping, zoom anchoring, fit/constrain and
  stretch round trips, plus checks that drags never flip a box or push it off the page.
- **`coco_roundtrip`**: lossless round trips, including the real 88-page regression set (read from
  `$LAYOUT_DATA_DIR` or the sibling checkout, skipped when absent); edit reporting;
  split-leak refusal.
- **`persistence`**: a child process that is **aborted** after acknowledged edits, and one aborted mid-write;
  navigation while saving; writer coalescing; a page whose image failed is never marked reviewed.
- **`dataset_scale`**: a 100,000-page project (800k boxes), with index-only lookups, under 5 ms per page,
  and app start-up checked.
- **`folder_project`**: a teacher-pool folder becomes `annotator.db` inside it, pages in page order with
  splits and the other teacher as a prediction layer; a rescan imports only new rows and reports a
  half-written line; region counts agree between SQL and the in-memory rule; bulk conversion by region and
  its undo (a box edited since keeps the edit); the export leaves out pages with unconverted `abandon`;
  a schema-1 project is upgraded in place; and, through the editor's key events, the book manager,
  `A`, Shift+`A` on a page with Ctrl+Z, and a book-wide top-band conversion with `U`.
- **`review_queues`**: each queue on a five-page pool, and a drawn figure leaving "only the
  model sees"; Q and a digit open a queue; a click adopts the model's picture, a second
  click selects it, a drag still draws, Ctrl+Z undoes; Right refuses a page with `abandon`
  left until it is labelled, and a second Right leaves it to do; X and N restore the
  earlier status; split assignment by stable hash; the pages-per-hour pace.
- **`shell`**: opening a folder imports it on a background thread and lands on the book manager;
  dropping it again opens it without an import; a plain start reopens the last project; a folder with
  no pages is an error on the start screen and leaves no `annotator.db` behind.
- **`grid_view`**: the large-figure filter and queue at the threshold (exactly 40% in, just
  under out, the largest figure decides, text boxes do not count), its count, a figure cut
  down or drawn bigger moving the page, the column added to an older project and refreshed
  after an older build's save; in the six-page view, which pages Right accepts (to do,
  including one opened when the app started) and which it leaves (image failed, `abandon`
  left after the warning, opened from the view and not accepted, reviewed, flagged,
  excluded), Shift+Right and Left; a click or a digit to a page cell (margins and gaps are
  none), Escape and G back to the same six with an edit made there saved.

## Design notes

- **LiminalKit is used unchanged**, as a git dependency that `Cargo.lock` pins to the same commit lege-gui
  uses (`Cargo.toml` shows the `[patch]` for building against a local checkout). The spec
  asks for an ordered pointer stream with capture, and for full physical-key events. The low-level
  `PixelApp` host already delivers mouse, key and scroll callbacks in order, with physical key names.
  So the app implements `PixelApp` directly, as the spec allows, and builds the queue and capture on top in
  `src/canvas/pointer.rs`. `src/canvas/` (viewport, pointer, hit-test, drag state machine, raster cache)
  knows nothing about COCO or layout classes, and is shaped to move into LiminalKit as a
  `liminalkit-canvas` crate.
- One gesture is one undo entry. The preview is always recomputed from the original box and the total
  pointer displacement, so it never drifts. Tentative geometry never reaches the database.
- On Windows, winit captures the mouse on press, so a drag that leaves the window still gets its release.

## Known limitations

- Very large pages (8,000–12,000 px) are decoded whole, at 4 bytes per pixel plus 33% for mips. That fits
  the current 1280-px dataset easily. Tiled or crop decoding is the next step if such pages appear.
- Only one window is supported.
- A folder project is a large file next to the images (1.7 GB for 232k pages, mostly the two
  copies of every pre-filled box: the editable layer and the immutable imported one).
- Model inference is not built in, by design: predictions are imported as files.
