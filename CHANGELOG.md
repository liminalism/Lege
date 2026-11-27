# Changelog

## 1.6.1 (2026-11-26)

### Processing

- **Binarization survives a GPU that runs out of memory.** When a page or tile does not fit in
  video memory, Best retries with smaller tiles (1024, then 768, then 512 px) and keeps the
  smaller size for the rest of the job, instead of failing the job.
- **A DjVu page that fails is an error, not a gap.** A page whose processing failed used to be
  left out of the DjVu file with only a log line; the job now stops and says which stage failed.
- **Preserve keeps the colour of scans that are already layered.** On a PDF whose pages are a
  colour image under a JBIG2 text mask (the usual shape of archive and Acrobat-compressed scans),
  Preserve passed the mask through but wrote the background in grey. The background is now colour
  JPEG 2000; the source's text mask is still carried over byte for byte.
- **Adaptive masking no longer erases photographs.** With layout detection off,
  `--grayscale --mrc-adaptive-mask` turned a full-page photograph into a few outlines on white;
  the picture now survives as it does with plain `--grayscale`.
- **TrueTyping's text can be searched and copied.** Each glyph learned its letter from the first
  word read over it, often a letter off, and kept it; an 886-page scan's text came out with 61% of
  its words right. The letters are now settled by vote across the book, ligatures (fi, fl, ff)
  and touching letters are read as the letters they are, word breaks follow the recognized words,
  and short words are no longer written a second time as invisible text: 94% of the words are
  right. The same book also needs one embedded font instead of three and comes out 22% smaller
  (8.5 MB; JBIG2 makes 15 MB).

### Faster, leaner PDF reading (exprim)

The PDF engine moves from the 2026-11-11 state that 1.6.0 was built with to exprim 0.4.5,
jp2lam 0.4.0 and jbig2enc-rust 0.6.2. Rendered pages are byte-identical; what changes is speed
and memory.

- Large pages render in horizontal strips, so a page's peak memory no longer grows several times
  over with its pixel count.
- JPEG 2000 images (common in scanned books) decode faster and in less memory; grayscale images
  are resampled as one channel instead of three.
- JBIG2 images decode faster (about 1.2x for generic regions, 2.3–2.7x for symbol and halftone
  regions), with fixes for malformed pattern dictionaries and region coordinates.
- Opening a large scan no longer reads its image data up front: compiling all 808 pages of a
  781 MB scan uses 38 MB instead of 755 MB and takes 0.78 s instead of about 1.8 s.

### Under the hood

- Lege now builds the PDF engine and codecs from GitHub (`liminalism/exprim`) at the commit pinned
  in `Cargo.lock`, so a fresh clone builds without a sibling checkout. `cargo latest` also
  refreshes Liminaldoc's git dependencies.
- The user guide covers Flatten page, Best/Fast binarization and the new layout model.
- `--debug-runtime-stats` reports the peak working set on Windows too (it read 0 there before).
- Every crate in the workspace declares its licence, AGPL-3.0-only.

## 1.6.0 (2026-11-14)

Published to the Microsoft Store on 2026-11-14 from commit `98dd5cc2`. The `1.6.0` git tag marks
an earlier commit (`1c97cfa1`, 2026-11-10) that has neither Flatten page nor the Best/Fast
binarization choice.

### Photographed pages

- **Flatten page** (GUI toggle beside Resolution, CLI `--flatten`) flattens photographed pages
  with the embedded Lege-Dewarp model. One photographed spread can become two pages
  (`--flatten-pages 1|2`); `--flatten-fast` makes one pass per page, faster and a little less
  straight. Flat scans are detected and
  left untouched, and a page the model cannot flatten passes through as it was. Works on folders,
  ZIPs and PDFs (a PDF's pages are treated as photos, so its outline and text layer do not carry
  over); EPUB and reflow jobs ignore it. Needs a hardware GPU.

### Binarization

- **Best or Fast.** The Text row offers Best, Fast and Grayscale; the CLI takes
  `--binarization best|fast`. Best (the default) is the tileable binarizer model on the GPU,
  cleaner on tinted and yellowed scans. Fast is adaptive binarization on the GPU, about twice the
  pages per second, for bulk jobs. Without a hardware GPU both fall back to the adaptive
  algorithm on the CPU, and a Best job says so once.
