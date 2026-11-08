"""P2 probe corpus: glyph-cache key quantization (B3) and worker font cache (B2).

Usage: python3 -I mkpdfs.py <outdir>

B3 (TrueType, Liberation Serif, upem 2048, embedded subset via reportlab TTFont):
  b3_pair_1200_1203.pdf    page0 12.00 pt, page1 12.03 pt, identical text
  b3_pair_1194_1203.pdf    page0 11.94 pt, page1 12.03 pt (both ends of one key bucket @2x)
  b3_ctrl_1200_1200.pdf    page0 12.00 pt, page1 12.00 pt (control)
  b3_ctrl_1200_1210.pdf    page0 12.00 pt, page1 12.10 pt (different bucket: control)
  b3_noise.pdf             page0 12 pt; page1 12 pt inside `1.0000003 0 0 1.0000003 0 0 cm`
  b3_noise_tm.pdf          page0 `12 0 0 12 x y Tm` + `1 Tf`; page1 `12.0000037 0 0 12.0000037 x y Tm`
  b3_multi16.pdf           16 pages alternating 12.00 / 12.03 pt (scheduler test)
B2 (embedded Type 1 via reportlab EmbeddedType1Face, FontFile):
  b2_t1_alt12.pdf          12 pages; even pages CharterBT-Roman only, odd pages Courier10PitchBT only
  b2_t1_same12.pdf         12 pages; all CharterBT-Roman (control: address reuse harmless)
  b2_ttf_alt12.pdf         12 pages; even Liberation Serif TTF, odd Liberation Mono TTF (control:
                           TrueType programs are not retained by the worker cache)
  b2_t1_poison.pdf         6 pages: Charter 12, Courier 12, Charter 13, Courier 13, Charter 13, Courier 13
  b2_t1_evict.pdf          12 pages: 0-10 Charter 12, 11 Courier 13
  b2_mixed_alt12.pdf       12 pages; even Charter (Type 1), odd Liberation Serif (TrueType)
"""
import os
import random
import sys

from reportlab.lib.pagesizes import letter
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas

FONT_DIR = "/usr/share/fonts/truetype/liberation"
T1_DIR = "/usr/share/fonts/X11/Type1"

WORDS = (
    "the of and to in is was that for it as with his he be on by at had not are but from or have "
    "an they which one you were her all she there would their we him been has when who will more no "
    "if out so said what up its about into than them can only other new some could time these two may "
    "then do first any my now such like our over man me even most made after also did many before must "
    "through back years where much your way well down should because each just those people how too "
    "little state good very make world still own see men work long get here between both life being under "
    "Quick Brown Fox Jumps Over Lazy Dog QUANTIZATION Glyph Cache Bucket 0123456789 ; : , . ! ? ( ) - "
).split()


def lines_of_text(seed, n_lines=46, width_chars=88):
    rng = random.Random(seed)
    out = []
    for _ in range(n_lines):
        line = []
        while sum(len(w) + 1 for w in line) < width_chars:
            line.append(rng.choice(WORDS))
        out.append(" ".join(line))
    return out


TEXT = lines_of_text(4242)


def text_page(c, font, size, leading=15.0, x=54.0, y=740.0, pre_cm=None, raw_tm=None):
    """One page of TEXT. `pre_cm` inserts a raw `cm` operator (exact digits).
    `raw_tm` = (sx, unit_size): write raw `sx 0 0 sx x y Tm /F unit_size Tf` lines."""
    if pre_cm is not None:
        c.saveState()
        c._code.append(pre_cm)
    if raw_tm is None:
        t = c.beginText(x, y)
        t.setFont(font, size, leading)
        for line in TEXT:
            t.textLine(line)
        c.drawText(t)
    else:
        # Text matrix carries the size: `S 0 0 S x y Tm` with `/F 1 Tf` per line
        # (reportlab prints S exactly when S is an integer; noise is patched in
        # afterwards with pypdf, see patch_tm()).
        t = c.beginText(x, y)
        t.setFont(font, 1, leading)
        yy = y
        for line in TEXT:
            t.setTextTransform(raw_tm, 0, 0, raw_tm, x, yy)
            t.textOut(line)
            yy -= leading
        c.drawText(t)
    if pre_cm is not None:
        c.restoreState()


