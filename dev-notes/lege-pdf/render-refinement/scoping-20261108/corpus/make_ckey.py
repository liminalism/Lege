"""B5 probe PDF: colour-key /Mask on 8-bit DeviceGray images vs gray->RGB promotion.

Every image has the quadrant pattern TL=0, TR=200, BL=200, BR=0 (image row 0
is the top). Colour-keyed images carry /Mask [195 205] (gray) or
[195 205 195 205 195 205] (RGB), so a conforming renderer shows the red
page background through the 200-valued quadrants and black elsewhere.

exec.rs promotes Gray8 -> RGB8 when: Normal blend, no masked clip, no soft
mask, alpha == 255, axis-aligned (inv.b == inv.c == 0), colour space Gray,
and (footprint[0] > 1 || footprint[1] > 1 || no smask/mask); then
gray8_promoted_to_rgb8 needs: not stencil, bpc 8, no /Decode, no sample
LUT (8-bit has none), n <= 64 Mpx. Placements:

  row 1 (y = 290..490)
   G_min        1000x1000 gray, key, 200x200 pt          -> minified 5x  (PROMOTED: bug expected)
   G_mag         100x100  gray, key, 200x200 pt          -> magnified 2x (not promoted)
   G_min_rot90  1000x1000 gray, key, rotated 90 deg      -> minified, inv.b/c != 0 (not promoted)
   G_min_decode 1000x1000 gray, key, /Decode [0 1]       -> minified, decode set (not promoted)
   G_min_ca50   1000x1000 gray, key, /ca 0.5             -> minified, alpha != 255 (not promoted)
  row 2 (y = 40..240)
   R_min        1000x1000 RGB,  key, 200x200 pt          -> minified RGB (control; never promoted)
   R_mag         100x100  RGB,  key                      -> magnified RGB (control)
   G_nomask     1000x1000 gray, NO mask                  -> what an unmasked draw looks like
   G_1to1        200x200  gray, key, 200x200 pt          -> 1:1 at scale 1 (footprint 1.0)
   G_min_xonly  1000x100  gray, key, 200x200 pt          -> x minified 5x, y magnified 2x (PROMOTED)
Writes <out>.pdf and <out>.json (placements in PDF points + texel mapping).
"""
import json
import sys
import zlib

out = sys.argv[1]
W, H = 1120, 520


def quad_gray(w, h):
    rows = []
    for r in range(h):
        top = r < h // 2
        left = bytes([0 if top else 200]) * (w // 2)
        right = bytes([200 if top else 0]) * (w - w // 2)
        rows.append(left + right)
    return b"".join(rows)


def quad_rgb(w, h):
    g = quad_gray(w, h)
    return bytes(b for v in g for b in (v, v, v))


objs = {}


def image_obj(num, w, h, cs, data, extra=""):
    comp = zlib.compress(data, 9)
    d = (f"<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /{cs} "
         f"/BitsPerComponent 8 {extra} /Filter /FlateDecode /Length {len(comp)} >>")
    objs[num] = d.encode() + b"\nstream\n" + comp + b"\nendstream"


GKEY = "/Mask [195 205]"
RKEY = "/Mask [195 205 195 205 195 205]"
image_obj(5, 1000, 1000, "DeviceGray", quad_gray(1000, 1000), GKEY)
image_obj(6, 100, 100, "DeviceGray", quad_gray(100, 100), GKEY)
image_obj(7, 1000, 1000, "DeviceGray", quad_gray(1000, 1000), GKEY + " /Decode [0 1]")
image_obj(8, 1000, 1000, "DeviceGray", quad_gray(1000, 1000), "")
image_obj(9, 1000, 1000, "DeviceRGB", quad_rgb(1000, 1000), RKEY)
image_obj(10, 100, 100, "DeviceRGB", quad_rgb(100, 100), RKEY)
image_obj(11, 200, 200, "DeviceGray", quad_gray(200, 200), GKEY)
image_obj(12, 1000, 100, "DeviceGray", quad_gray(1000, 100), GKEY)
objs[13] = b"<< /Type /ExtGState /ca 0.5 >>"

# name, xobject, cm, image size
placements = [
    ("G_min", "Gk1000", [200, 0, 0, 200, 20, 290], (1000, 1000), None),
    ("G_mag", "Gk100", [200, 0, 0, 200, 240, 290], (100, 100), None),
    ("G_min_rot90", "Gk1000", [0, 200, -200, 0, 660, 290], (1000, 1000), None),
    ("G_min_decode", "GkD1000", [200, 0, 0, 200, 680, 290], (1000, 1000), None),
    ("G_min_ca50", "Gk1000", [200, 0, 0, 200, 900, 290], (1000, 1000), "GSa"),
    ("R_min", "Rk1000", [200, 0, 0, 200, 20, 40], (1000, 1000), None),
    ("R_mag", "Rk100", [200, 0, 0, 200, 240, 40], (100, 100), None),
    ("G_nomask", "G1000", [200, 0, 0, 200, 460, 40], (1000, 1000), None),
    ("G_1to1", "Gk200", [200, 0, 0, 200, 680, 40], (200, 200), None),
    ("G_min_xonly", "Gk1000x100", [200, 0, 0, 200, 900, 40], (1000, 100), None),
]
content = [f"1 0 0 rg 0 0 {W} {H} re f"]
for name, xo, cm, _, gs in placements:
    g = f"/{gs} gs " if gs else ""
    content.append(f"q {g}{' '.join(str(v) for v in cm)} cm /{xo} Do Q")
content = "\n".join(content).encode()
objs[4] = f"<< /Length {len(content)} >>".encode() + b"\nstream\n" + content + b"\nendstream"
objs[1] = b"<< /Type /Catalog /Pages 2 0 R >>"
objs[2] = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"
objs[3] = (f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {W} {H}] /Contents 4 0 R "
           "/Resources << /XObject << /Gk1000 5 0 R /Gk100 6 0 R /GkD1000 7 0 R "
           "/G1000 8 0 R /Rk1000 9 0 R /Rk100 10 0 R /Gk200 11 0 R /Gk1000x100 12 0 R >> "
           "/ExtGState << /GSa 13 0 R >> >> >>").encode()

buf = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
offsets = {}
maxnum = max(objs)
for num in sorted(objs):
    offsets[num] = len(buf)
    buf += f"{num} 0 obj\n".encode() + objs[num] + b"\nendobj\n"
xref = len(buf)
buf += f"xref\n0 {maxnum + 1}\n".encode()
buf += b"0000000000 65535 f \n"
for num in range(1, maxnum + 1):
    if num in offsets:
        buf += f"{offsets[num]:010d} 00000 n \n".encode()
    else:
        buf += b"0000000000 65535 f \n"
buf += f"trailer\n<< /Size {maxnum + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
open(out + ".pdf", "wb").write(bytes(buf))
json.dump({"page_size_pt": [W, H],
           "placements": [{"name": n, "xobject": x, "cm": cm, "image_size": s, "gs": gs}
                          for n, x, cm, s, gs in placements]},
          open(out + ".json", "w"), indent=1)
print("wrote", out + ".pdf", len(buf), "bytes")
