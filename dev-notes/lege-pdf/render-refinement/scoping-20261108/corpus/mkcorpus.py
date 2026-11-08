"""Synthesize a perf corpus for the Lege PDF renderer.

Usage: python3 -I mkcorpus.py <outdir> [class ...]
classes: text vector scans bilevel transp mrc jpx all
"""
import io
import math
import os
import random
import struct
import sys
import zlib

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont, TiffImagePlugin

FONT_DIR = "/usr/share/fonts/truetype/liberation"
T1_DIR = "/usr/share/fonts/X11/Type1"

WORDS = (
    "the of and to in is was that for it as with his he be on by at had not are but from or have "
    "an they which one you were her all she there would their we him been has when who will more no "
    "if out so said what up its about into than them can only other new some could time these two may "
    "then do first any my now such like our over man me even most made after also did many before must "
    "through back years where much your way well down should because each just those people how too "
    "little state good very make world still own see men work long get here between both life being under "
    "never day same another know while last might us great old year off come since against go came right "
    "used take three states himself few house use during without again place american around however home "
    "small found thought went say part once general high upon school every don does got united left number "
    "course war until always away something fact though water less public put think almost hand enough far "
    "took head yet government system better set told nothing night end why called didn eyes find going look "
    "asked later knew point next program city business give group toward young days let room president side "
    "social given present several order national possible rather second face per among form important often "
    "things looked early white case john become large big need four within felt along children saw best church "
    "ever least power development light thing seemed family interest want members mind country area others "
    "although turned done open god service certain kind problem began different door thus help sense means whole "
    "matter perhaps itself york times law human line above name example action company local show whether five "
    "history gave today either act feet across taken past quite anything seen having death experience body word "
    "half really field am car words already themselves information tell together college shall money period held "
    "keep sure probably free seems real behind cannot miss political air question making office brought whose special "
    "major heard problems ago became federal study available known result street economic boy position reason change "
    "south board individual job areas society west close turn love community true court force full seem am"
).split()


def sentence(rng):
    n = rng.randint(6, 22)
    ws = [rng.choice(WORDS) for _ in range(n)]
    ws[0] = ws[0].capitalize()
    out = []
    for i, w in enumerate(ws):
        out.append(w)
        if i < n - 1 and rng.random() < 0.08:
            out[-1] += ","
    return " ".join(out) + rng.choice([".", ".", ".", ".", "?", ";"])


def paragraph(rng, nsent=None):
    return " ".join(sentence(rng) for _ in range(nsent or rng.randint(3, 8)))


# ---------------------------------------------------------------------------
# Minimal raw PDF writer
# ---------------------------------------------------------------------------
class RawPdf:
    def __init__(self):
        self.objs = []

    def reserve(self):
        self.objs.append(None)
        return len(self.objs)

    def add(self, body):
        self.objs.append(body if isinstance(body, bytes) else body.encode("latin-1"))
        return len(self.objs)

    def set(self, oid, body):
        self.objs[oid - 1] = body if isinstance(body, bytes) else body.encode("latin-1")

    def stream(self, entries, data, oid=None):
        body = b"<< " + entries.encode("latin-1") + b" /Length %d >>\nstream\n" % len(data) + data + b"\nendstream"
        if oid is None:
            return self.add(body)
        self.set(oid, body)
        return oid

    def flate(self, entries, data, oid=None):
        return self.stream(entries + " /Filter /FlateDecode", zlib.compress(data, 6), oid)

    def write(self, path, root):
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets = []
        for i, body in enumerate(self.objs):
            offsets.append(len(out))
            out += b"%d 0 obj\n" % (i + 1) + body + b"\nendobj\n"
        xref = len(out)
        out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(self.objs) + 1)
        for off in offsets:
            out += b"%010d 00000 n \n" % off
        out += b"trailer\n<< /Size %d /Root %d 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(self.objs) + 1, root, xref)
        with open(path, "wb") as f:
            f.write(out)


def raw_doc(pages_fn, path, media=(612, 792)):
    """pages_fn(pdf) -> list of (content_bytes, resources_str)."""
    pdf = RawPdf()
    catalog = pdf.reserve()
    pages = pdf.reserve()
    kids = []
    for content, resources in pages_fn(pdf):
        cid = pdf.flate("", content)
        pid = pdf.add(
            "<< /Type /Page /Parent %d 0 R /MediaBox [0 0 %d %d] /Resources %s /Contents %d 0 R >>"
            % (pages, media[0], media[1], resources, cid)
        )
        kids.append(pid)
    pdf.set(pages, "<< /Type /Pages /Kids [%s] /Count %d >>" % (" ".join("%d 0 R" % k for k in kids), len(kids)))
    pdf.set(catalog, "<< /Type /Catalog /Pages %d 0 R >>" % pages)
    pdf.write(path, catalog)


