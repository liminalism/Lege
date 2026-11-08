"""Two-page PDF that draws ONE JPEG image XObject at two sizes.

usage: python3 -I mk2page.py IN.jpg OUT.pdf [--reverse]

page 1: the JPEG as a 1.5-inch-wide thumbnail (plus a caption)
page 2: the same JPEG full page (612x792 pt)
--reverse swaps the pages (full page first, thumbnail second).

reportlab registers an image XObject once per (filename, mask) digest, so
both drawImage calls reference the same object; A85 is disabled so the
stream is a bare /DCTDecode (the bytes are the input JPEG unchanged).
"""
import sys

from reportlab import rl_config

rl_config.useA85 = 0

from reportlab.pdfgen import canvas  # noqa: E402

src, out = sys.argv[1], sys.argv[2]
reverse = "--reverse" in sys.argv[3:]
c = canvas.Canvas(out, pagesize=(612, 792), pageCompression=0)


def thumb():
    w = 108.0
    h = w * 3300.0 / 2550.0
    c.setFont("Helvetica", 10)
    c.drawString(72, 720, "thumbnail draw of the shared JPEG XObject")
    c.drawImage(src, 72, 720 - 8 - h, w, h)
    c.showPage()


def full():
    c.drawImage(src, 0, 0, 612, 792)
    c.showPage()


if reverse:
    full()
    thumb()
else:
    thumb()
    full()
c.save()
print("wrote", out)
