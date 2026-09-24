# lege-docwrite

A page-native book writing environment. You write straight into the typeset
book: the pages on screen are the pages that print. Underneath, the
manuscript is semantic structure — book, part, chapter, section, block, run —
with paragraph styles, page masters and chapter templates, so the book stays
easy to reorganize, restyle and export.

It is deliberately not a general word processor: no track changes, mail
merge, forms, spreadsheets, drawing tools, macros or Word compatibility. The
nouns are **book, chapter, paragraph, page, source and style**.

## Running it

From the Lege-ecosystem root (the pixelkit checkout is expected beside it, at
`../pixelkit`):

```sh
cargo docwrite-run -- Book.legebook               # open, or start, a book
cargo docwrite-run -- --fullscreen Book.legebook  # start in full-screen writing mode
cargo docwrite-run -- --source Letters.pdf Book.legebook   # with a source PDF beside it
cargo docwrite-run -- export Book.legebook Book.pdf        # headless export (.pdf or .md)
```

Or from this directory: `cargo run -p docwrite-app -- …`. With no book
argument it opens `Untitled.legebook` in the current directory.

To try it on a sample book:

```sh
cargo run -p docwrite-app --example demo_book -- Demo.legebook
cargo run -p docwrite-app -- Demo.legebook
```

`examples/snapshot` paints the editor without a window and saves a PNG
(`-- BOOK.legebook OUT.png [page] [--scale 2] [--fullscreen] [--spread]
[--source S.pdf]`), which is handy for checking layout without a display.

## Writing

- **Typing into the typeset page.** Paragraphs reflow and repaginate as you
  type; the view follows the caret onto whatever page it lands on.
- **Selection and editing:** click to place the caret, arrows, Option-arrows
  by word, Cmd-arrows and Home/End to paragraph ends, Up/Down by typeset line
  (across page turns), Shift to extend any of these; cut, copy and paste
  through the system clipboard; undo and redo, one step per edit.
- **Smart punctuation while typing:** straight quotes curl open or closed by
  context, an apostrophe inside a word becomes ’, `--` becomes an em dash and
  `...` an ellipsis. One undo gives back what was typed. Pasted text is left
  alone.
- **Character formatting:** bold, italic, small caps, superscript and
  subscript, on a selection or — with nothing selected — for what you type
  next. Small caps use the font's own (`smcp`) or are synthesized when it has
  none.
- **Paragraph styles:** Body, Subhead, Block Quote, Epigraph, Verse, Caption
  and Scene Break (an empty scene break shows as `*  *  *`), plus the
  chapter-title, first-paragraph, contents and bibliography styles books use
  automatically.
- **Find and replace:** find ignores case and wraps round the book; replace
  replaces every match as one undo step. Replacement text is used as typed
  (case is not matched).
- **IME:** input methods compose at the caret (preedit is drawn underlined)
  and their candidate window follows it.

## The book

- **Chapters and parts.** New chapters come from a chapter template; the
  Book Map on the left lists parts, chapters and named sections. Click a
  chapter to go to it, drag one onto another to reorder the book, click a
  part to fold it.
- **Chapter templates** say how a chapter opens — next page or next recto —
  which page master it uses, its title, first-paragraph and body styles, and
  whether its opener shows a folio. Changing a template changes every
  chapter that uses it. A chapter's title is set on its opener page in the
  template's title style.
- **Page masters** set margins, facing pages, folios and running heads within
  the book's trim size (6 × 9 in by default). Versos mirror the margins.
- **Footnotes** are numbered through the book, marked in superscript in the
  text, set at footnote size under a short rule at the foot of the page of
  their reference, and continue line by line onto later pages when long.
- **Endnotes** are numbered on their own and set in a **Notes** section after
  the last chapter.
- **Table of contents** (optional, per book): a Contents page before the first
  chapter, each chapter with the page it opens on, flush right, kept current
  as the book changes.
- **Bibliography:** entries (typed in, or imported from BibTeX or CSL JSON)
  are set in a **Bibliography** section, sorted by author, with italic titles
  and hanging indents.