# ---------------------------------------------------------------------------
# (a) text: embedded TrueType, standard 14, embedded Type 1
# ---------------------------------------------------------------------------
def make_text(outdir, npages=24):
    from reportlab.lib.enums import TA_JUSTIFY
    from reportlab.lib.pagesizes import letter
    from reportlab.lib.styles import ParagraphStyle
    from reportlab.pdfbase import pdfmetrics
    from reportlab.pdfbase.ttfonts import TTFont
    from reportlab.lib.fonts import addMapping
    from reportlab.platypus import Paragraph, SimpleDocTemplate, Spacer, PageBreak

    pdfmetrics.registerFont(TTFont("LSerif", os.path.join(FONT_DIR, "LiberationSerif-Regular.ttf")))
    pdfmetrics.registerFont(TTFont("LSerif-I", os.path.join(FONT_DIR, "LiberationSerif-Italic.ttf")))
    pdfmetrics.registerFont(TTFont("LSerif-B", os.path.join(FONT_DIR, "LiberationSerif-Bold.ttf")))
    pdfmetrics.registerFont(TTFont("LSerif-BI", os.path.join(FONT_DIR, "LiberationSerif-BoldItalic.ttf")))
    pdfmetrics.registerFont(TTFont("LSans-B", os.path.join(FONT_DIR, "LiberationSans-Bold.ttf")))
    from reportlab.pdfbase.pdfmetrics import registerFontFamily
    registerFontFamily("LSerif", normal="LSerif", bold="LSerif-B", italic="LSerif-I", boldItalic="LSerif-BI")

    # Embedded Type 1 (Bitstream Charter), as LaTeX-ish documents carry.
    t1 = {}
    for fn in sorted(os.listdir(T1_DIR)):
        if fn.endswith(".afm"):
            with open(os.path.join(T1_DIR, fn), "rb") as f:
                for line in f.read().decode("latin-1").splitlines():
                    if line.startswith("FontName "):
                        t1[line.split()[1]] = fn[:-4]
    t1_names = {}
    for role, fontname in (("n", "CharterBT-Roman"), ("i", "CharterBT-Italic"), ("b", "CharterBT-Bold"), ("bi", "CharterBT-BoldItalic")):
        stem = t1.get(fontname)
        if stem is None:
            continue
        face = pdfmetrics.EmbeddedType1Face(os.path.join(T1_DIR, stem + ".afm"), os.path.join(T1_DIR, stem + ".pfb"))
        pdfmetrics.registerTypeFace(face)
        pdfmetrics.registerFont(pdfmetrics.Font(fontname, face.name, "WinAnsiEncoding"))
        t1_names[role] = fontname
    if len(t1_names) == 4:
        registerFontFamily("CharterBT-Roman", normal=t1_names["n"], bold=t1_names["b"], italic=t1_names["i"], boldItalic=t1_names["bi"])

    variants = [
        ("text_ttf.pdf", "LSerif", "LSerif-I", "LSans-B"),
        ("text_std14.pdf", "Times-Roman", "Times-Italic", "Helvetica-Bold"),
    ]
    if len(t1_names) == 4:
        variants.append(("text_type1.pdf", t1_names["n"], t1_names["i"], t1_names["b"]))

    for fname, body_font, ital_font, head_font in variants:
        rng = random.Random(1234)
        body = ParagraphStyle("body", fontName=body_font, fontSize=12, leading=15, alignment=TA_JUSTIFY,
                              firstLineIndent=14, spaceAfter=0)
        head = ParagraphStyle("head", fontName=head_font, fontSize=15, leading=19, spaceBefore=6, spaceAfter=10)
        story = []
        chapter = 1
        while len(story) < npages * 9:
            story.append(Paragraph("Chapter %d. %s" % (chapter, sentence(rng)[:-1].title()), head))
            for _ in range(rng.randint(9, 16)):
                txt = paragraph(rng)
                # sprinkle italics / bold like a real book
                words = txt.split(" ")
                for k in range(0, len(words), rng.randint(15, 40)):
                    words[k] = "<i>%s</i>" % words[k] if rng.random() < 0.7 else "<b>%s</b>" % words[k]
                story.append(Paragraph(" ".join(words), body))
            chapter += 1

        def on_page(canv, doc, ital_font=ital_font):
            canv.saveState()
            canv.setFont(ital_font, 9)
            canv.drawCentredString(306, 750, "A Synthetic Treatise on Rendering Performance")
            canv.drawCentredString(306, 40, str(doc.page))
            canv.setLineWidth(0.4)
            canv.line(72, 744, 540, 744)
            canv.restoreState()

        path = os.path.join(outdir, fname)
        doc = SimpleDocTemplate(path, pagesize=letter, leftMargin=72, rightMargin=72, topMargin=72, bottomMargin=60,
                                title=fname, author="probe")
        # Build, then truncate to npages by building only enough story.
        doc.build(story, onFirstPage=on_page, onLaterPages=on_page)
        print("wrote", path)


