"""groups.pdf: a transparency-group-heavy page for the B14 speed A/B. Stdlib only.

usage: python -I mkgroups.py <outdir>

Page 1: 400 small isolated transparency groups (40x40 pt BBox) laid out on a
grid; each group's content is a filled rectangle and a diagonal stroke that run
from inside the BBox to the right page edge, so the group's clipped content
crosses the offscreen's right boundary (the B14 case).
Page 2: the same grid, each cell under a luminosity soft mask whose content
also runs past the mask BBox.
Page 3: 2000 tiny groups (8x8 pt) with a short path each, the "small group far
from the right edge" cost case.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mkstencil import Pdf  # noqa: E402

W, H = 612, 792


def grid_groups(pdf, cell, step, count_x, count_y, long_run=True):
    """Return (form xobject ids, content) for a grid of group forms."""
    forms = {}
    content = []
    n = 0
    for j in range(count_y):
        for i in range(count_x):
            x = 30 + i * step
            y = H - 60 - j * step
            if x + cell > W - 20 or y < 20:
                continue
            right = W + 10 if long_run else x + cell + 2
            body = (
                "0.9 0.2 0.1 rg %d %d %d %d re f "
                "0 0 0.8 RG 2 w %d %d m %d %d l S"
                % (x + 4, y + 4, right - x, cell - 8, x + 2, y + 2, right, y + cell - 2)
            )
            fid = pdf.stream(
                "/Type /XObject /Subtype /Form /BBox [%d %d %d %d] "
                "/Group << /S /Transparency /CS /DeviceRGB /I true >>"
                % (x, y, x + cell, y + cell),
                body.encode(),
            )
            name = "F%d" % n
            forms[name] = fid
            content.append("q /%s Do Q" % name)
            n += 1
    return forms, " ".join(content)


pdf = Pdf()
out = sys.argv[1]
os.makedirs(out, exist_ok=True)

f1, c1 = grid_groups(pdf, 40, 27, 20, 26)
page1 = pdf.page("0.95 0.95 0.95 rg 0 0 612 792 re f " + c1, f1, size=(W, H))

# Page 2: soft masks. One luminosity mask form per cell; its content runs right.
forms2 = {}
gs2 = {}
content2 = []
n = 0
for j in range(26):
    for i in range(20):
        x = 30 + i * 27
        y = H - 60 - j * 27
        if x + 40 > W - 20 or y < 20:
            continue
        mask_form = pdf.stream(
            "/Type /XObject /Subtype /Form /BBox [%d %d %d %d] "
            "/Group << /S /Transparency /CS /DeviceGray >>" % (x, y, x + 40, y + 40),
            ("1 g %d %d %d 32 re f 0.3 g %d %d m %d %d l 6 w S"
             % (x + 4, y + 4, W + 10 - x, x, y, W + 10, y + 40)).encode(),
        )
        gid = pdf.add(
            ("<< /Type /ExtGState /SMask << /S /Luminosity /G %d 0 R /BC [0] >> >>" % mask_form).encode()
        )
        gname = "G%d" % n
        gs2[gname] = gid
        content2.append("q /%s gs 0.1 0.4 0.9 rg %d %d 40 40 re f Q" % (gname, x, y))
        n += 1
page2 = pdf.page(
    "0.95 0.95 0.95 rg 0 0 612 792 re f " + " ".join(content2),
    {},
    size=(W, H),
    extgstates=gs2,
)

f3, c3 = grid_groups(pdf, 8, 11, 50, 60, long_run=False)
page3 = pdf.page("1 1 1 rg 0 0 612 792 re f " + c3, f3, size=(W, H))

pdf.write(os.path.join(out, "groups.pdf"), [page1, page2, page3])
print("wrote", os.path.join(out, "groups.pdf"))
