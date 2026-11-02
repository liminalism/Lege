#!/usr/bin/env python3
"""Prepare fixed bilevel inputs from a local PDF corpus specification.

Example spec: {"dpi":72,"threshold":128,"sources":[{"id":"scan",
"path":"/local/book.pdf","pages":[10,20,50],"tags":["scanned-text"]}]}
Use "pages":"all" for a whole book. Source PDFs are read only. Pillow and
external Poppler tools are required; no document data is included in the crate.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

from PIL import Image, __version__ as pillow_version


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spec", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--root", type=Path, help="base directory for relative PDF paths in the spec")
    parser.add_argument("--pdftoppm", default="pdftoppm")
    parser.add_argument("--pdfinfo", default="pdfinfo")
    args = parser.parse_args()
    spec = json.loads(args.spec.read_text())
    dpi, threshold = spec.get("dpi", 72), spec.get("threshold", 128)
    if not isinstance(dpi, (float, int)) or dpi <= 0 or not isinstance(threshold, int) or not 1 <= threshold <= 255:
        parser.error("dpi must be positive; threshold must be an integer in 1..255")
    sources = spec.get("sources", [])
    ids = [source["id"] for source in sources]
    if not sources or len(set(ids)) != len(ids) or any(not re.fullmatch(r"[A-Za-z0-9_-]+", name) for name in ids):
        parser.error("sources need unique simple alphanumeric IDs")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    version = subprocess.run([args.pdftoppm, "-v"], text=True, capture_output=True, check=True)
    metadata = {"dpi": dpi, "threshold": threshold, "raster": "Poppler grayscale; sample >= threshold is white; no dither/crop/cleanup",
                "pdftoppm_version": (version.stdout + version.stderr).strip(),
                "spec_sha256": sha256(args.spec), "pillow_version": pillow_version, "sources": [], "pages": [],
                "fidelity_contract": "codec equality is against the prepared bilevel PBM, not the original grayscale PDF",
                "data_distribution": "source PDFs and derived raster data stay in the caller-selected corpus directory"}
    for source in sources:
        path = Path(source["path"])
        if args.root is not None and not path.is_absolute():
            path = args.root / path
        path = path.resolve()
        source_hash = sha256(path)
        if source.get("expected_sha256", source_hash) != source_hash:
            raise ValueError(f"source identity mismatch: {source['id']}")
        info = subprocess.run([args.pdfinfo, str(path)], text=True, capture_output=True, check=True)
        match = re.search(r"^Pages:\s+(\d+)\s*$", info.stdout, re.MULTILINE)
        if not match:
            raise ValueError(f"no PDF page count for source {source['id']}")
        page_count = int(match[1])
        if source.get("expected_pages", page_count) != page_count:
            raise ValueError(f"PDF page-count mismatch: {source['id']}")
        pages = list(range(1, page_count + 1)) if source["pages"] == "all" else source["pages"]
        if not pages or len(set(pages)) != len(pages) or any(type(page) is not int or page < 1 or page > page_count for page in pages):
            raise ValueError(f"invalid page selection for {source['id']}")
        metadata["sources"].append({"id": source["id"], "filename": path.name,
                                    "sha256": source_hash, "total_pages": page_count,
                                    "selected_pages": pages, "tags": source.get("tags", []),
                                    "notes": source.get("notes", ""), "pdfinfo_stderr": info.stderr})
        for page in pages:
            with tempfile.TemporaryDirectory(prefix="jbig2-raster-") as temporary:
                prefix = Path(temporary) / "page"
                render = subprocess.run([args.pdftoppm, "-f", str(page), "-l", str(page), "-r", str(dpi),
                                         "-gray", "-singlefile", str(path), str(prefix)],
                                        text=True, capture_output=True, check=True)
                with Image.open(prefix.with_suffix(".pgm")) as gray:
                    bilevel = gray.point(lambda sample: 255 if sample >= threshold else 0).convert("1", dither=Image.Dither.NONE)
                    target = output / f"{source['id']}-{page:04}.pbm"
                    bilevel.save(target, format="PPM")
                    width, height = bilevel.size
                    black_pixels = bilevel.histogram()[0]
                metadata["pages"].append({"file": target.name, "source": source["id"], "page": page,
                                           "width": width, "height": height, "black_pixels": black_pixels, "sha256": sha256(target),
                                           "render_stderr": render.stderr})
            if len(metadata["pages"]) % 10 == 0:
                print(f"prepared {len(metadata['pages'])} pages", flush=True)
        if sha256(path) != source_hash:
            raise RuntimeError(f"source changed during preparation: {source['id']}")
    (output / "preparation.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"prepared {len(metadata['pages'])} pages in {output}")


if __name__ == "__main__":
    main()