# ---------------------------------------------------------------------------
# (b) vector-heavy pages
# ---------------------------------------------------------------------------
def make_vector(outdir, npages=10):
    from reportlab.lib.pagesizes import letter
    from reportlab.pdfgen import canvas

    path = os.path.join(outdir, "vector.pdf")
    c = canvas.Canvas(path, pagesize=letter, pageCompression=1)
    W, H = letter
    for pg in range(npages):
        rng = random.Random(100 + pg)
        kind = pg % 5
        if kind == 0:
            # "map": random-walk polylines + even-odd filled polygons + dashed contours
            for _ in range(250):
                c.setFillColorRGB(rng.random(), rng.random(), rng.random(), alpha=None)
                p = c.beginPath()
                cx, cy = rng.uniform(0, W), rng.uniform(0, H)
                p.moveTo(cx, cy)
                for _ in range(rng.randint(5, 14)):
                    p.lineTo(cx + rng.uniform(-40, 40), cy + rng.uniform(-40, 40))
                p.close()
                c.drawPath(p, stroke=0, fill=1, fillMode=1)
            c.setLineJoin(1)
            c.setLineCap(1)
            for i in range(350):
                c.setStrokeColorRGB(rng.random() * 0.5, rng.random() * 0.5, rng.random() * 0.8)
                c.setLineWidth(rng.choice([0.25, 0.5, 0.75, 1.0, 2.0]))
                if i % 4 == 0:
                    c.setDash([3, 2, 1, 2], 0)
                else:
                    c.setDash([])
                p = c.beginPath()
                x, y = rng.uniform(0, W), rng.uniform(0, H)
                p.moveTo(x, y)
                for _ in range(rng.randint(20, 60)):
                    x += rng.uniform(-8, 8)
                    y += rng.uniform(-8, 8)
                    p.lineTo(x, y)
                c.drawPath(p, stroke=1, fill=0)
        elif kind == 1:
            # "chart": hairline grid, bezier series, 2000-point scatter of small circles
            c.setDash([])
            c.setLineWidth(0)
            c.setStrokeColorRGB(0.7, 0.7, 0.7)
            for x in range(36, int(W) - 36, 9):
                c.line(x, 36, x, H - 36)
            for y in range(36, int(H) - 36, 9):
                c.line(36, y, W - 36, y)
            for s in range(24):
                c.setStrokeColorRGB(rng.random(), rng.random(), rng.random())
                c.setLineWidth(rng.choice([0.5, 1, 1.5]))
                p = c.beginPath()
                y = rng.uniform(100, H - 100)
                p.moveTo(36, y)
                x = 36
                while x < W - 60:
                    p.curveTo(x + 8, y + rng.uniform(-30, 30), x + 16, y + rng.uniform(-30, 30), x + 24, y + rng.uniform(-20, 20))
                    x += 24
                c.drawPath(p, stroke=1, fill=0)
            for _ in range(2000):
                c.setFillColorRGB(rng.random(), 0.3, 0.6)
                c.circle(rng.uniform(40, W - 40), rng.uniform(40, H - 40), rng.uniform(0.8, 2.5), stroke=0, fill=1)
        elif kind == 2:
            # "CAD": rectangles, arcs, dashed centre lines, sharp miter joins, hatching
            c.setLineJoin(0)
            c.setMiterLimit(10)
            for _ in range(300):
                c.setStrokeColorRGB(0, 0, 0)
                c.setLineWidth(rng.choice([0.3, 0.6, 1.2]))
                c.setDash([])
                x, y = rng.uniform(20, W - 120), rng.uniform(20, H - 120)
                w, h = rng.uniform(5, 100), rng.uniform(5, 100)
                c.rect(x, y, w, h, stroke=1, fill=0)
                c.setDash([12, 3, 2, 3], 0)
                c.setLineWidth(0.25)
                c.line(x - 5, y + h / 2, x + w + 5, y + h / 2)
                c.setDash([])
                c.arc(x, y, x + w, y + h, rng.uniform(0, 180), rng.uniform(30, 270))
            # zig-zag with sharp angles
            for _ in range(80):
                c.setLineWidth(rng.uniform(1, 4))
                p = c.beginPath()
                x, y = rng.uniform(0, W), rng.uniform(0, H)
                p.moveTo(x, y)
                for k in range(12):
                    p.lineTo(x + 6 * k, y + (8 if k % 2 else -8))
                c.drawPath(p, stroke=1, fill=0)
            # hatch: 5000 short separate segments
            c.setLineWidth(0.2)
            for _ in range(5000):
                x, y = rng.uniform(0, W), rng.uniform(0, H)
                c.line(x, y, x + 4, y + 4)
        elif kind == 3:
            # clipping: shapes clipped by circles / stars, nested, even-odd clip
            for _ in range(40):
                c.saveState()
                cx, cy, r = rng.uniform(60, W - 60), rng.uniform(60, H - 60), rng.uniform(20, 70)
                p = c.beginPath()
                if rng.random() < 0.5:
                    p.circle(cx, cy, r)
                else:
                    for k in range(10):
                        a = math.pi / 2 + k * math.pi / 5
                        rr = r if k % 2 == 0 else r * 0.45
                        (p.moveTo if k == 0 else p.lineTo)(cx + rr * math.cos(a), cy + rr * math.sin(a))
                    p.close()
                c.clipPath(p, stroke=0, fill=0, fillMode=rng.choice([0, 1]))
                if rng.random() < 0.5:
                    p2 = c.beginPath()
                    p2.rect(cx - r * 0.7, cy - r * 0.7, r * 1.4, r * 1.0)
                    c.clipPath(p2, stroke=0, fill=0)
                for _ in range(60):
                    c.setStrokeColorRGB(rng.random(), rng.random(), rng.random())
                    c.setLineWidth(rng.uniform(0.5, 3))
                    c.line(cx - r, cy + rng.uniform(-r, r), cx + r, cy + rng.uniform(-r, r))
                c.setFillColorRGB(rng.random(), rng.random(), rng.random())
                c.circle(cx + r / 3, cy - r / 3, r / 2, stroke=0, fill=1)
                c.restoreState()
            # an axial + radial gradient (shadings)
            c.saveState()
            p = c.beginPath()
            p.rect(36, 36, 200, 120)
            c.clipPath(p, stroke=0, fill=0)
            c.linearGradient(36, 36, 236, 156, ((1, 0, 0), (0, 0, 1)), extend=False)
            c.restoreState()
            c.saveState()
            p = c.beginPath()
            p.circle(450, 120, 80)
            c.clipPath(p, stroke=0, fill=0)
            c.radialGradient(450, 120, 80, ((1, 1, 0), (0, 0.5, 0)), extend=False)
            c.restoreState()
        else:
            # strokes under rotated / non-uniform CTMs, all caps and joins, dash phases
            for i in range(400):
                c.saveState()
                c.translate(rng.uniform(0, W), rng.uniform(0, H))
                c.rotate(rng.uniform(0, 360))
                c.scale(rng.uniform(0.3, 2.0), rng.uniform(0.3, 2.0))
                c.setLineCap(i % 3)
                c.setLineJoin((i // 3) % 3)
                c.setLineWidth(rng.uniform(0.5, 6))
                if i % 2:
                    c.setDash([rng.uniform(1, 6), rng.uniform(1, 4)], rng.uniform(0, 3))
                c.setStrokeColorRGB(rng.random(), rng.random(), rng.random())
                p = c.beginPath()
                p.moveTo(0, 0)
                p.curveTo(20, 30, 40, -30, 60, 0)
                p.lineTo(60, 20)
                p.lineTo(30, 10)
                c.drawPath(p, stroke=1, fill=0)
                c.restoreState()
        c.showPage()
    c.save()
    print("wrote", path)


# ---------------------------------------------------------------------------
# Scan page synthesis
# ---------------------------------------------------------------------------
SCAN_W, SCAN_H = 2550, 3300  # letter @ 300 DPI


def scan_text_layer(rng, w=SCAN_W, h=SCAN_H, figure=False):
    """Return an L image (255 paper, 0 ink) of a page of text at 300 DPI."""
    img = Image.new("L", (w, h), 255)
    d = ImageDraw.Draw(img)
    font = ImageFont.truetype(os.path.join(FONT_DIR, "LiberationSerif-Regular.ttf"), 44)
    bold = ImageFont.truetype(os.path.join(FONT_DIR, "LiberationSerif-Bold.ttf"), 64)
    x0, y = 300, 300
    d.text((x0, y), sentence(rng)[:40], font=bold, fill=0)
    y += 120
    fig_box = None
    if figure:
        fig_box = (x0, 1300, w - 300, 2000)
    while y < h - 330:
        if fig_box and fig_box[1] - 60 <= y <= fig_box[3]:
            y = fig_box[3] + 60
            continue
        line = ""
        while d.textlength(line, font=font) < (w - 2 * x0 - 200):
            line += rng.choice(WORDS) + " "
        d.text((x0, y), line.strip(), font=font, fill=0)
        y += 58
    return img, fig_box


def scanify_gray(text_img, rng, seed):
    nrng = np.random.default_rng(seed)
    ink = np.asarray(text_img, dtype=np.float32) / 255.0  # 1 paper, 0 ink
    h, w = ink.shape
    # paper with low-frequency variation
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float32)
    paper = 232 + 8 * np.sin(xx / 400.0 + seed) * np.cos(yy / 530.0)
    val = paper * ink + 25 * (1 - ink)
    # scanner shadow on the binding edge
    val *= np.clip(0.75 + xx / 600.0, 0, 1)
    val += nrng.normal(0, 6, size=val.shape)
    img = Image.fromarray(np.clip(val, 0, 255).astype(np.uint8), "L")
    img = img.rotate(rng.uniform(-0.6, 0.6), resample=Image.BILINEAR, fillcolor=230)
    return img.filter(ImageFilter.GaussianBlur(0.7))


def scanify_color(text_img, rng, seed, fig_box):
    nrng = np.random.default_rng(seed)
    ink = np.asarray(text_img, dtype=np.float32) / 255.0
    h, w = ink.shape
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float32)
    tint = np.array([241, 233, 214], dtype=np.float32)
    var = (6 * np.sin(xx / 350.0 + seed) * np.cos(yy / 470.0))[..., None]
    paper = tint[None, None, :] + var
    inkc = np.array([40, 30, 25], dtype=np.float32)
    val = paper * ink[..., None] + inkc[None, None, :] * (1 - ink[..., None])
    if fig_box:
        x0, y0, x1, y1 = fig_box
        fy, fx = np.mgrid[y0:y1, x0:x1].astype(np.float32)
        fig = np.stack([
            128 + 100 * np.sin(fx / 90.0) * np.cos(fy / 70.0),
            128 + 90 * np.cos(fx / 130.0 + 1.0),
            128 + 110 * np.sin((fx + fy) / 160.0),
        ], axis=-1)
        val[y0:y1, x0:x1] = fig
    val += nrng.normal(0, 5, size=val.shape)
    img = Image.fromarray(np.clip(val, 0, 255).astype(np.uint8), "RGB")
    img = img.rotate(rng.uniform(-0.6, 0.6), resample=Image.BILINEAR, fillcolor=(236, 228, 210))
    return img.filter(ImageFilter.GaussianBlur(0.7))


