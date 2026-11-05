# lege-pdf (agent tool)

A command-line tool and local MCP server for reading PDFs, built on Lege's
native PDF engine (the `pdf-*` crates). Binary: **`lege-pdf`**. Package:
**`lege-pdf-agent`**.

It covers what an agent does with a PDF it has not seen before:

| Question | Command |
|---|---|
| What is this document? | `inspect` — pages, metadata, outline, page sizes and numbering, text layer or scan |
| Where is what? | `map` — one line per page |
| What does it say? | `text` — plain, physical layout, blocks, words, characters; OCR on request |
| Where does it say X? | `search` — phrases across line breaks and hyphenation, regular expressions |
| What does it look like? | `render` — a page, a region of one, or a contact sheet |
| What is in that table? | `tables` — cells as Markdown, CSV or JSON |
| Where do the links go? What was commented? Which fields are filled in? | `annotations` |
| Is the file sound? | `check` — a preflight report |

plus `images`, `content`, `print`, a persistent stdio service (`serve`) and an
MCP server (`mcp`). `lege-pdf pdftotext` and `lege-pdf pdftoppm` take
Poppler's command lines, so existing scripts run unchanged.

It reads PDFs; it does not repair, rewrite or convert them. Known limits:
`tables` finds columns separated by white space, not by ruled lines alone;
`--layout blocks` finds paragraphs from line spacing, indents and short
last lines, so text set with none of those comes back as one block a column; OCR gives word boxes, not character boxes.

## Build

```sh
cargo build --profile debug-fast -p lege-pdf-agent --bin lege-pdf
# the same through the aliases, which name no profile of their own
cargo lege-pdf --profile debug-fast
cargo pdf-tool --profile debug-fast
```

The binary is `target/debug-fast/lege-pdf` (`lege-pdf.exe` on Windows).
Without `--profile` the aliases build the unoptimised `dev` profile into
`target/debug/`: fine for a compile check, far too slow to render or OCR
with, and not the build to register as an MCP server.

Install it as `~/.local/bin/lege-pdf` by copying that binary, or let Cargo
build and place it:

```sh
cp target/debug-fast/lege-pdf ~/.local/bin/          # lege-pdf.exe on Windows
# or, from a clean build:
cargo install --path lege-pdf/agent --root ~/.local --locked --force --profile debug-fast
```

Left without `--profile`, `cargo install` builds the `release` profile: fat
LTO and stripped symbols. It runs no faster, takes several minutes longer to
build, and is a third of the size (10 MB against 34 MB on Windows).

## Page numbering

- CLI page numbers are **one-based** (the first page is 1).
- JSON always includes both `page` (1-based) and `page_index` (0-based).
- Mixed conventions are rejected (e.g. page `0` is an error).
- The numbers printed on the pages (i, ii, 1, 2, …) are *page labels*;
  `inspect` and `map` report them next to the page number.

## Output contract

Records go to **stdout**; diagnostics to **stderr**.

```json
{
  "schema": "lege-pdf.agent/v1",
  "document": "/abs/path.pdf",
  "page": 1,
  "page_index": 0,
  "status": "ok",
  "warnings": [],
  "data": {}
}
```

| Flag | Behavior |
|---|---|
| (default) | Human-readable |
| `--json` | One pretty JSON envelope (commands that write a record per page still write one per line when there are several) |
| `--jsonl` | One JSON object per line |

Limits: `--max-pages`, `--max-items 10000`, `--max-bytes 8388608`,
`--timeout 60`. Each is enforced and each says so when it bites:

- `--max-pages` defaults to 50 for the commands whose output grows with
  every page (`text`, `images`, `render`, `inspect --pages`). The commands
  that scan and summarise (`search`, `map`, `check`, `annotations`, `tables`)
  cover every selected page unless you pass it. When it cuts a run short the
  result has `truncated: true` and `next_page`, and a warning names both.
- `--max-items` cuts a list (words, matches, images) and adds a warning.
- `--max-bytes` cuts a text record to fit (geometry first, then text), sets
  `status: "truncated"`, and the warning says how much was kept. In
  `serve`/`mcp` it is also the budget for one whole response.
- `--timeout` is a wall-clock budget for the command, or for each
  `serve`/`mcp` request. Pages finished in time are kept; the run then ends
  with a `status: "truncated"` record and exit code `1`. OCR of a single page
  and the document scan behind `inspect` are not interruptible and are caught
  at the next page boundary.

Multi-page work runs on every core and still writes its records in page
order; `--threads N` limits it. OCR runs one page at a time.

