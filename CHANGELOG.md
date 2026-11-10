# Changelog

## 1.6.0 (2026-11-10)

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

### Under the hood

- The PDF engine (reading, rendering, printing) and the JBIG2, JPEG 2000 and DjVu codecs moved to
  the [exprim](https://github.com/liminalism/exprim) repository. Lege builds against a sibling
  checkout of it.
- Direct3D 12 is no longer used on Windows; Lege runs on Vulkan, which is also what reaches the
  GPU's tensor cores.
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
