"""Side-finding repro PDF: two embedded Type 1 fonts used in alternating
first-use order on consecutive pages, so the per-page FontId numbering
(order of first use) swaps every page. A renderer cache that hands one page's
FontId-indexed parsed programs to the next page draws with the wrong program.
"""
import os
import sys

import reportlab
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfgen import canvas

out = sys.argv[1]
pages = int(sys.argv[2]) if len(sys.argv) > 2 else 40
fdir = os.path.join(os.path.dirname(reportlab.__file__), "fonts")
names = []
for stem in ("DarkGardenMK", "callig15"):
    face = pdfmetrics.EmbeddedType1Face(os.path.join(fdir, stem + ".afm"),
                                        os.path.join(fdir, stem + ".pfb"))
    pdfmetrics.registerTypeFace(face)
    pdfmetrics.registerFont(pdfmetrics.Font(stem, face.name, "WinAnsiEncoding"))
    names.append(stem)

c = canvas.Canvas(out, pagesize=(400, 300))
for k in range(pages):
    order = names if k % 2 == 0 else names[::-1]
    y = 220
    for name in order:
        c.setFont(name, 36)
        c.drawString(20, y, f"{name[:4]} Hamburg {k}")
        y -= 90
    c.showPage()
c.save()
print("wrote", out)
