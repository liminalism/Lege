"""repeat_image.pdf: one image XObject drawn many times per page (A7). Stdlib only.

usage: python -I mkrepeat.py <outdir>

Three pages. Each draws a single 1200x1600 8-bit gray Flate image (a scan-like
page, ~1.9 MB inflated) twelve times at thumbnail size, the way a contact
sheet, a repeated logo or a tiled texture would. Page 2 gives the image a
resource-named colour space (`/CS0` -> `/DeviceGray`), the case the per-compile
cache must not serve; page 3 draws the same image as an RGB 800x1000 one.
"""
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mkstencil import Pdf  # noqa: E402


def gray_image(w, h, seed):
    rng = random.Random(seed)
    rows = []
    for y in range(h):
        base = 210 + (y * 30) // h
        row = bytearray(min(255, base + rng.randint(-8, 8)) for _ in range(w))
        if (y // 30) % 2 == 0 and y % 30 < 18:
            x = 100
            while x < w - 100:
                ww = rng.randint(20, 120)
                if rng.random() < 0.7:
                    for xx in range(x, min(x + ww, w - 100)):
                        row[xx] = rng.randint(20, 60)
                x += ww + rng.randint(10, 24)
        rows.append(bytes(row))
    return b"".join(rows)


def rgb_image(w, h, seed):
    rng = random.Random(seed)
    out = bytearray()
    for y in range(h):
        g = 180 + (y * 60) // h
        for x in range(w):
            out += bytes((min(255, g + rng.randint(-5, 5)), (x * 255) // w, 255 - g))
    return bytes(out)


def grid(name, cols=4, rows=3, cell=(140, 190), origin=(20, 40), gap=(10, 50)):
    ops = []
    for j in range(rows):
        for i in range(cols):
            x = origin[0] + i * (cell[0] + gap[0])
            y = origin[1] + j * (cell[1] + gap[1])
            ops.append("q %g 0 0 %g %g %g cm /%s Do Q" % (cell[0], cell[1], x + 0.3 * i, y + 0.6 * j, name))
    return " ".join(ops)


pdf = Pdf()
out = sys.argv[1]
os.makedirs(out, exist_ok=True)
W, H = 1200, 1600
data = gray_image(W, H, 5)
g = pdf.stream("/Type /XObject /Subtype /Image /Width %d /Height %d "
               "/ColorSpace /DeviceGray /BitsPerComponent 8" % (W, H), data)
g_named = pdf.stream("/Type /XObject /Subtype /Image /Width %d /Height %d "
                     "/ColorSpace /CS0 /BitsPerComponent 8" % (W, H), data)
rgb = pdf.stream("/Type /XObject /Subtype /Image /Width 800 /Height 1000 "
                 "/ColorSpace /DeviceRGB /BitsPerComponent 8", rgb_image(800, 1000, 6))

page1 = pdf.page("0.97 0.97 0.95 rg 0 0 612 792 re f " + grid("G"), {"G": g})
# Page 2: the colour space is a resource name; the page's /ColorSpace dict maps it.
cid = pdf.stream("", ("0.97 0.97 0.95 rg 0 0 612 792 re f " + grid("N")).encode())
page2 = pdf.add((
    "<< /Type /Page /Parent PARENT /MediaBox [0 0 612 792] "
    "/Resources << /XObject << /N %d 0 R >> /ColorSpace << /CS0 /DeviceGray >> >> "
    "/Contents %d 0 R >>" % (g_named, cid)).encode())
page3 = pdf.page("1 1 1 rg 0 0 612 792 re f " + grid("R"), {"R": rgb})
pdf.write(os.path.join(out, "repeat_image.pdf"), [page1, page2, page3])
print("wrote", os.path.join(out, "repeat_image.pdf"))