- The fixed threshold is no longer a method. `threshold`, `--threshold` and `--sauvola-k` are
  accepted with a note and change nothing; `heavy` and `--heavy` mean Best, `adaptive` means Fast.
- Pages over 1.6 megapixels are binarized in tiles, so very large pages no longer exhaust GPU
  memory.
- Leaving Grayscale for Best or Fast switches TrueTyping back to JBIG2, since TrueTyping needs
  grayscale outside Preserve; saved settings are normalized the same way.

### Layout detection

- **New layout model (v2).** RF-DETR-S trained on 64,785 pages of 547 books whose boxes were
  corrected in bulk and reviewed by hand on 16,090 pages. On hand-reviewed pages from books the
  model never saw, box accuracy (AP50:95) rises from 0.547 to 0.667 and from 0.689 to 0.789, and
  clipped text falls from 24.1% to 7.7% and from 17.8% to 3.8%, against the 1.5 model. Same size
  (58 MB) and speed.
- **The model's boxes are used as they are.** The corrections written for the previous models
  are gone: text boxes widened to ink, synthetic text and image boxes, the second detection pass
  on an inverted page, and the full-page figure expansion. Measured through Lege on 882 reviewed
  pages they lowered accuracy with the new model (AP50 0.486 with them, 0.663 without) and merged
  running headers and page numbers into text.
- Tables of contents, reference lists and formulas are no longer separate classes; they are text.
- `LEGE_LAYOUT_IMAGE_REDETECT` and `LEGE_LAYOUT_OCR_DET` no longer exist.
- **Pictures the model finds are kept as pictures.** The separate line-art test is removed. It
  judged by tone whether an image region was a drawing to be binarized with the text, and it also
  sent dark, light and vignetted photographs there as black blobs. Drawings and maps the model
  boxes as pictures are now grayscale or colour overlays too, which costs bytes: on a 200-page
  illustrated textbook the output is about 13% larger than 1.5.1, with 44 more pictures.

### Output

- **JBIG2 Symbol mode now applies to ordinary pages.** A header, footer or page number on a page
  used to force Generic mode, which in practice meant every page of a book. Generic is still the
  default.

### Window

- Smoother edges and crisper text; a file shelf with draggable tiles, one tile per kind of file
  with a count.
- A dropped file joins the queue at once; its page count follows.
- Progress lines keep their place while pages are added.
- Option tooltips appear in a free band clear of the buttons; the chosen output folder is the
  Output Directory button's label; a small label beside the cursor gives a file tile's full path.
- The options answer at once after a file is added, without waiting for its page count.
- Files can be added one after another: Add File, Add Folder, Output Directory and a drop no
  longer wait for the page count of the file added before. Process, which does need the page
  counts, says "Reading page counts" for the moment it has to wait.

### Under the hood

- The PDF engine (reading, rendering, printing) and the JBIG2, JPEG 2000 and DjVu codecs moved to
  the [exprim](https://github.com/liminalism/exprim) repository. Lege builds against a sibling
  checkout of it.
- Direct3D 12 is no longer used on Windows; Lege runs on Vulkan, which is also what reaches the
  GPU's tensor cores.
- The Sauvola model is no longer in the executable (0.4 MB). Heavy binarization runs the tileable
  binarizer, on the GPU as before and now also on the CPU when the GPU is off or fails. The
  `sauvola-model` feature embeds the old model again, and a `sauvola.onnx` beside the executable
  is still used as the CPU model.
- Dependencies are current as of the release, and none of Lege's own (exprim, the codecs,
  LiminalKit) is pinned to a commit. `cargo latest` moves LiminalKit to its newest commit and
  builds.
- `lege-titler` (in `lege-misc/pdf-titler`) fills in missing PDF titles and authors from page 1.
- Liminal Annotator (in `lege-misc/liminal-annotator`), the tool the layout set was reviewed in.

### Known

- A picture's box can stop a few pixels inside the picture; the rim outside it is binarized and
  shows as a dark edge beside the overlay.
- A running header that the model reads as a heading stays in cropped and reflowed output: 9 of
  585 on the reviewed test pages.