- **Index:** terms attached to paragraphs are set in an **Index** section with
  the pages they appear on.

## Typography

- Real OpenType shaping (harfrust) with kerning and ligatures, run by run in
  the family's regular, bold, italic and bold-italic faces. The editor uses
  Georgia, then Times New Roman, then Noto Sans; set `LEGE_DOCWRITE_FONT`
  (and optionally `LEGE_DOCWRITE_FONT_BOLD`, `_ITALIC`, `_BOLD_ITALIC`) to
  another TrueType family.
- Justified text with a ragged last line (spaces never open wider than two
  ems), left, centered and right alignment, first-line and hanging indents,
  left and right indents, space before and after.
- Hyphenation (US English patterns) with the hyphen drawn at the break.
- Widow and orphan control, keep-with-next and keep-lines-together.
- Drop caps that span their lines: sized from the font's cap height, set on
  the lower baseline, with the lines beside them indented.
- Old-style figures where the font has them.

## Views

- **Page view:** the page fills the window's height, with the next page below
  it; scrolling is continuous during a gesture and settles on a page when the
  gesture ends. PageDown and PageUp always move exactly one page.
- **Spread view:** facing pages side by side — page 1 alone on the right, then
  2|3, 4|5 … — with PageDown and PageUp moving a spread at a time.
- **Resizable Book Map:** drag its right edge to any width; drop it narrow and
  it folds into a tab at the window's edge, which brings it back at its old
  width. Cmd-\ does the same.
- **Full-screen writing mode,** in the manner of WordPerfect 5.1: borderless
  full screen, no chrome, the page edge to edge across the screen in light
  text on blue, and a status line with the book's title and
  `Doc 1  Pg N  Ln x.xx"  Pos y.yy"` for the caret.
- **Toolbar:** B, I, small caps, x², x₂ (lit when the selection has them), the
  caret's paragraph style (click to cycle), body size −/+, new chapter,
  rename, footnote, endnote, source, spread, contents, and the page count.
  Commands that need text open a one-line prompt in the toolbar: Enter
  commits, Esc cancels.

## Research

Open a source PDF beside the manuscript (Source… in the toolbar, Cmd-Shift-O,
or `--source`). The pane shows one page at a time with ‹ › to turn pages.

1. Drag across words to select a passage.
2. **Capture** keeps it as a Source Note — document, page, passage and the
   passage's position on the page — and asks how to cite it (the default is
   the file name and page).
3. **Quote** inserts the passage in quotation marks followed by the
   citation; **Cite** inserts the citation alone.
4. Click a citation in the manuscript to reopen its source at that page with
   the passage highlighted.

A chapter's sources are the ones it cites.

## Files and export

