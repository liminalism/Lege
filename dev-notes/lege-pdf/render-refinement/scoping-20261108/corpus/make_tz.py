"""B4 probe PDF: horizontal scaling (Tz) on glyph shapes.

Page 0: embedded DejaVuSans (TrueType). Page 1: non-embedded Helvetica.
Rows (each its own text object):
  line_tz100 / line_tz50 / line_tz200 : 'MMMM WWWW' at Tz 100/50/200
  m_tz100 / m_tz50 / m_tz200          : a single 'M' at Tz 100/50/200
  line_cm50 / line_cm200              : 'MMMM WWWW' at Tz 100 under a
                                        [0.5 0 0 1] / [2 0 0 1] cm (what a
                                        correct Tz 50 / Tz 200 must look like)
  m_cm50 / m_cm200                    : single 'M' under the same cm
Writes <out>.pdf and <out>.json (row baselines in PDF points).
"""
import json
import sys

from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas

out = sys.argv[1]
W, H = 820, 790
SIZE = 48
X0 = 30
ROW_STEP = 70
pdfmetrics.registerFont(TTFont("DejaVu", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"))

rows = [
    ("line_tz100", "MMMM WWWW", 100, 1.0),
    ("line_tz50", "MMMM WWWW", 50, 1.0),
    ("line_tz200", "MMMM WWWW", 200, 1.0),
    ("line_cm50", "MMMM WWWW", 100, 0.5),
    ("line_cm200", "MMMM WWWW", 100, 2.0),
    ("m_tz100", "M", 100, 1.0),
    ("m_tz50", "M", 50, 1.0),
    ("m_tz200", "M", 200, 1.0),
    ("m_cm50", "M", 100, 0.5),
    ("m_cm200", "M", 100, 2.0),
]

c = canvas.Canvas(out + ".pdf", pagesize=(W, H), pageCompression=0)
manifest = {"page_size_pt": [W, H], "font_size": SIZE, "pages": []}
for font in ("DejaVu", "Helvetica"):
    page_rows = []
    for i, (label, text, tz, sx) in enumerate(rows):
        baseline = H - 50 - i * ROW_STEP
        c.saveState()
        if sx != 1.0:
            c.transform(sx, 0, 0, 1, 0, 0)
        t = c.beginText()
        t.setTextOrigin(X0, baseline)
        t.setFont(font, SIZE)
        t.setHorizScale(tz)
        t.textOut(text)
        c.drawText(t)
        c.restoreState()
        page_rows.append(
            {"label": label, "text": text, "tz": tz, "cm_sx": sx, "baseline_pt": baseline,
             "x_origin_pt": X0 * sx}
        )
    manifest["pages"].append({"font": font, "rows": page_rows})
    c.showPage()
c.save()
with open(out + ".json", "w") as f:
    json.dump(manifest, f, indent=1)
print("wrote", out + ".pdf")