def jpeg_bytes(img, quality, subsampling=None):
    b = io.BytesIO()
    kw = {"quality": quality, "optimize": False}
    if subsampling is not None:
        kw["subsampling"] = subsampling
    img.save(b, "JPEG", **kw)
    return b.getvalue()


def g4_bytes(img1):
    """CCITT G4 strip of a mode-'1' image with fax polarity: ink coded as black
    runs, so the stream decodes with /BlackIs1 false to ink = 0."""
    inv = img1.convert("L").point(lambda v: 255 - v).convert("1", dither=Image.Dither.NONE)
    b = io.BytesIO()
    w, h = inv.size
    inv.save(b, "TIFF", compression="group4", strip_size=math.ceil(w / 8) * h)
    data = b.getvalue()
    t = Image.open(io.BytesIO(data))
    offs = t.tag_v2[273]
    counts = t.tag_v2[279]
    assert len(offs) == 1, "expected one strip"
    assert t.tag_v2.get(262) == 1
    return data[offs[0]:offs[0] + counts[0]]


def image_dict(w, h, cs, bpc, filt, extra=""):
    return "/Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace %s /BitsPerComponent %d /Filter %s %s" % (
        w, h, cs, bpc, filt, extra)


def make_scans(outdir, npages=10):
    for color in (False, True):
        fname = "jpeg_color_scan.pdf" if color else "jpeg_gray_scan.pdf"

        def pages(pdf, color=color):
            for pg in range(npages):
                rng = random.Random(500 + pg + (100 if color else 0))
                text, fig = scan_text_layer(rng, figure=color and pg % 2 == 0)
                if color:
                    img = scanify_color(text, rng, pg, fig)
                    data = jpeg_bytes(img, 75, subsampling=2)  # 4:2:0
                    cs = "/DeviceRGB"
                else:
                    img = scanify_gray(text, rng, pg)
                    data = jpeg_bytes(img, 80)
                    cs = "/DeviceGray"
                iid = pdf.stream(image_dict(SCAN_W, SCAN_H, cs, 8, "/DCTDecode"), data)
                yield b"q 612 0 0 792 0 0 cm /Im0 Do Q\n", "<< /XObject << /Im0 %d 0 R >> >>" % iid

        raw_doc(pages, os.path.join(outdir, fname))
        print("wrote", fname)


