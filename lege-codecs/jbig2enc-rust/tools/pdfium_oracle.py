#!/usr/bin/env python3
"""Render a one-page test PDF at one pixel per unit, using external PDFium.

Requires pypdfium2 and Pillow in the test Python environment. No renderer code or
binary is vendored. API reference: https://pypdfium2.readthedocs.io/en/stable/python_api.html
"""
import sys

import pypdfium2 as pdfium
from PIL import Image


def main():
    if len(sys.argv) == 2 and sys.argv[1] == "--version":
        print(f"pypdfium2 {pdfium.PYPDFIUM_INFO}; PDFium {pdfium.PDFIUM_INFO}")
        return
    if len(sys.argv) != 3:
        raise SystemExit("usage: pdfium_oracle.py input.pdf output.pbm")
    with pdfium.PdfDocument(sys.argv[1]) as document:
        if len(document) != 1:
            raise ValueError("expected one test page")
        page = document[0]
        bitmap = page.render(scale=1, grayscale=True, no_smoothimage=True,
                             no_smoothpath=True, no_smoothtext=True)
        bitmap.to_pil().convert("1", dither=Image.Dither.NONE).save(sys.argv[2], format="PPM")
        bitmap.close()
        page.close()


if __name__ == "__main__":
    main()