- **Books are `.legebook` folders:** `book.json` (title, styles, masters,
  templates, notes, sources, bibliography, index, selection, chapter order)
  and one `chapters/<id>.json` per chapter (blocks, their kinds, and every
  run's marks). Nothing is lost on reopen; ids are kept.
- **Saving:** autosave writes in the background once typing pauses for a
  moment and never blocks the keyboard; Cmd-S saves now; the book is saved on
  quit. Each save builds the new folder beside the old one and swaps them, so
  a crash leaves one of the two intact (a left-over `.Name.legebook.old` is
  opened if the swap was interrupted). Named snapshots can be saved and
  restored from the editor API.
- **Older bundles** (`manifest.txt` and text chapters) still open and are
  rewritten in the current format on the next save.
- **PDF** (Cmd-E, next to the book): the typeset pages exactly as laid out —
  visible, searchable text in subset TrueType fonts (one per face used), with
  a ToUnicode map from the shaping, chapter bookmarks, `/Lang`, page labels
  and the footnote rules.
- **Markdown** (Cmd-Shift-E): headings per chapter, paragraphs, bold and
  italic runs, links, block quotes, images and footnotes; page geometry is
  dropped.
- **IDML** (`export_idml`, from the API): styles, a parent spread and the
  story with footnotes — see the limitations below.
- **Preflight** (`preflight`, from the API): missing or unembeddable fonts,
  low-resolution images and broken references.

## Keys

Cmd is Ctrl on Windows and Linux; Option is Alt.

| Keys | Does |
|---|---|
| Cmd-S | Save now |
| Cmd-Z / Cmd-Shift-Z (or Cmd-Y) | Undo / redo |
| Cmd-X / Cmd-C / Cmd-V | Cut / copy / paste |
| Cmd-B / Cmd-I | Bold / italic |
| Cmd-Shift-H | Small caps |
| Cmd-Shift-= / Cmd-Shift-− | Superscript / subscript |
| Cmd-Option-0 … 6 | Body, Subhead, Block Quote, Epigraph, Verse, Caption, Scene Break |
| Cmd-F / Cmd-G | Find / find again |
| Cmd-Option-F | Replace all |
| Cmd-Shift-N / Cmd-Shift-R | New chapter / rename chapter |
| Cmd-Option-N / Cmd-Option-E | Footnote / endnote |
| Cmd-Option-I | Index the caret's paragraph under a term |
| Cmd-Option-B / Cmd-Option-Shift-B | Add a bibliography entry (`Author; Title; Year`) / import .bib or CSL JSON |
| Cmd-Option-T | Table of contents on or off |
| Cmd-Shift-O | Open a source PDF |
| Cmd-Shift-P | Spread view on or off |
| Cmd-\ | Fold or unfold the Book Map |
| F11 or Cmd-Shift-F / Esc | Full-screen writing mode / leave it |
| Cmd-E / Cmd-Shift-E | Export PDF / Markdown next to the book |
| PageDown / PageUp | Next / previous page (or spread) |
| Up / Down | Previous / next typeset line |
| Option-Left / Option-Right (Ctrl- on Windows and Linux) | Word back / forward |
| Cmd-Left / Cmd-Right, Home / End | Start / end of the paragraph |

## Performance

Typing never waits on the rest of the book: an edit reshapes only the
paragraph it touched and repaginates forward only until the page breaks line
up with the previous layout. In a release build, typing into a 550-page book
takes about 2 ms from keypress to painted frame (p50 1.9 ms, max 2.2 ms,
against an 8 ms budget); the book opens in about 40 ms. The gate is
`cargo test --release -p docwrite-app --test latency_gate -- --nocapture`.

## Not done yet

- Images are not placed in the page flow (an image block lays out as text).
- Right-to-left and mixed-direction text is not handled.
- The IDML package has no spreads or threaded text frames, and has not been
  opened in InDesign.
- CFF-outline (OpenType/PostScript) fonts cannot be embedded in the PDF; the
  export says so. TrueType families work.
- Links and citations are not clickable in the PDF; there is no colour or
  general vector drawing beyond gray rules.
- The PDF is not really tagged: lege-pdf-write writes a placeholder structure
  tree (one element per paragraph, not tied to the page content).
- No EPUB export.
- Named snapshots have no command in the window yet.
- The window has been exercised through the editor's API, offscreen
  snapshots and short launches, not a long interactive writing session.

These are tracked in the project's AKR ledger (`.akr/`, milestone M7);
`docs/generated/ROADMAP.md` is rendered from it.

## Layout of the code

A nested Cargo workspace inside Lege-ecosystem, excluded from its root
workspace, path-depending on `lege-pdf/*` and on `../pixelkit`.

| Crate | What |
|---|---|
| `docwrite-model` | Book tree, blocks, runs and marks, styles, masters, templates, notes, sources, find/replace, undoable edits, bundles |
| `docwrite-typeset` | Font families and shaping, line breaking, hyphenation, justification, incremental pagination, footnotes, generated sections, glyph atlas |
| `docwrite-export` | PDF, Markdown, IDML, preflight |
| `docwrite-app` | The `lege-docwrite` binary on pixelkit: page canvas, Book Map, toolbar, full screen, spreads, research pane |

Tests: `cargo test` (93 tests) and `cargo clippy --all-targets` (clean).
