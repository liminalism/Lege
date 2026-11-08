"""Gray JPX fixture for the A12 (fused gray 9/7) timing. Stdlib only.

usage: python -I mkgrayjpx.py png <outdir>          writes gray_2000x2800.png
       python -I mkgrayjpx.py pdf <outdir> <file.jp2>  writes gray_jpx.pdf

Encode the PNG with jp2lam in between (irreversible 9/7, the default lossy
mode): `jp2lam encode gray_2000x2800.png --quality 60`. The PDF draws the JPX
stream on three pages: full page, a window into it, and minified; all three
take jp2lam's packed Gray8 output path.
"""
import os
import random
import struct
import sys
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mkstencil import Pdf  # noqa: E402

W, H = 2000, 2800


def gray_rows(w, h, seed):
    """A scan-like page: paper tone gradient, text-ish dark blobs, noise."""
    rng = random.Random(seed)
    rows = []
    for y in range(h):
        base = 215 + (y * 25) // h
        row = bytearray(min(255, base + rng.randint(-6, 6)) for _ in range(w))
        if 200 < y < h - 200 and (y // 40) % 2 == 0 and (y % 40) < 26:
            x = 180
            while x < w - 180:
                ww = rng.randint(30, 160)
                if rng.random() < 0.7:
                    for xx in range(x, min(x + ww, w - 180)):
                        row[xx] = rng.randint(20, 70)
                x += ww + rng.randint(14, 30)
        rows.append(bytes(row))
    return rows


def write_png(path, rows, w, h):
    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + r for r in rows)
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 6))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def write_pdf(path, jp2_bytes):
    pdf = Pdf()
    img = pdf.add(
        b"<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray "
        b"/BitsPerComponent 8 /Filter /JPXDecode /Length %d >>\nstream\n" % (W, H, len(jp2_bytes))
        + jp2_bytes + b"\nendstream")
    pages = [
        pdf.page("q 612 0 0 792 0 0 cm /G Do Q", {"G": img}),
        # A window: the image scaled 2x and offset so only part lands on the page.
        pdf.page("q 1224 0 0 1584 -300 -400 cm /G Do Q", {"G": img}),
        # Minified to a quarter page, fractional placement.
        pdf.page("1 1 1 rg 0 0 612 792 re f q 300.3 0 0 420.7 150.2 180.6 cm /G Do Q", {"G": img}),
    ]
    pdf.write(path, pages)


if __name__ == "__main__":
    mode, out = sys.argv[1], sys.argv[2]
    os.makedirs(out, exist_ok=True)
    if mode == "png":
        write_png(os.path.join(out, "gray_2000x2800.png"), gray_rows(W, H, 21), W, H)
        print("wrote png")
    elif mode == "pdf":
        with open(sys.argv[3], "rb") as f:
            jp2 = f.read()
        write_pdf(os.path.join(out, "gray_jpx.pdf"), jp2)
        print("wrote", os.path.join(out, "gray_jpx.pdf"), len(jp2), "jpx bytes")