def bilevel_page(pg):
    rng = random.Random(900 + pg)
    text, _ = scan_text_layer(rng)
    g = scanify_gray(text, rng, 900 + pg)
    return g.point(lambda v: 255 if v > 135 else 0).convert("1", dither=Image.Dither.NONE)


def make_bilevel(outdir, npages=10):
    imgs = [bilevel_page(pg) for pg in range(npages)]
    path = os.path.join(outdir, "bilevel_g4.pdf")
    imgs[0].save(path, "PDF", resolution=300, save_all=True, append_images=imgs[1:])
    print("wrote", path)
    # What does reportlab do with a mode-1 image? (filter census only)
    from reportlab.lib.pagesizes import letter
    from reportlab.lib.utils import ImageReader
    from reportlab.pdfgen import canvas

    rpath = os.path.join(outdir, "bilevel_reportlab.pdf")
    c = canvas.Canvas(rpath, pagesize=letter)
    for im in imgs[:2]:
        c.drawImage(ImageReader(im), 0, 0, 612, 792)
        c.showPage()
    c.save()
    print("wrote", rpath)
    # 1-bit page images for the JBIG2 encoder (PBM, 1 = black)
    pbmdir = os.path.join(outdir, "pbm")
    os.makedirs(pbmdir, exist_ok=True)
    for i, im in enumerate(imgs):
        im.save(os.path.join(pbmdir, "page-%02d.pbm" % i))


