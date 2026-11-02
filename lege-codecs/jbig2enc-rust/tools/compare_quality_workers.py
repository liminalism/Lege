#!/usr/bin/env python3
"""Interleave two already-built quality workers on one immutable corpus.

Build both with the same toolchain/profile/features before running. Compilation
is deliberately outside this process. This measures a local guardrail; it is
not a substitute for representative scanned-book/device measurements.
"""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repetitions", type=int, default=7)
    parser.add_argument("--mode", default="symbol_shipping")
    parser.add_argument("--allow-byte-changes", action="store_true", help="report reviewed encoding changes while still requiring identical source-pixel differences")
    args = parser.parse_args()
    if args.repetitions < 3:
        parser.error("at least three repetitions are required")
    if not (os.environ.get("JBIG2DEC") or shutil.which("jbig2dec")):
        parser.error("comparison requires JBIG2DEC or jbig2dec on PATH")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    corpus = args.corpus.resolve()
    pages = sorted(corpus.glob("*.pbm"))
    if not pages:
        parser.error("corpus is empty")
    hashes = {page.name: digest(page) for page in pages}
    workers = {name: getattr(args, name).resolve() for name in ("reference", "candidate")}
    metadata = {"workers": {name: {"path": str(path), "sha256": digest(path)} for name, path in workers.items()},
                "mode": args.mode, "repetitions": args.repetitions, "platform": platform.platform(),
                "corpus": str(corpus), "input_hashes": hashes, "seed": 714,
                "cpu": next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                             if line.startswith("model name")), "unknown")}
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    env = dict(os.environ, QUALITY_CORPUS=str(corpus), QUALITY_MODE=args.mode, QUALITY_REQUIRE_ORACLE="1")
    rng = random.Random(714)
    rows = []
    for repetition in range(args.repetitions):
        order = list(workers)
        rng.shuffle(order)
        for name in order:
            run = subprocess.run([str(workers[name]), "quality_corpus_worker", "--exact", "--ignored", "--nocapture"],
                                 env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            (output / f"{repetition}-{name}.log").write_text(run.stdout)
            run.check_returncode()
            results = [line.split("\t")[1:] for line in run.stdout.splitlines() if line.startswith("QUALITY_RESULT\t")]
            if len(results) != 1:
                raise RuntimeError("missing unique worker result")
            result = results[0]
            if result[0] != args.mode or int(result[1]) != len(pages):
                raise RuntimeError("worker did not encode the requested corpus/mode")
            rows.append({"repetition": repetition, "worker": name, "encoded_bytes": int(result[3]),
                         "encode_seconds": float(result[7]), "source_differences": int(result[8]),
                         "cold_decode_seconds": float(result[9]), "warm_decode_seconds": float(result[10]),
                         "oracle_source_differences": int(result[15])})
            if hashes != {page.name: digest(page) for page in sorted(corpus.glob("*.pbm"))}:
                raise RuntimeError("corpus changed during comparison")
    with (output / "runs.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=rows[0].keys())
        writer.writeheader()
        writer.writerows(rows)
    for field in ("source_differences", "oracle_source_differences") + (() if args.allow_byte_changes else ("encoded_bytes",)):
        if len({row[field] for row in rows}) != 1:
            raise RuntimeError(f"reference/candidate output contract differs: {field}")
    medians = {name: {field: statistics.median(row[field] for row in rows if row["worker"] == name)
                      for field in ("encode_seconds", "cold_decode_seconds", "warm_decode_seconds")}
               for name in workers}
    ratios = {field: medians["candidate"][field] / medians["reference"][field] for field in medians["reference"]}
    summary = {"medians": medians, "candidate_to_reference": ratios,
               "encoded_bytes":{name:sorted({row['encoded_bytes'] for row in rows if row['worker']==name}) for name in workers},
               "byte_changes_allowed":args.allow_byte_changes,
               "scope": "interleaved local comparison on the input hashes in metadata.json; no device/OS-cache claim"}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
