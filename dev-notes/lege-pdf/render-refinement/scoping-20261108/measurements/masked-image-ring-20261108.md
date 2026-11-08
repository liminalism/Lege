# Masked image border ring: PDFium comparison, 2026-11-08

Follow-up to [image-edge-ring-pdfium-probe-20261108.md](image-edge-ring-pdfium-probe-20261108.md).
Commit `cd576089` (branch `scoping-r3-ring`) makes the generic image body give
an axis-aligned draw (`inv.b == inv.c == 0` exactly, mirrors included) the
pixel-centre rule at its outer edge, the rule the opaque fast paths already
use: a border pixel is painted at full weight when its centre lies inside the
destination rectangle and not at all otherwise. Before it, every draw through
the generic body (`/ImageMask` stencils, `/SMask`, `/Mask`, and so Lege's MRC
text layers) painted A10 fractional coverage there, a blend with the backdrop.
Rotated and sheared draws keep A10.

## Setup

Fixtures regenerated with `mkring.py` and `mkstencil.py` (corpus directory of
these notes). Rendered at scale 1 (612×792) with
`tools/pdfium-diff render <pdf> --pages … --scale 1 --reference pdfium`
(`--features renderer-pdfium`, `PDF_RENDERER_PDFIUM_LIB` set to
`deprecated-files/DocBrakeGUI/pdfium.dll`, the 2025-09 build the first probe
used). "Before" is the same build with the new branch in `edge_coverage`
switched off, which is the A10 behaviour of `0f3a6516`.

## PDFium's rule, corrected

PDFium paints every device pixel an axis-aligned image's rectangle touches, at
full weight, masked or not. It never blends a border pixel, and it never leaves
one blank either: even 10–30 % slivers are painted. On the magnified nearest
image `I` of ring.pdf its texel boundaries fit a nearest sampling of the image
stretched over the outward-rounded rectangle `[floor(x0), ceil(x1)) ×
[floor(y0), ceil(y1))` (151 of 151 columns and 111 of 114 rows on column 100
and row 130), against 139/151 and 104/114 for the true rectangle. That is
`GetOuterRect()` plus a stretch, not a pixel-centre split.

The first probe got this wrong for `I`'s top edge. Its table gives (51,51,51),
the backdrop, at y=78 (30 % covered). This build paints (80,0,160) there; y=77
is the backdrop. Its own row for `I` bottom (30 % covered, painted) already
contradicted the pixel-centre reading. The same stretch explains the interior
note at the end of that probe ((80,0,160) in PDFium against our (75,0,160)):
PDFium's texel phase is shifted by the rounding.

## Boundary pixels