# ---------------------------------------------------------------------------
# (e) transparency
# ---------------------------------------------------------------------------
def circle_path(cx, cy, r):
    k = 0.5523 * r
    return ("%.2f %.2f m %.2f %.2f %.2f %.2f %.2f %.2f c %.2f %.2f %.2f %.2f %.2f %.2f c "
            "%.2f %.2f %.2f %.2f %.2f %.2f c %.2f %.2f %.2f %.2f %.2f %.2f c h\n") % (
        cx + r, cy, cx + r, cy + k, cx + k, cy + r, cx, cy + r,
        cx - k, cy + r, cx - r, cy + k, cx - r, cy, cx - r, cy - k, cx - k, cy - r, cx, cy - r,
        cx + k, cy - r, cx + r, cy - k, cx + r, cy)


def make_transp(outdir, npages=6):
    def pages(pdf):
        # shared resources
        gs = {}
        for name, d in (("A50", "/ca 0.5 /CA 0.5"), ("A30", "/ca 0.3"), ("A75", "/ca 0.75 /CA 0.75"),
                        ("Mul", "/BM /Multiply /ca 0.9"), ("Scr", "/BM /Screen"), ("Ovl", "/BM /Overlay /ca 0.8"),
                        ("Lum", "/BM /Luminosity"), ("Diff", "/BM /Difference")):
            gs[name] = pdf.add("<< /Type /ExtGState %s >>" % d)
        # RGBA photo-like image with smooth alpha (JPEG colour + Flate SMask)
        w, h = 1200, 900
        yy, xx = np.mgrid[0:h, 0:w].astype(np.float32)
        rgb = np.stack([128 + 120 * np.sin(xx / 70.0), 128 + 120 * np.cos(yy / 50.0), 128 + 100 * np.sin((xx + yy) / 110.0)], -1)
        rgb += np.random.default_rng(3).normal(0, 8, rgb.shape)
        alpha = np.clip(255 * (1 - np.hypot(xx - w / 2, yy - h / 2) / (0.55 * w)), 0, 255)
        smask = pdf.flate("/Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray /BitsPerComponent 8" % (w, h),
                          alpha.astype(np.uint8).tobytes())
        jpg = jpeg_bytes(Image.fromarray(np.clip(rgb, 0, 255).astype(np.uint8), "RGB"), 85)
        img_rgba = pdf.stream(image_dict(w, h, "/DeviceRGB", 8, "/DCTDecode", "/SMask %d 0 R" % smask), jpg)
        # Flate RGBA (PNG-like) icon-size image with hard alpha
        w2, h2 = 300, 300
        yy2, xx2 = np.mgrid[0:h2, 0:w2].astype(np.float32)
        rgb2 = np.stack([xx2 * 0.8, yy2 * 0.8, 255 - xx2 * 0.5], -1)
        a2 = ((np.hypot(xx2 - 150, yy2 - 150) < 140) * 255).astype(np.uint8)
        sm2 = pdf.flate("/Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray /BitsPerComponent 8" % (w2, h2), a2.tobytes())
        img2 = pdf.flate(image_dict(w2, h2, "/DeviceRGB", 8, "", "/SMask %d 0 R" % sm2).replace("/Filter  ", ""),
                         np.clip(rgb2, 0, 255).astype(np.uint8).tobytes())
        # luminosity soft-mask group: radial shading painted into a group
        fn = pdf.add("<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>")
        sh = pdf.add("<< /ShadingType 3 /ColorSpace /DeviceGray /Coords [306 396 0 306 396 300] /Function %d 0 R /Extend [true true] >>" % fn)
        mask_form = pdf.flate("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency /CS /DeviceGray >> /Resources << /Shading << /Sh0 %d 0 R >> >>" % sh,
                              b"/Sh0 sh\n")
        gs["SM"] = pdf.add("<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G %d 0 R >> >>" % mask_form)
        # alpha soft mask from a group containing a semi-transparent stack of discs
        disc = "".join(circle_path(150 + 60 * k, 600, 70) + "f\n" for k in range(6)).encode()
        amask_form = pdf.flate("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency >> /Resources << /ExtGState << /A50 %d 0 R >> >>" % gs["A50"],
                               b"/A50 gs 0 0 0 rg " + disc)
        gs["SMa"] = pdf.add("<< /Type /ExtGState /SMask << /Type /Mask /S /Alpha /G %d 0 R >> >>" % amask_form)
        # isolated knockout and non-isolated groups with content
        grp_content = b"".join((("%.2f %.2f %.2f rg " % (k / 8, 0.4, 1 - k / 8)) + circle_path(200 + 25 * k, 300, 60) + "f\n").encode() for k in range(8))
        g_iso = pdf.flate("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency /I true /K true >> /Resources << /ExtGState << /A50 %d 0 R >> >>" % gs["A50"],
                          b"/A50 gs " + grp_content)
        g_non = pdf.flate("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency /I false /K false >> /Resources << /ExtGState << /Mul %d 0 R >> >>" % gs["Mul"],
                          b"/Mul gs " + grp_content)
        res = "<< /ExtGState << %s >> /XObject << /Im0 %d 0 R /Im1 %d 0 R /G0 %d 0 R /G1 %d 0 R >> >>" % (
            " ".join("/%s %d 0 R" % (k, v) for k, v in gs.items()), img_rgba, img2, g_iso, g_non)
        for pg in range(npages):
            rng = random.Random(70 + pg)
            ops = []
            # opaque backdrop: stripes so blending is visible
            for k in range(0, 792, 24):
                ops.append("%.2f %.2f %.2f rg 0 %d 612 12 re f\n" % (rng.random(), rng.random(), rng.random(), k))
            if pg % 3 == 0:
                ops.append("q /SM gs q 600 0 0 450 6 300 cm /Im0 Do Q Q\n")  # luminosity smask over RGBA image
                for k in range(40):
                    ops.append("q /%s gs %.2f %.2f %.2f rg %s f Q\n" % (rng.choice(["A50", "A30", "A75", "Mul", "Scr", "Ovl", "Diff"]),
                               rng.random(), rng.random(), rng.random(), circle_path(rng.uniform(50, 560), rng.uniform(50, 740), rng.uniform(20, 90))))
            elif pg % 3 == 1:
                ops.append("q 560 0 0 420 26 340 cm /Im0 Do Q\n")  # SMask image
                for k in range(12):
                    ops.append("q %.2f 0 0 %.2f %.2f %.2f cm /Im1 Do Q\n" % ((s := rng.uniform(40, 150)), s, rng.uniform(0, 500), rng.uniform(0, 680)))
                ops.append("q /A75 gs /G0 Do Q\n")
                ops.append("q 1 0 0 1 0 -260 cm /G1 Do Q\n")
            else:
                ops.append("q /SMa gs 0.1 0.3 0.8 rg 0 0 612 792 re f Q\n")  # alpha soft mask on a page fill
                ops.append("q /SM gs /G0 Do Q\n")
                for k in range(60):
                    ops.append("q /A50 gs %.2f %.2f %.2f rg %.1f %.1f %.1f %.1f re f Q\n" % (rng.random(), rng.random(), rng.random(),
                               rng.uniform(0, 560), rng.uniform(0, 740), rng.uniform(20, 200), rng.uniform(20, 200)))
            yield "".join(ops).encode(), res

    raw_doc(pages, os.path.join(outdir, "transparency.pdf"))
    print("wrote transparency.pdf")


