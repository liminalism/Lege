"""Stencil (/ImageMask) fixtures for the B1/B2 hash gate. Stdlib only.

usage: python -I mkstencil.py <outdir> [jbig2dir]

Writes:
  stencil_flate.pdf  - Flate /ImageMask stencils: full-page minified text layer,
                       /Decode [1 0] at a fractional placement, small magnified
                       stencils (Nearest and /Interpolate), a rotated one, one
                       under a clip path with fill alpha, one under Multiply, a
                       truncated buffer, and a 1-bit /Gray image (non-stencil
                       bilevel, B2's other client).
  bilevel_600.pdf    - a 600-dpi Letter 1-bit gray image (5100x6600, over the
                       old 32M-entry SAT cap) and the same as /ImageMask.
  pbm/page-*.pbm     - text-like 300-dpi pages for mkjbig2.
  stencil_jbig2.pdf  - (when jbig2dir holds mkjbig2 output) MRC-shaped pages:
                       an 8-bit gray Flate background plus a JBIG2 /ImageMask
                       text layer with shared /JBIG2Globals.
"""
import os
import random
import sys
import zlib


def text_bitmap(w, h, seed, ink=0.5):
    """Packed 1-bit rows (MSB first): lines of word-like blobs, mostly 0."""
    rng = random.Random(seed)
    stride = (w + 7) // 8
    data = bytearray(stride * h)
    y = h // 20
    while y + 40 < h - h // 20:
        line_h = rng.randint(22, 34)
        x = w // 12
        words = []
        while x < w - w // 12:
            ww = rng.randint(40, 220)
            if x + ww >= w - w // 12:
                break
            words.append((x, ww))
            x += ww + rng.randint(18, 30)
        for yy in range(y, y + line_h):
            row = yy * stride
            for (wx, ww) in words:
                b0, b1 = wx // 8, (wx + ww) // 8
                chunk = bytes(
                    (rng.getrandbits(8) & rng.getrandbits(8)) if rng.random() < ink else 0
                    for _ in range(b1 - b0)
                )
                data[row + b0:row + b1] = chunk
        y += line_h + rng.randint(14, 26)
    return bytes(data)


def random_bits(w, h, seed):
    rng = random.Random(seed)
    stride = (w + 7) // 8
    return bytes(rng.getrandbits(8) for _ in range(stride * h))


class Pdf:
    def __init__(self):
        self.objs = []

    def add(self, body):
        self.objs.append(body)
        return len(self.objs)

    def stream(self, dict_body, data, flate=True):
        if flate:
            data = zlib.compress(data, 6)
            dict_body += " /Filter /FlateDecode"
        return self.add(b"<< " + dict_body.encode() + b" /Length %d >>\nstream\n" % len(data)
                        + data + b"\nendstream")

    def write(self, path, page_ids):
        pages_id = len(self.objs) + 1
        catalog_id = pages_id + 1
        # Fix up page parents (placeholder PARENT).
        for pid in page_ids:
            self.objs[pid - 1] = self.objs[pid - 1].replace(b"PARENT", b"%d 0 R" % pages_id)
        kids = " ".join("%d 0 R" % p for p in page_ids)
        self.add(("<< /Type /Pages /Kids [%s] /Count %d >>" % (kids, len(page_ids))).encode())
        self.add(("<< /Type /Catalog /Pages %d 0 R >>" % pages_id).encode())
        out = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
        offsets = []
        for i, body in enumerate(self.objs):
            offsets.append(len(out))
            out += b"%d 0 obj\n" % (i + 1) + body + b"\nendobj\n"
        xref = len(out)
        out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(self.objs) + 1)
        for off in offsets:
            out += b"%010d 00000 n \n" % off
        out += b"trailer\n<< /Size %d /Root %d 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
            len(self.objs) + 1, catalog_id, xref)
        with open(path, "wb") as f:
            f.write(out)

    def page(self, content, xobjects, extgstates=None, size=(612, 792)):
        cid = self.stream("", content.encode())
        xo = " ".join("/%s %d 0 R" % (k, v) for k, v in xobjects.items())
        gs = ""
        if extgstates:
            gs = " /ExtGState << %s >>" % " ".join(
                "/%s %d 0 R" % (k, v) for k, v in extgstates.items())
        return self.add((
            "<< /Type /Page /Parent PARENT /MediaBox [0 0 %g %g] "
            "/Resources << /XObject << %s >>%s >> /Contents %d 0 R >>"
            % (size[0], size[1], xo, gs, cid)).encode())


def stencil(pdf, w, h, data, decode_inverted=False, interpolate=False, flate=True):
    d = "/Type /XObject /Subtype /Image /Width %d /Height %d /ImageMask true" % (w, h)
    if decode_inverted:
        d += " /Decode [1 0]"
    if interpolate:
        d += " /Interpolate true"
    return pdf.stream(d, data, flate)


def gray1(pdf, w, h, data):
    return pdf.stream("/Type /XObject /Subtype /Image /Width %d /Height %d "
                      "/ColorSpace /DeviceGray /BitsPerComponent 1" % (w, h), data)


def gray8_background(pdf, w, h, seed):
    rng = random.Random(seed)
    rows = []
    for y in range(h):
        base = 200 + (y * 40) // h
        rows.append(bytes(min(255, base + rng.randint(-12, 12)) for _ in range(w)))
    return pdf.stream("/Type /XObject /Subtype /Image /Width %d /Height %d "
                      "/ColorSpace /DeviceGray /BitsPerComponent 8" % (w, h), b"".join(rows))


