"""Extract raw /JPXDecode streams from PDFs for the paired benchmark.

usage: python extract.py <out-dir> <name>=<pdf>[:p0,p1,...] ...

    python extract.py streams structures=D:/pdf-test/Structures.pdf:0,1,2,100,300 \
        cm=D:/pdf-test/chinese-medicine.pdf:0,1,2,100,300

Writes <out>/<name>-p<page>-l<k>.jpx for the k-th JPX XObject of each page
(the MRC books carry two full-page layers per page). Needs pymupdf.
"""
import os
import sys

import pymupdf

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
for spec in sys.argv[2:]:
    name, rest = spec.split("=", 1)
    path, _, pages = rest.partition(":")
    doc = pymupdf.open(path)
    page_list = [int(p) for p in pages.split(",")] if pages else range(doc.page_count)
    for p in page_list:
        k = 0
        for im in doc[p].get_images(full=True):
            xref = im[0]
            if "JPX" not in str(doc.xref_get_key(xref, "Filter")):
                continue
            raw = doc.xref_stream_raw(xref)
            fn = f"{out}/{name}-p{p}-l{k}.jpx"
            with open(fn, "wb") as f:
                f.write(raw)
            print(fn, len(raw), doc.xref_get_key(xref, "Width"), doc.xref_get_key(xref, "Height"))
            k += 1