Exit codes: `0` success, `1` a page or the document failed or the run was cut
short by `--timeout`, `2` bad CLI. With `--json`/`--jsonl` every failure is
also a record on stdout, including one that happens before any page is read
(a missing file, a bad page range).

Page ranges accept `all`, `3`, `1-3`, `5-` (to the end), `-3` (from the
start) and comma lists. A range that runs past the last page is clamped with
a warning; a page or range wholly outside the document is an error.

Boxes are `[x0, y0, x1, y1]` in PDF points, origin at the bottom left of the
page. A box reported by `search`, `text`, `tables` or `annotations` can be
passed to `render --crop` or `text --bbox` as it is.

## Commands

```text
lege-pdf inspect FILE [--pages RANGE] [--outline-limit N]
lege-pdf map FILE [--pages RANGE]
lege-pdf text FILE [--pages RANGE] [--layout plain|physical|blocks|words|chars]
                   [--ocr never|auto|always] [--ocr-language eng]
                   [--bbox X0,Y0,X1,Y1]
lege-pdf search FILE QUERY [--pages RANGE] [--regex|--exact] [--case-sensitive]
                           [--context N] [--max-matches N]
                           [--ocr never|auto|always] [--ocr-language eng]
lege-pdf render FILE --output TEMPLATE [--pages RANGE]
                     [--dpi N|--scale N] [--max-edge PX] [--thumbnail]
                     [--crop X0,Y0,X1,Y1] [--gray]
                     [--format png|jpeg|ppm|pgm] [--quality 1-100]
                     [--contact-sheet [--columns N]]
lege-pdf tables FILE [--pages RANGE] [--format markdown|csv]
                     [--ocr never|auto|always] [--ocr-language eng]
lege-pdf annotations FILE [--pages RANGE] [--links] [--comments] [--forms]
lege-pdf check FILE [--pages RANGE] [--min-dpi N] [--strict]
lege-pdf images FILE [--pages RANGE] [--mode inventory|source|decoded|rendered]
                     [--extract DIR]
lege-pdf content FILE --page N [--ops] [--resources] [--objects]
lege-pdf print FILE [--printer NAME] [--list-printers] [--pages RANGE]
                    [--paper a4|letter|210x297mm|...] [--orientation portrait|landscape|auto]
                    [--margin PT|--margin-mm MM|--margin-in IN]
                    [--margin-left PT] [--margin-right PT] [--margin-top PT] [--margin-bottom PT]
                    [--scaling actual|fit|shrink|fill|NN%]
                    [--n-up 1|2|4|6|9|16|booklet] [--n-up-border]
                    [--n-up-order right-down|left-down|down-right|down-left]
                    [--duplex none|long|short] [--copies N] [--no-collate] [--reverse]
                    [--source-box crop|media|trim|bleed|art] [--gray] [--dpi N]
                    [--to-file DIR] [--dry-run [--query-device]]
lege-pdf serve --stdio [--max-open N] [--idle-timeout SECONDS]
lege-pdf mcp [--max-open N] [--idle-timeout SECONDS]
lege-pdf pdftotext [Poppler options] PDF [TEXT-FILE]
lege-pdf pdftoppm [Poppler options] PDF [PREFIX]
```

Every command also takes the global flags: `--json`, `--jsonl`, `--password`,
`--max-pages`, `--max-items`, `--max-bytes`, `--timeout`, `--threads`,
`--fail-fast`, `--system-fonts`.

### inspect and map

`inspect` answers "what is this" without touching the page contents more than
it must: page count, PDF version, metadata, the outline with the page each
item points to (`--outline-limit`, default 200 items), the page sizes in use,
the printed page numbering, encryption, forms and JavaScript, whether the
file had to be repaired to open, and a sample of up to five pages saying
whether they carry text or are scans. `--pages` adds boxes, rotation and
whether each of those pages draws cleanly.

`map` is one line per page, for every selected page:

```text
p.1 (i) 400x300 native-text 48 chars | Name Type Size
p.4 (2) 400x300 blank 0 chars
```

Page kinds: `native-text`, `hybrid` (text and images), `scanned-image` (no
text layer — pass `--ocr auto` to read it), `render-required` (nothing
trustworthy to extract: look at it), `blank` (nothing drawn), `failed`.

### text

- `plain` — reading order.
- `physical` — the page on a character grid, so table columns and side-by-side
  text columns keep their alignment; like `pdftotext -layout`.
