#!/usr/bin/env python3
"""Report exact meaningful-line overlaps without copying reference source.

This is a triage aid, not an authorship detector: translated, renamed, reordered
or independently similar code needs human review. Reference files stay external
to the package. Comments, import lines and punctuation-only lines are ignored.
"""
import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import re


def lines(path):
    result = []
    block = False
    for number, raw in enumerate(path.read_text().splitlines(), 1):
        text = raw.strip()
        if block:
            if "*/" in text:
                block = False
            continue
        if text.startswith("/*"):
            block = "*/" not in text
            continue
        if text.startswith(("//", "use ", "pub use ", "#!", "#[")):
            continue
        # Exact normalized text; identifiers and literals are not generalized.
        text = re.sub(r"\s+", " ", text)
        if re.search(r"[A-Za-z0-9_]", text):
            result.append((number, text))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--window", type=int, default=6)
    args = parser.parse_args()
    if args.window < 2:
        parser.error("--window must be at least two meaningful lines")
    source = sorted(args.source.rglob("*.rs"))
    reference = sorted(args.reference.rglob("*.rs"))
    index = defaultdict(list)
    reference_lines = {}
    for path in reference:
        content = lines(path)
        reference_lines[path] = content
        for start in range(len(content) - args.window + 1):
            key = tuple(text for _, text in content[start:start + args.window])
            index[key].append((path, start))
    matches = defaultdict(set)
    source_lines = {}
    for path in source:
        content = lines(path)
        source_lines[path] = content
        for start in range(len(content) - args.window + 1):
            key = tuple(text for _, text in content[start:start + args.window])
            for other, offset in index.get(key, ()):
                matches[path, other, offset - start].add(start)
    blocks = []
    for (path, other, shift), starts in sorted(matches.items()):
        ordered = sorted(starts)
        groups = []
        for start in ordered:
            if groups and start <= groups[-1][1] + 1:
                groups[-1][1] = start
            else:
                groups.append([start, start])
        for first, last in groups:
            end = last + args.window - 1
            blocks.append({
                "source": str(path.relative_to(args.source)),
                "reference": str(other.relative_to(args.reference)),
                "source_lines": [source_lines[path][first][0], source_lines[path][end][0]],
                "reference_lines": [reference_lines[other][first + shift][0], reference_lines[other][end + shift][0]],
                "meaningful_lines": end - first + 1,
            })
    def inventory(paths, root):
        return [{"path": str(path.relative_to(root)),
                 "sha256": hashlib.sha256(path.read_bytes()).hexdigest()} for path in paths]
    report = {"method": "Exact sequences of normalized meaningful Rust lines; comments/imports/attributes/punctuation-only lines ignored",
              "window": args.window,
              "limitations": "Not a proof of independence, infringement or license compatibility. Translated/renamed/reordered expressions and non-Rust sources are outside this comparison.",
              "source_files": inventory(source, args.source),
              "reference_files": inventory(reference, args.reference), "matches": blocks}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"{len(source)} source files, {len(reference)} reference files, {len(blocks)} overlap blocks")


if __name__ == "__main__":
    main()
