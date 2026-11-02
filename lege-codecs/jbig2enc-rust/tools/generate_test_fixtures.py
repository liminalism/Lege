#!/usr/bin/env python3
"""Generate original font-free PBMs for the distributed regression suite.

SPDX-License-Identifier: MIT
All pixels come from the geometric rules below; no scan, font, external image
or codec output is used. --check verifies both raster bytes and their inventory.
"""
import argparse
import hashlib
import json
from pathlib import Path

CRATE = Path(__file__).resolve().parents[1]


def raster(width, height, points):
    data = bytearray(((width + 7) // 8) * height)
    for x, y in points:
        assert 0 <= x < width and 0 <= y < height
        data[y * ((width + 7) // 8) + x // 8] |= 128 >> (x % 8)
    return (f"P4\n# Original geometric fixture; SPDX-License-Identifier: MIT\n{width} {height}\n".encode()
            + data)


def fixtures():
    yield "blank.pbm", 2, 2, set(), "Four white pixels; no external generator or image."
    points = set()
    # Repeated connected geometric symbols with counters and internal bars.
    # They are deliberately shapes, not renderings of a font or alphabet.
    for row in range(9):
        for column in range(15):
            kind = (column + 3 * row) % 8
            left, top = 7 + 16 * column, 8 + 19 * row
            for y in range(13):
                for x in range(9):
                    border = x in (0, 8) or y in (0, 12)
                    vertical = kind & 1 and x == 4
                    horizontal = kind & 2 and y == 6
                    diagonal = kind & 4 and x == y % 9
                    if border or vertical or horizontal or diagonal:
                        points.add((left + x, top + y))
    yield "symbols.pbm", 257, 193, points, "135 repeated geometric symbols, eight variants; odd width."
    points = {(x, 47) for x in range(3, 127)}
    points.update((127, y) for y in range(14, 89))
    points.update((x, y) for y in range(9, 25) for x in range(14, 39))
    points.update((2 + 11 * i, 3 + (17 * i) % 89) for i in range(11))
    points.update((70 + i, 61 + i) for i in range(19))
    yield "marks.pbm", 131, 97, points, "Rule, dense rectangle, isolated marks and diagonal contacts; odd width."


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=CRATE / "tests/fixtures/original")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    files = {}
    inventory = []
    for name, width, height, points, description in fixtures():
        data = raster(width, height, points)
        files[name] = data
        inventory.append({"path": name, "width": width, "height": height,
                          "black_pixels": len(points), "bytes": len(data),
                          "sha256": hashlib.sha256(data).hexdigest(), "description": description})
    manifest = {"license": "MIT", "origin": "Original geometric rules in tools/generate_test_fixtures.py; no external image/font/scan",
                "generator_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                "files": inventory}
    files["manifest.json"] = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
    if args.check:
        for name, expected in files.items():
            actual = args.output / name
            if not actual.is_file() or actual.read_bytes() != expected:
                raise SystemExit(f"fixture differs from original generator: {actual}")
        print(f"verified {len(inventory)} original fixtures and inventory")
    else:
        args.output.mkdir(parents=True, exist_ok=True)
        for name, data in files.items():
            (args.output / name).write_bytes(data)
        print(f"generated {len(inventory)} original fixtures and inventory in {args.output}")


if __name__ == "__main__":
    main()