- `blocks` — paragraph-like blocks with boxes. A **heuristic** (words → rows →
  reading order → blocks): columns of running text separated by a gutter are
  read one after the other, a row that crosses the gutter ends the columns
  above it, and a gap between short cells (a glossary, a table) is not treated
  as a gutter.
- `words` / `chars` — native geometry. A word broken by a line-end hyphen is
  joined in the page text (`international`) and reported as its two pieces
  (`inter-`, marked `continued`, and `national`), each with the box of its own
  line. `first_char`/`char_count` count characters of the page text.

OCR defaults to `never`. `auto` keeps trustworthy native text and OCRs only
scanned or low-trust pages; `always` renders and OCRs every selected page.
Every page reports `provenance`: where the text came from, the page kind, how
many images the page has, and the OCR engine and language when used. OCR
gives plain, physical, word and block output; character geometry from OCR
fails explicitly instead of pretending to be precise.

### search

By default a query is matched the way a reader would: case-insensitively,
across line breaks and line-end hyphenation, with ligatures (`ﬁ`) read as
their letters and typographic quotes and dashes as their plain forms.
`--exact` matches character for character; `--case-sensitive` keeps case;
`--regex` takes Rust `regex` syntax and sees each page as one line.

Every selected page is searched. Only pages with matches are written, each
match with its text, `--context` characters around it and the box of the
words it covers; then one summary says what was covered:

```json
{"summary": {"query": "clause", "match_count": 7, "pages_with_matches": 3,
             "pages_searched": 164, "pages_selected": 164,
             "pages_without_text": 2, "pages_failed": 0, "truncated": false}}
```

`pages_without_text` counts scanned pages that could not be searched without
OCR, so "no matches" is never mistaken for "not there". `--max-matches`
stops the run and reports `next_page` to resume from.

### render

`--output` is a path template supporting `{page}`, `{page_index}` and
`{stem}`; `--output -` writes one image to stdout (single page, human mode
only). Size comes from `--dpi` (default 150), `--scale`, or `--max-edge`,
which alone fits the longest edge to that many pixels and beside the others
only caps it. `--crop X0,Y0,X1,Y1` renders only that PDF-space rectangle;
the record's `region` is the rectangle the image covers after clamping to the
page and snapping to whole pixels. `--contact-sheet` tiles the selected pages
(up to 64) into one image, each under its page number — the cheapest way to
find the page with the figure. PNG output is 8-bit gray when the page is
gray, which most text pages are.

### tables

Tables are found from the alignment of text: runs of rows whose words line
up in columns separated by white space. Each comes back with its box, its
size and its cells, row by row; a wrapped cell stays on its own rows. Two
columns of running text are not reported as a table. Ruled tables whose
columns touch, and tables that are images, are not found — render those.

### annotations

Links with the text they sit on and where they go (a page of this document,
or a URL); comments and markup with their text, author, date and the text
they mark; form fields with name, type, value and options. With none of
`--links`, `--comments`, `--forms`, all three.

### check

A preflight report: findings with a severity (`error`, `warning`, `note`), a
code, the pages concerned and a sentence. Exit code `1` when there are
errors, or warnings with `--strict`.

| Code | Meaning |
|---|---|
| `page-failed`, `page-degraded` | a page cannot be read, or part of it cannot be drawn |
| `no-text-layer` | pages that are images only: not searchable without OCR |
| `unmapped-text`, `word-spacing-lost`, `text-outside-page` | text that will not copy, search or show correctly |
| `font-not-embedded` | text drawn with whatever the viewer substitutes |
| `low-resolution-image` | images below `--min-dpi` (default 100) on the page |
| `blank-page`, `mixed-page-sizes`, `no-title` | things worth knowing before printing or publishing |
| `repaired-on-open`, `encrypted`, `javascript`, `xfa-form` | facts about the file itself |

### images

Image extract modes are exclusive: inventory metadata vs encoded source vs
decoded samples vs a limited rendered appearance (RGB samples only).

## Poppler command lines

```sh
lege-pdf pdftotext -layout -f 3 -l 9 book.pdf out.txt
lege-pdf pdftoppm -png -r 100 -f 1 -l 1 -singlefile book.pdf cover
```

`lege-pdf pdftotext` and `lege-pdf pdftoppm` take the options of the Poppler
tools, write the files those tools write under the names they give them
(`prefix-07.png`, zero-padded to the document's page count; `book.txt` beside
`book.pdf` when no text file is named; `-` for stdout) and return their exit
codes (`0`, `1` cannot open, `2` cannot write, `99` other). A copy or link of
the executable named `pdftotext` or `pdftoppm` behaves the same way.