def register_fonts():
    pdfmetrics.registerFont(TTFont("LSerif", os.path.join(FONT_DIR, "LiberationSerif-Regular.ttf")))
    pdfmetrics.registerFont(TTFont("LMono", os.path.join(FONT_DIR, "LiberationMono-Regular.ttf")))
    t1 = {}
    for stem in ("c0648bt_", "c0419bt_"):
        face = pdfmetrics.EmbeddedType1Face(os.path.join(T1_DIR, stem + ".afm"), os.path.join(T1_DIR, stem + ".pfb"))
        pdfmetrics.registerTypeFace(face)
        pdfmetrics.registerFont(pdfmetrics.Font(face.name, face.name, "WinAnsiEncoding"))
        t1[stem] = face.name
    return t1["c0648bt_"], t1["c0419bt_"]


def doc(path, pages, compress=1):
    c = canvas.Canvas(path, pagesize=letter, pageCompression=compress, invariant=1)
    for spec in pages:
        text_page(c, **spec)
        c.showPage()
    c.save()


def patch_tm(src, dst, page, old, new):
    import pypdf
    from pypdf.generic import NameObject, StreamObject
    r = pypdf.PdfReader(src)
    w = pypdf.PdfWriter(clone_from=r)
    pg = w.pages[page]
    data = pg.get_contents().get_data()
    n = data.count(old)
    assert n > 0, "pattern not found"
    st = StreamObject()
    st.set_data(data.replace(old, new))
    pg[NameObject("/Contents")] = w._add_object(st)
    with open(dst, "wb") as f:
        w.write(f)
    print(f"patched {n} Tm operators on page {page} of {dst}")


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    charter, courier = register_fonts()
    j = lambda n: os.path.join(out, n)
    doc(j("b3_pair_1200_1203.pdf"), [dict(font="LSerif", size=12.00), dict(font="LSerif", size=12.03)])
    doc(j("b3_pair_1194_1203.pdf"), [dict(font="LSerif", size=11.94), dict(font="LSerif", size=12.03)])
    doc(j("b3_ctrl_1200_1200.pdf"), [dict(font="LSerif", size=12.00), dict(font="LSerif", size=12.00)])
    doc(j("b3_ctrl_1200_1210.pdf"), [dict(font="LSerif", size=12.00), dict(font="LSerif", size=12.10)])
    doc(j("b3_noise.pdf"), [dict(font="LSerif", size=12.0),
                            dict(font="LSerif", size=12.0, pre_cm="1.0000003 0 0 1.0000003 0 0 cm")],
        compress=0)
    doc(j("b3_noise_tm_base.pdf"), [dict(font="LSerif", size=12.0, raw_tm=12),
                                    dict(font="LSerif", size=12.0, raw_tm=12)], compress=0)
    patch_tm(j("b3_noise_tm_base.pdf"), j("b3_noise_tm.pdf"), page=1,
             old=b"12 0 0 12 ", new=b"12.0000037 0 0 12.0000037 ")
    doc(j("b3_multi16.pdf"), [dict(font="LSerif", size=(12.00 if i % 2 == 0 else 12.03)) for i in range(16)])
    doc(j("b2_t1_alt12.pdf"), [dict(font=(charter if i % 2 == 0 else courier), size=12.0) for i in range(12)])
    doc(j("b2_t1_same12.pdf"), [dict(font=charter, size=12.0) for i in range(12)])
    doc(j("b2_ttf_alt12.pdf"), [dict(font=("LSerif" if i % 2 == 0 else "LMono"), size=12.0) for i in range(12)])
    # p0 Charter 12, p1 Courier 12, p2 Charter 13, p3 Courier 13, p4 Charter 13, p5 Courier 13
    doc(j("b2_t1_poison.pdf"), [dict(font=charter, size=12.0), dict(font=courier, size=12.0),
                                dict(font=charter, size=13.0), dict(font=courier, size=13.0),
                                dict(font=charter, size=13.0), dict(font=courier, size=13.0)])
    # pages 0-10 Charter 12, page 11 Courier 13 (page-cache eviction scenarios)
    doc(j("b2_t1_evict.pdf"), [dict(font=charter, size=12.0) for _ in range(11)] + [dict(font=courier, size=13.0)])
    # even pages Charter (Type 1), odd pages Liberation Serif (TrueType)
    doc(j("b2_mixed_alt12.pdf"), [dict(font=(charter if i % 2 == 0 else "LSerif"), size=12.0) for i in range(12)])
    print("charter", charter, "courier", courier)


if __name__ == "__main__":
    main()