def flate_fixture(path):
    pdf = Pdf()
    pages = []
    W, H = 2550, 3300
    text = text_bitmap(W, H, 1)
    full = stencil(pdf, W, H, text)
    full_inv = stencil(pdf, W, H, bytes(255 - b for b in text_bitmap(W, H, 2)), True)
    small = stencil(pdf, 61, 43, random_bits(61, 43, 3))
    small_i = stencil(pdf, 61, 43, random_bits(61, 43, 4), interpolate=True)
    small_inv = stencil(pdf, 37, 29, random_bits(37, 29, 5), decode_inverted=True)
    mid = stencil(pdf, 900, 700, text_bitmap(900, 700, 6, ink=0.8))
    trunc_full = text_bitmap(800, 600, 7)
    trunc = stencil(pdf, 800, 600, trunc_full[: len(trunc_full) // 2])
    g1 = gray1(pdf, 1700, 2200, text_bitmap(1700, 2200, 8))
    alpha = pdf.add(b"<< /Type /ExtGState /ca 0.6 >>")
    mult = pdf.add(b"<< /Type /ExtGState /BM /Multiply >>")
    # 1: full-page text layer over a tinted page, minified at 72-200 dpi.
    pages.append(pdf.page(
        "0.95 0.93 0.85 rg 0 0 612 792 re f 0.1 0.1 0.35 rg q 612 0 0 792 0 0 cm /A Do Q",
        {"A": full}))
    # 2: inverted /Decode at a fractional placement, plus a second colour.
    pages.append(pdf.page(
        "0.5 0 0 rg q 500.3 0 0 640.7 50.25 60.6 cm /B Do Q "
        "0 0.4 0 rg q 200.2 0 0 150.9 20.5 600.3 cm /M Do Q",
        {"B": full_inv, "M": mid}))
    # 3: small magnified stencils, Nearest, /Interpolate and inverted, at
    # fractional positions; one mirrored.
    pages.append(pdf.page(
        "0 0 0.7 rg q 250.6 0 0 180.3 30.3 500.7 cm /S Do Q "
        "0.7 0.2 0 rg q 250.6 0 0 180.3 300.4 500.2 cm /I Do Q "
        "0 0.5 0.5 rg q 140.1 0 0 110.7 40.8 300.1 cm /V Do Q "
        "0.3 0.3 0.3 rg q -140.1 0 0 -110.7 400.8 400.1 cm /S Do Q",
        {"S": small, "I": small_i, "V": small_inv}))
    # 4: rotated (generic), clipped with fill alpha, and Multiply (generic).
    pages.append(pdf.page(
        "0.9 0.9 0.2 rg 0 0 612 792 re f "
        "0.1 0.2 0.6 rg q 0.9659 0.2588 -0.2588 0.9659 120.4 80.2 cm 300 0 0 230 0 0 cm /M Do Q "
        "q 100 450 m 500 470 l 450 760 l 120 700 l h W n /GA gs 0.6 0 0.2 rg "
        "400.5 0 0 310.25 90.3 440.6 cm /M Do Q "
        "q /GM gs 0 0.6 0.9 rg 300.3 0 0 230.7 250.1 120.9 cm /M Do Q",
        {"M": mid}, {"GA": alpha, "GM": mult}))
    # 5: truncated stencil data (generic, no panic) and a 1-bit gray image.
    pages.append(pdf.page(
        "0 0 0 rg q 300.4 0 0 225.2 20.6 500.3 cm /T Do Q "
        "q 380.2 0 0 491.9 200.7 10.4 cm /G Do Q",
        {"T": trunc, "G": g1}))
    pdf.write(path, pages)


def bilevel600_fixture(path):
    pdf = Pdf()
    W, H = 5100, 6600
    bits = text_bitmap(W, H, 11, ink=0.6)
    g = gray1(pdf, W, H, bits)
    s = stencil(pdf, W, H, bits)
    pages = [
        pdf.page("q 612 0 0 792 0 0 cm /G Do Q", {"G": g}),
        pdf.page("0.2 0.1 0 rg q 612 0 0 792 0 0 cm /S Do Q", {"S": s}),
    ]
    pdf.write(path, pages)


def write_pbms(outdir, n=3):
    os.makedirs(outdir, exist_ok=True)
    W, H = 2550, 3300
    for i in range(n):
        with open(os.path.join(outdir, "page-%02d.pbm" % i), "wb") as f:
            f.write(b"P4\n%d %d\n" % (W, H))
            f.write(text_bitmap(W, H, 100 + i))


def jbig2_fixture(path, jdir, n=3):
    pdf = Pdf()
    W, H = 2550, 3300
    globals_id = pdf.stream("", open(os.path.join(jdir, "globals.jb2"), "rb").read(), flate=False)
    pages = []
    for i in range(n):
        data = open(os.path.join(jdir, "page-%02d.jb2" % i), "rb").read()
        sid = pdf.add(
            b"<< /Type /XObject /Subtype /Image /Width %d /Height %d /ImageMask true "
            b"/Filter /JBIG2Decode /DecodeParms << /JBIG2Globals %d 0 R >> /Length %d >>\n"
            b"stream\n" % (W, H, globals_id, len(data)) + data + b"\nendstream")
        bg = gray8_background(pdf, 425, 550, 200 + i)
        pages.append(pdf.page(
            "q 612 0 0 792 0 0 cm /BG Do Q 0.05 0.05 0.1 rg q 612 0 0 792 0 0 cm /FG Do Q",
            {"BG": bg, "FG": sid}))
    pdf.write(path, pages)


if __name__ == "__main__":
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    if len(sys.argv) > 2:
        jbig2_fixture(os.path.join(out, "stencil_jbig2.pdf"), sys.argv[2])
    else:
        flate_fixture(os.path.join(out, "stencil_flate.pdf"))
        bilevel600_fixture(os.path.join(out, "bilevel_600.pdf"))
        write_pbms(os.path.join(out, "pbm"))