# ---------------------------------------------------------------------------
# (f) MRC: JPEG / JPX background + 1-bit stencil text layer
# ---------------------------------------------------------------------------
def jp2_bytes(img, rate):
    b = io.BytesIO()
    img.save(b, "JPEG2000", irreversible=True, quality_mode="rates", quality_layers=[rate], num_resolutions=6, no_jp2=False)
    return b.getvalue()


def mrc_layers(pg):
    rng = random.Random(1500 + pg)
    text, fig = scan_text_layer(rng, figure=True)
    color = scanify_color(text, rng, 1500 + pg, fig)
    # background: text removed (paper + figure), 150 DPI
    blank = Image.new("L", (SCAN_W, SCAN_H), 255)
    bg = scanify_color(blank, rng, 1500 + pg, fig).resize((SCAN_W // 2, SCAN_H // 2), Image.BILINEAR)
    mask = text.point(lambda v: 255 if v > 128 else 0).convert("1", dither=Image.Dither.NONE)
    return color, bg, mask


def make_mrc(outdir, npages=6, jbig2_dir=None):
    layers = [mrc_layers(pg) for pg in range(npages)]

    def pages_jpeg_g4(pdf):
        for color, bg, mask in layers:
            bgid = pdf.stream(image_dict(bg.width, bg.height, "/DeviceRGB", 8, "/DCTDecode"), jpeg_bytes(bg, 40, subsampling=2))
            g4 = g4_bytes(mask)
            black_is_1 = "false"
            mid = pdf.stream("/Type /XObject /Subtype /Image /Width %d /Height %d /ImageMask true /BitsPerComponent 1 /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns %d /Rows %d /BlackIs1 %s >>"
                             % (mask.width, mask.height, mask.width, mask.height, black_is_1), g4)
            content = b"q 612 0 0 792 0 0 cm /Bg Do Q q 0.12 0.09 0.07 rg 612 0 0 792 0 0 cm /Fg Do Q\n"
            yield content, "<< /XObject << /Bg %d 0 R /Fg %d 0 R >> >>" % (bgid, mid)

    raw_doc(pages_jpeg_g4, os.path.join(outdir, "mrc_jpeg_g4.pdf"))
    print("wrote mrc_jpeg_g4.pdf")

    # JPX full-page colour scan (jpx_scan class)
    def pages_jpx(pdf):
        for color, bg, mask in layers:
            iid = pdf.stream(image_dict(color.width, color.height, "/DeviceRGB", 8, "/JPXDecode"), jp2_bytes(color, 40))
            yield b"q 612 0 0 792 0 0 cm /Im0 Do Q\n", "<< /XObject << /Im0 %d 0 R >> >>" % iid

    raw_doc(pages_jpx, os.path.join(outdir, "jpx_scan.pdf"))
    print("wrote jpx_scan.pdf")

    # MRC with JPX background + JBIG2 mask (needs encoded JBIG2 streams)
    if jbig2_dir and os.path.exists(os.path.join(jbig2_dir, "page-00.jb2")):
        def pages_jpx_jbig2(pdf):
            gpath = os.path.join(jbig2_dir, "globals.jb2")
            gid = None
            if os.path.exists(gpath):
                with open(gpath, "rb") as f:
                    gid = pdf.stream("", f.read())
            for i, (color, bg, mask) in enumerate(layers):
                bgid = pdf.stream(image_dict(bg.width, bg.height, "/DeviceRGB", 8, "/JPXDecode"), jp2_bytes(bg, 60))
                with open(os.path.join(jbig2_dir, "page-%02d.jb2" % i), "rb") as f:
                    jb = f.read()
                parms = " /DecodeParms << /JBIG2Globals %d 0 R >>" % gid if gid else ""
                mid = pdf.stream("/Type /XObject /Subtype /Image /Width %d /Height %d /ImageMask true /BitsPerComponent 1 /Filter /JBIG2Decode%s"
                                 % (mask.width, mask.height, parms), jb)
                content = b"q 612 0 0 792 0 0 cm /Bg Do Q q 0.12 0.09 0.07 rg 612 0 0 792 0 0 cm /Fg Do Q\n"
                yield content, "<< /XObject << /Bg %d 0 R /Fg %d 0 R >> >>" % (bgid, mid)

        raw_doc(pages_jpx_jbig2, os.path.join(outdir, "mrc_jpx_jbig2.pdf"))
        print("wrote mrc_jpx_jbig2.pdf")
    # PBMs of the MRC masks for the JBIG2 encoder
    pbmdir = os.path.join(outdir, "pbm_mrc")
    os.makedirs(pbmdir, exist_ok=True)
    for i, (_, _, mask) in enumerate(layers):
        mask.save(os.path.join(pbmdir, "page-%02d.pbm" % i))


def main():
    outdir = sys.argv[1]
    classes = sys.argv[2:] or ["all"]
    os.makedirs(outdir, exist_ok=True)
    jbig2_dir = os.environ.get("JBIG2_DIR")
    if "all" in classes or "text" in classes:
        make_text(outdir)
    if "all" in classes or "vector" in classes:
        make_vector(outdir)
    if "all" in classes or "scans" in classes:
        make_scans(outdir)
    if "all" in classes or "bilevel" in classes:
        make_bilevel(outdir)
    if "all" in classes or "transp" in classes:
        make_transp(outdir)
    if "all" in classes or "mrc" in classes:
        make_mrc(outdir, jbig2_dir=jbig2_dir)


if __name__ == "__main__":
    main()