Pages are worked on in parallel. An option with no equivalent here is
refused by name rather than ignored — `-bbox`, `-tsv`, `-mono`, `-tiff` and a
few others; `-h` on either lists what is taken and what differs. The two
differences in output: the page is always its CropBox, and `-raw` gives the
same reading order as the default.

## Printing

`lege-pdf print` drives `lege-pdf-print`: office printing — paper, margins,
scaling, N-up, booklet, duplex, copies, page ranges. Prepress (CMYK,
separations, PDF/X) is deliberately out of scope; see `dev-notes/lege-pdf/print-PLAN.md`
§7.

A job takes one of two routes, and the record says which:

| `route` | when | what is spooled |
|---|---|---|
| `pass_through` | nothing changes page geometry **and** the queue accepts `application/pdf` | the original bytes, untouched |
| `composed` | any N-up, booklet, margin, non-crop source box, or geometry-changing scale — or a queue that only takes bitmaps (Windows always) | sheets we impose and rasterize at `--dpi` |

Modes, in order of how safe they are to run unattended:

- `--dry-run` reports the whole plan as JSON — route, sheet count, and every
  placement's source page, scale, rotation, translation, transform and cell —
  and **contacts no spooler at all**. It plans against an assumed device
  (quarter-inch hardware margins; PDF pass-through exactly where the platform
  has it), and says so via `device.source = "assumed"`. Pass `--query-device`
  to ask the real queue instead.
- `--to-file DIR` spools through the `file` backend, writing
  `document-NNNN.pdf` (pass-through) or `sheet-NNNN.png` (composed) into `DIR`.
  No real printer is touched.
- Neither flag spools for real, to `--printer` or the system default.

`--list-printers` enumerates queues (`backend`, `printers`, `default`) and
exits; it accepts `--to-file DIR` to enumerate the file backend instead of the
platform one, and needs no `FILE` argument.

Counting: `sheet_count` is printed **sides** for one copy (the length of
`sheets`), `paper_sheets` is physical sheets for one copy (duplex puts two
sides on one), and `total_sides` multiplies by `--copies`. Imposition emits a
single copy — `copies_applied_by` is `spooler`, because CUPS and winspool both
take a native copy count and multiplying in both places would print copies²
pages.

`--scaling` takes an explicit factor only with the `%`: the underlying value is
a multiplier where `1.0` is 1:1, so a bare `50` would be ambiguous between half
size and fifty times, and is rejected.

All print lengths in the JSON are PostScript points (`unit: "points"`).
`--pages` here also accepts `odd` and `even`, and an open-ended span such as
`8-`, on top of the `1,3-5` / `all` forms the other commands take.

**`print` is deliberately not exposed as an MCP tool or a `serve` method.**
Spooling actuates hardware, which is a side effect no bounded read contract
should hand to a model; and a planning-only tool would be a second, weaker
copy of `--dry-run`. Agents should shell out to `lege-pdf print … --dry-run`
(or `--to-file`) and leave the real submission to a person. If that changes,
the shape is a single read-only `pdf_print_plan` method in
`src/commands/serve.rs` returning the same `PrintPlanData`, with no route to
`submit`.

## MCP server

`lege-pdf mcp` is a newline-delimited JSON-RPC MCP server on stdio with ten
tools, listed in the order an agent meets a document:

| Tool | Returns |
|---|---|
| `pdf_inspect` | the document summary `inspect` prints |
| `pdf_map` | one line per page |
| `pdf_text` | page text under `--- page N ---` headers |
| `pdf_search` | one line per match: page, box, context |
| `pdf_render` | the page as an image in the result |
| `pdf_tables` | each table as Markdown |
| `pdf_annotations` | links, comments, form fields |
| `pdf_check` | the preflight findings |
| `pdf_images` | image inventory or extraction |
| `pdf_content` | the content stream of one page |

**Results are text meant to be read**, not JSON to be parsed: a model reads
`p.12 [72,410,523,424]: …the clause in question…` at a fraction of the tokens
of the same match as a JSON record. Pass `response_format: "json"` to any
tool for the structured form (compact JSON text plus `structuredContent`),
which is what carries geometry beyond the boxes shown. `pdf_text` with
`layout` `words` or `chars` is always JSON.

**Every result says what it covered.** `pdf_text` returns up to `max_chars`
characters (default 40000) starting at the first selected page; when it
stops, the first line names the page and `start_char` to continue from, and
the next call picks up at that character. `pdf_search` stops at
`max_matches` (default 50) and names the page to resume from. A page limit,
the byte budget and the time budget report the same way.

