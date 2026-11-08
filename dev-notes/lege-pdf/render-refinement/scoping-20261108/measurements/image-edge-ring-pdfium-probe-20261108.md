# Opaque image border ring: PDFium probe, 2026-11-08

The scoping review left open whether an opaque, unmasked image placed at a
fractional device position should get a hard 1-px edge (the fast paths' pixel-
centre rule) or the generic body's A10 fractional coverage (a soft edge, as
every masked path paints). Option A (soften the fast paths) was implemented on
`scoping-r2-stencil` as `107f7b8b`, with the agreed rule that a PDFium probe
would decide: hard for axis-aligned if PDFium snaps, soft for rotated.

## Probe

`ring.pdf` (generator `mkring.py`, kept beside the stencil fixtures): three
images on a (51,51,51) backdrop at 612×792 pt, rendered at scale 1 through
`tools/pdfium-diff render --reference pdfium` against the PDFium build at
`deprecated-files/DocBrakeGUI/pdfium.dll` (2025-09).

| Image | Placement (pt) | Device edges |
|---|---|---|
| I: 48×36 RGB magnified, nearest | `150.3 0 0 112.6 50.45 600.7 cm` | x 50.45–200.75, y 78.7–191.3 |
| B: 400×300 RGB minified | `120.7 0 0 90.35 300.2 620.4 cm` | x 300.2–420.9, y 81.25–171.6 |
| G: 300×200 1-bit gray minified | `140.6 0 0 93.3 60.4 400.55 cm` | — |

PDFium pixels at the edges (row 130 and column 100 or 306):

| Edge | Coverage of boundary pixel | PDFium | Option A (ours, `107f7b8b`) |
|---|---|---|---|
| I left, x=50 | 55% | (0,112,160) full image colour | (23,84,111) blend |
| I right, x=200 | 75% | (235,112,160) full | (189,97,133) blend |
| I top, y=78 | 30% | (51,51,51) backdrop | (58,36,84) blend |
| I bottom, y=191 | 30% | (80,245,160) full | (58,109,84) blend |
| B top, y=81 | 75% | (60,90,31) full | (58,80,36) blend |
| B bottom, y=171 | 60% | (60,90,87) full | (56,74,73) blend |
| B left, x=300 | 80% | (41,90,32) full | (43,82,36) blend |
| B right, x=420 | 90% | (87,90,32) full | (84,86,35) blend |

PDFium rounds an axis-aligned image's destination rectangle to whole device
pixels and paints it solid: a boundary pixel is either fully image or fully
backdrop, with the split at the pixel centre (x=50 painted for a 50.45 start;
y=78 not painted for a 78.7 start). That is exactly the pre-`107f7b8b` fast-path
rule. Option A moved axis-aligned draws away from PDFium and would have put a
faint lighter line on the right or bottom edge of every scanned page rendered at
a DPI that is not a multiple of 72.

## Decision

`107f7b8b` is reverted. Axis-aligned opaque unmasked images keep the hard,
pixel-centre ring (PDFium's behaviour); rotated and sheared draws stay on the
generic body and keep A10's fractional coverage. The companion fix `4a175132`
(the generic bilinear sampler keeps an opaque image's alpha at 255 instead of
254) is kept: it is a real sampling bug, independent of the ring.

Still open, now with a direction: masked images on axis-aligned fractional
placements (the generic body, including Lege's own MRC output) paint a soft
ring where PDFium would paint a hard one. Aligning them means making the
generic body snap axis-aligned image edges, which changes MRC page edges and
needs its own gate; it is not a refactor. The ring test
(`fractional_placements_match_the_generic_body_including_the_border_ring`)
holds unmasked axis-aligned paths to the interior and masked paths to the whole
surface, and documents the divergence.

Interior note from the same probe: on the magnified nearest image PDFium
samples (80,0,160) where we produce (75,0,160); a pre-existing nearest-sampling
difference unrelated to the ring.
