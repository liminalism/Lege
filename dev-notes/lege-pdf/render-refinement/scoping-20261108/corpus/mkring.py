"""ring.pdf: opaque unmasked images at fractional device positions."""
import sys
sys.path.insert(0, '.')
from mkstencil import Pdf, text_bitmap

pdf = Pdf()
w, h = 48, 36
rgb = bytes(v for y in range(h) for x in range(w)
            for v in ((x * 5) % 256, (y * 7) % 256, 160))
img = pdf.stream("/Type /XObject /Subtype /Image /Width %d /Height %d "
                 "/ColorSpace /DeviceRGB /BitsPerComponent 8" % (w, h), rgb)
big = bytes(v for y in range(300) for x in range(400)
            for v in (40 + (x % 50), 90, 30 + (y % 80)))
imgbig = pdf.stream("/Type /XObject /Subtype /Image /Width 400 /Height 300 "
                    "/ColorSpace /DeviceRGB /BitsPerComponent 8", big)
g1 = pdf.stream("/Type /XObject /Subtype /Image /Width 300 /Height 200 "
                "/ColorSpace /DeviceGray /BitsPerComponent 1", text_bitmap(300, 200, 3, ink=0.9))
page = pdf.page(
    "0.2 0.2 0.2 rg 0 0 612 792 re f "
    "q 150.3 0 0 112.6 50.45 600.7 cm /I Do Q "      # magnified RGB nearest
    "q 120.7 0 0 90.35 300.2 620.4 cm /B Do Q "      # minified RGB area
    "q 140.6 0 0 93.3 60.4 400.55 cm /G Do Q",       # minified 1-bit gray
    {"I": img, "B": imgbig, "G": g1}, size=(612, 792))
pdf.write("out/ring.pdf", [page])