**`pdf_render` returns the image itself** when no `output` path is given:
sized for reading (longest edge 1568 pixels by default, `max_edge` up to
4096), PNG unless the page is a scan or photograph that is much smaller as
JPEG, at most six pages per call. `contact_sheet: true` puts up to 64 pages
on one image. `crop` takes a box from `pdf_search` or `pdf_text` to look
closely at a figure, a formula or a table. With `output` (a path template)
the images are written to disk and only described.

Register it with a client:

```sh
claude mcp add --scope user lege-pdf -- ~/.local/bin/lege-pdf mcp
codex mcp add lege-pdf -- ~/.local/bin/lege-pdf mcp
```

Equivalent `~/.codex/config.toml` configuration (use the absolute path of
the installed binary):

```toml
[mcp_servers.lege-pdf]
command = "/absolute/path/to/lege-pdf"
args = ["mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 60
default_tools_approval_mode = "writes"
```

The server accepts the established MCP initialization versions through
`2025-11-25`, while `tools/list` and `tools/call` can also be used directly by
clients that do not require a handshake. Paths refer to the machine running
the server. Tools are annotated read-only except `pdf_render` and
`pdf_images`, which can write files.

The initialization instructions give the usual order of tools, explain the
coverage lines and the box convention, and ask for one thing in return: when
a result is wrong or unusable, use another PDF tool for that file, page and
operation only, and report it — with `akr papercut --about lege-pdf` in a
workspace that has an AKR ledger, otherwise to the user — leaving document
contents, passwords and private paths out of the report.

## Native serve protocol

`serve --stdio` is the lower-level Lege JSONL service the MCP adapter is
built on. It is retained for shell integrations that do not need MCP.

Newline-delimited JSON on stdio:

```json
{"id":1,"method":"ping","params":{}}
{"id":2,"method":"pdf_inspect","params":{"path":"/doc.pdf","pages":"1-3"}}
{"id":3,"method":"pdf_text","params":{"path":"/doc.pdf","pages":"1","layout":"words"}}
{"id":4,"method":"close","params":{}}
```

Methods: `ping`, the ten `pdf_*` tools above with the same parameters, and
`close`/`shutdown`. Results are always the structured form. `pdf_render`
needs an `output` path here: this protocol carries no images.

Every result carries `warnings`, and the multi-page methods report their
coverage, so it never has to be inferred from the number of pages that came
back:

```json
{
  "page_count": 164,
  "pages_selected": 164,
  "truncated": true,
  "next_page": 51,
  "pages": [{ "page": 1, "page_index": 0, "data": {} }],
  "warnings": ["page limit max-pages=50 reached: 50 of 164 selected pages returned; pages from 51 on were not processed"]
}
```

`truncated` is set by the page limit, the character, match or byte budget,
or the time budget; `next_page` (with `next_char` when the stop fell inside a
page) is where to resume. A page that fails is returned in place with an
`error` instead of `data` and does not end the request.

Snapshots are cached by canonical path + size + mtime with LRU eviction
(`--max-open`, default 4), so a second call on the same document does not
open it again.

## Examples

```sh
lege-pdf inspect book.pdf
lege-pdf map book.pdf --pages 1-40
lege-pdf search book.pdf "force majeure"
lege-pdf search book.pdf 'section \d+\.\d+' --regex --max-matches 20 --jsonl
lege-pdf text book.pdf --pages 12-14
lege-pdf text book.pdf --pages 12 --layout physical
lege-pdf text book.pdf --pages 1-5 --layout words --jsonl
lege-pdf text scans.pdf --pages 1-5 --ocr auto --layout blocks --jsonl
lege-pdf tables report.pdf --pages 7 --format csv
lege-pdf annotations book.pdf --links --json
lege-pdf check book.pdf --strict
lege-pdf render book.pdf --pages 12 --output p{page}.png --max-edge 1600
lege-pdf render book.pdf --pages 12 --crop 72,410,523,640 --dpi 300 --output fig.png
lege-pdf render book.pdf --pages 1-30 --contact-sheet --output sheet.png
lege-pdf pdftotext -layout book.pdf -
lege-pdf pdftoppm -png -r 100 book.pdf page
lege-pdf print book.pdf --dry-run --n-up 2 --margin-mm 10 --json
lege-pdf print book.pdf --to-file /tmp/spool --pages odd --json
lege-pdf print book.pdf --printer HP_LaserJet --duplex long --copies 2
```

## Relation to `pdfr`

`pdfr` (`pdf-cli`) remains the developer driver with free-form dumps.
`lege-pdf` owns the versioned agent contract.
