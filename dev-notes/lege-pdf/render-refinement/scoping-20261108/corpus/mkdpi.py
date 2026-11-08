"""One-page PDF whose page is a single full-page DCT image of a given size.

usage: python3 -I mkdpi.py SRC.jpg OUT.pdf PAGE_W_PT PAGE_H_PT IMG_W IMG_H [QUALITY]

The source scan is resampled (LANCZOS) to IMG_W x IMG_H and re-encoded as a
baseline JPEG; content stream is `q W 0 0 H 0 0 cm /Im0 Do Q` (same shape as
the corpus scans).
"""
import io
import sys

from PIL import Image

src, out = sys.argv[1], sys.argv[2]
pw, ph = float(sys.argv[3]), float(sys.argv[4])
iw, ih = int(sys.argv[5]), int(sys.argv[6])
q = int(sys.argv[7]) if len(sys.argv) > 7 else 80
im = Image.open(src)
im = im.resize((iw, ih), Image.LANCZOS)
b = io.BytesIO()
im.save(b, "JPEG", quality=q, optimize=False)
jpg = b.getvalue()
cs = "/DeviceGray" if im.mode == "L" else "/DeviceRGB"
content = ("q %.4f 0 0 %.4f 0 0 cm /Im0 Do Q\n" % (pw, ph)).encode()
objs = [
    b"<< /Type /Catalog /Pages 2 0 R >>",
    b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    ("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %.4f %.4f] /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>" % (pw, ph)).encode(),
    b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream",
    ("<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace %s /BitsPerComponent 8 /Filter /DCTDecode /Length %d >>\nstream\n" % (iw, ih, cs, len(jpg))).encode() + jpg + b"\nendstream",
]
pdf = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
offsets = []
for i, body in enumerate(objs, 1):
    offsets.append(len(pdf))
    pdf += b"%d 0 obj\n" % i + body + b"\nendobj\n"
xref = len(pdf)
pdf += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
for o in offsets:
    pdf += b"%010d 00000 n \n" % o
pdf += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
open(out, "wb").write(pdf)
print("wrote", out, iw, ih, len(jpg))
