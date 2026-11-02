#!/usr/bin/env python3
"""Matched, pinned-CPU C/Rust generic kernel runs on identical packed pages.

SPDX-License-Identifier: MIT
Times context allocation, template-0 coding (TPGDON off), finalization, output
copy and destruction. Input preparation, file output and byte verification are
outside each reported timer. This is a kernel comparison, not a C PDF pipeline.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import re
import statistics
import struct
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def pbm(path):
    data = path.read_bytes()
    cursor = 0
    tokens = []
    while len(tokens) < 3:
        while data[cursor:cursor+1].isspace():
            cursor += 1
        if data[cursor:cursor+1] == b"#":
            cursor = data.index(b"\n", cursor) + 1
            continue
        start = cursor
        while not data[cursor:cursor+1].isspace():
            cursor += 1
        tokens.append(data[start:cursor])
    assert tokens[0] == b"P4", path
    cursor += 2 if data[cursor:cursor+2] == b"\r\n" else 1
    width, height = map(int, tokens[1:])
    stride = (width + 7) // 8
    raster = data[cursor:]
    assert len(raster) == stride * height, path
    words = bytearray()
    for y in range(height):
        row = raster[y*stride:(y+1)*stride]
        for x in range(0, width, 32):
            word = int.from_bytes(row[x//8:x//8+4].ljust(4, b"\0"), "big")
            if x + 32 > width:
                word &= (0xffffffff << (32 - (width-x))) & 0xffffffff
            words.extend(struct.pack("<I", word))
    return width, height, words


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--baseline-worker", type=Path, help="also pair the earlier Rust worker; require exact raw equality")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--pairs", type=int, default=5)
    args = parser.parse_args()
    assert args.rounds > 0 and args.pairs > 0
    args.output.mkdir(parents=True, exist_ok=False)
    sources = sorted(args.corpus.glob("*.pbm"))
    assert sources
    packed = bytearray(b"GREGION1" + struct.pack("<I", len(sources)))
    inventory = []
    for source in sources:
        width, height, words = pbm(source)
        packed.extend(struct.pack("<III", width, height, len(words)//4))
        packed.extend(words)
        inventory.append({"file": str(source), "sha256": digest(source), "width": width, "height": height})
    input_path = args.output.resolve() / "packed-input.bin"
    input_path.write_bytes(packed)
    cpu = min(os.sched_getaffinity(0))
    env = os.environ.copy()
    for key in ["GENERIC_PROFILE", "GENERIC_OUTPUT"]:
        env.pop(key, None)
    env.update({"GENERIC_INPUT": str(input_path), "GENERIC_ROUNDS": str(args.rounds)})
    metadata = {"corpus": inventory, "packed_sha256": digest(input_path), "cpu": cpu,
                "rounds": args.rounds, "pairs": args.pairs,
                "rust_worker": {"path": str(args.worker.resolve()), "sha256": digest(args.worker)},
                "c_reference": {"path": str(args.reference.resolve()), "sha256": digest(args.reference)},
                "c_source": {p.name: digest(p) for p in args.reference.parent.glob("jbig2arith.*")},
                "scope": __doc__}
    if args.baseline_worker:
        metadata["baseline_worker"] = {"path": str(args.baseline_worker.resolve()), "sha256": digest(args.baseline_worker)}
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rows = []
    rng = random.Random(20261101)
    for pair in range(args.pairs):
        order = ["rust", "c"] + (["baseline"] if args.baseline_worker else [])
        rng.shuffle(order)
        for role in order:
            output = args.output.resolve() / (role + "-bytes")
            output.mkdir(exist_ok=True)
            run_env = env.copy()
            if role in ("rust", "baseline"):
                worker = args.worker if role == "rust" else args.baseline_worker
                command = [str(worker.resolve()), "--ignored", "--exact", "generic_kernel_worker", "--nocapture"]
                if pair == 0:
                    run_env["GENERIC_OUTPUT"] = str(output)
            else:
                command = [str(args.reference.resolve()), str(input_path), str(args.rounds)]
                if pair == 0:
                    command.append(str(output))
            result = subprocess.run(command, env=run_env, text=True, capture_output=True,
                                    preexec_fn=lambda: os.sched_setaffinity(0, {cpu}))
            (args.output / f"{pair}-{role}.log").write_text(result.stdout + result.stderr)
            result.check_returncode()
            match = re.search(r"GENERIC_RESULT\t([0-9.]+)\t(\d+)", result.stdout)
            assert match, result.stdout
            rows.append({"pair": pair, "role": role, "seconds": float(match[1]), "bytes_all_rounds": int(match[2])})
        if pair == 0:
            checks = []
            for index in range(len(sources)):
                rust = (args.output / "rust-bytes" / f"{index:04}.bin").read_bytes()
                c = (args.output / "c-bytes" / f"{index:04}.bin").read_bytes()
                baseline = (args.output / "baseline-bytes" / f"{index:04}.bin").read_bytes() if args.baseline_worker else None
                if baseline is not None:
                    assert rust == baseline, f"Rust before/after raw bytes differ on {sources[index]}"
                canonical = rust[:-3] + b"\xff\xac" if rust.endswith(b"\xff\xff\xac") else rust
                assert canonical == c, f"C/Rust arithmetic bytes differ on {sources[index]}"
                checks.append({"index": index, "rust_sha256": hashlib.sha256(rust).hexdigest(),
                               "c_sha256": hashlib.sha256(c).hexdigest(), "canonical_equal": True,
                               **({"baseline_sha256": hashlib.sha256(baseline).hexdigest(), "raw_baseline_equal": True} if baseline is not None else {})})
            (args.output / "byte-checks.json").write_text(json.dumps(checks, indent=2) + "\n")
    ratios = [next(r["seconds"] for r in rows if r["pair"] == i and r["role"] == "rust") /
              next(r["seconds"] for r in rows if r["pair"] == i and r["role"] == "c") for i in range(args.pairs)]
    summary = {"runs": rows, "rust_over_c_ratios": ratios, "median_rust_over_c": statistics.median(ratios),
               "byte_equality": "all page payloads equal after removing the known legacy duplicate FF prefix"}
    if args.baseline_worker:
        baseline_ratios = [next(r["seconds"] for r in rows if r["pair"] == i and r["role"] == "rust") /
                           next(r["seconds"] for r in rows if r["pair"] == i and r["role"] == "baseline") for i in range(args.pairs)]
        summary.update({"rust_over_baseline_ratios": baseline_ratios,
                        "median_rust_over_baseline": statistics.median(baseline_ratios),
                        "raw_baseline_equality": "every page payload is byte-for-byte unchanged"})
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