One sample per edge: the boundary pixel, in PDFium, Lege before and Lege
after, and the pixel one step inside. "Centre" says whether the pixel's centre
lies inside the destination rectangle (the pixel-centre rule's test).

| Image | Edge, pixel | Coverage | Centre | PDFium | Lege before (A10) | Lege after | Inside: ours / PDFium |
|---|---|---|---|---|---|---|---|
| ring I, RGB8 magnified (fast path) | top y=78.7, (100, 78) | 30% | out | (80, 0, 160) | (51, 51, 51) | (51, 51, 51) | (75, 0, 160) / (80, 0, 160) |
| ring I, RGB8 magnified (fast path) | bottom y=191.3, (100, 191) | 30% | out | (80, 245, 160) | (51, 51, 51) | (51, 51, 51) | (75, 245, 160) / (80, 245, 160) |
| ring G, 1-bit gray (fast path) | left x=60.4, (60, 340) | 60% | in | (0, 0, 0) | (0, 0, 0) | (0, 0, 0) | (0, 0, 0) / (0, 0, 0) |
| ring G, 1-bit gray (fast path) | top y=298.15, (130, 298) | 85% | in | (0, 0, 0) | (0, 0, 0) | (0, 0, 0) | (0, 0, 0) / (0, 0, 0) |
| ring G, 1-bit gray (fast path) | bottom y=391.45, (130, 391) | 45% | out | (0, 0, 0) | (51, 51, 51) | (51, 51, 51) | (0, 0, 0) / (0, 0, 0) |
| stencil_flate p2 B, `/ImageMask` minified, `/Decode [1 0]` | left x=50.25, (50, 400) | 75% | in | (128, 0, 0) | (160, 64, 64) | (128, 0, 0) | (128, 0, 0) / (128, 0, 0) |
| p2 B | right x=550.55, (550, 400) | 55% | in | (128, 0, 0) | (185, 115, 115) | (128, 0, 0) | (128, 0, 0) / (128, 0, 0) |
| p2 B | top y=90.7, (300, 90) | 30% | out | (128, 0, 0) | (217, 178, 178) | (255, 255, 255) | (128, 0, 0) / (128, 0, 0) |
| p2 B | bottom y=731.4, (300, 731) | 40% | out | (128, 0, 0) | (204, 153, 153) | (255, 255, 255) | (128, 0, 0) / (128, 0, 0) |
| p2 M, `/ImageMask` minified | left x=20.5, (20, 120) | 50% | tie, painted | (0, 102, 0) | (128, 179, 128) | (0, 102, 0) | (0, 102, 0) / (0, 102, 0) |
| p2 M | right x=220.7, (220, 120) | 70% | in | (0, 102, 0) | (38, 72, 0) | (0, 102, 0) | (0, 102, 0) / (0, 102, 0) |
| p2 M | top y=40.8, (120, 40) | 20% | out | (0, 102, 0) | (204, 224, 204) | (255, 255, 255) | (0, 102, 0) / (0, 102, 0) |
| p2 M | bottom y=191.7, (120, 191) | 70% | in | (0, 102, 0) | (42, 79, 7) | (0, 102, 0) | (0, 102, 0) / (0, 102, 0) |
| p3 S′, `/ImageMask` magnified, mirrored x and y | left x=260.7, (260, 393) | 30% | out | (77, 77, 77) | (202, 202, 202) | (255, 255, 255) | (77, 77, 77) / (103, 103, 103) |
| p3 S′ | right x=400.8, (400, 393) | 80% | in | (159, 159, 159) | (113, 113, 113) | (77, 77, 77) | (77, 77, 77) / (147, 147, 147) |
| p3 S′ | top y=391.9, (262, 391) | 10% | out | (181, 181, 181) | (237, 237, 237) | (255, 255, 255) | (77, 77, 77) / (181, 181, 181) |
| p3 S′ | bottom y=502.6, (270, 502) | 60% | in | (77, 77, 77) | (148, 148, 148) | (77, 77, 77) | (77, 77, 77) / (77, 77, 77) |
| p5 T, `/ImageMask` truncated data | left x=20.6, (20, 150) | 40% | out | (0, 0, 0) | (153, 153, 153) | (255, 255, 255) | (0, 0, 0) / (0, 0, 0) |
| p5 T | bottom y=291.7, (100, 291) | 70% | in | (0, 0, 0) | (77, 77, 77) | (0, 0, 0) | (0, 0, 0) / (0, 0, 0) |

PDFium smooths the mirrored magnified stencil S′ in its interior too (grey
inside pixels such as 103, 147, 181 where the texel is solid (77,77,77)), so
its S′ rows compare interior sampling as much as edges. That is a separate,
pre-existing difference.

Whole edges, counting the boundary pixels where both renderers agree on the
pixel one step inside (so interior sampling differences are left out), over
ring I and G, stencil_flate p2 B and M, p3 S, V and S′, p5 T and G:

| Boundary pixels | Count | Equal to PDFium before | Equal to PDFium after |
|---|---|---|---|
| centre inside (coverage over 50 %) | 3,344 | 1,162 (fast-path images only) | 3,343 |
| centre outside, or an exact 50 % tie on a top or right edge | 2,741 | 0 | 0 |

Every masked boundary pixel was a blend before, equal to PDFium nowhere. After
the change the masked draws behave like the opaque fast paths:

- **Centre inside (coverage over 50 %):** now equal to PDFium (3,343 of 3,344;
  one pixel on p2 B's left edge differs, not investigated).
- **Centre outside (coverage under 50 %):** not equal. Lege leaves the backdrop
  where PDFium paints the full pixel; before, Lege painted a blend of under
  50 %. The opaque fast paths have always done the same (ring I top/bottom and
  G bottom in both columns). At an exact 50 % tie the half-open `[0, 1)` test
  paints a left or bottom edge and not a right or top one, as the fast paths
  do; PDFium paints all four.

So the change makes masked and unmasked axis-aligned images agree, with hard
edges like PDFium's, and it matches PDFium on the edges a pixel centre falls
inside. It does not reach PDFium on the far side of the centre: those pixels
were a minority blend and are now the backdrop, one pixel short of PDFium.

## What matching PDFium fully would take

Snap each axis-aligned image's destination rectangle outward to whole device
pixels when it is prepared (`GetOuterRect`), and stretch the image over it
(recompute `inv`, `bounds` and `footprint`). Then no axis-aligned draw has a
fractional edge, the fast paths and the generic body agree without any edge
rule, and the texel phase moves to PDFium's (the (75 vs 80) interior note). It
changes the fast paths' output too: every opaque image on a fractional
placement gains its far-side sliver pixels and shifts its sampling by up to a
pixel. That needs its own decision and corpus gate. With it, the pixel-centre
rule in `edge_coverage` would apply only to near-axis-aligned draws (off-
diagonals not exactly zero).

## Corpus effect of `cd576089`

`probe run --hash --fresh-backend`, new rule against the A10 rule in the same
build, scales 1.0 / 1.37 / 2.0, RGBA and Gray8 (`PROBE_GRAY8`):

| Document | Pages that change |
|---|---|
| beans, vogtle, viaduct, xenosystems (305 pages), sahib_processed_1794115972 | none, at every scale and format |
| ring.pdf | none (all three images take opaque fast paths) |
| stencil_flate.pdf | pages 2–5 at every scale; page 1 (the full-page stencil at 0 0 612 792) at 1.37 only |
| bilevel_600.pdf | page 2 (the full-page `/ImageMask`) at 1.37 only |

sahib's JBIG2 layers are unmasked 1-bit DeviceGray images (the LUT-nearest and
bilevel box fast paths), so no real document in the corpus changes. MRC pages
whose text layer is an `/ImageMask` (Lege's own output, `stencil_jbig2.pdf`)
change at scales where the page edge is fractional, like bilevel_600 page 2;
stencil_jbig2 was not regenerated here (it needs `mkjbig2`).

### Addendum: full gate after the merge (`9c0b9fe3`)

The 16-document gate (scales 1.0/1.37/2.0, RGBA and Gray8) against the
round-3 hashes gives 2,162 of 2,202 renders identical. Besides the stencil
pages above, `cmyk16`, `gray16` and `rgb16` change at 1.37 only: full-page
16-bit images that `ring_unchanged_by_promotion` keeps on the generic body,
whose far edge (column 838 of 838.44, row 1085 of 1085.04) was an A10 blend
and is now the backdrop, with the previous column and row fully painted.
Edge-only, the same rule as the stencils.
