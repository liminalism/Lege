#!/usr/bin/env python3
"""Reproducible corpus runs, each measured in its own Linux process.

The built-in corpus is synthetic stress data, not evidence of scanned-book
compression quality. Use --corpus with fixed scanned PBMs for release evidence.
Only Python's standard library is required. Run --help for all options.
"""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess

CRATE = Path(__file__).resolve().parents[1]
BASE_MODES = ("generic", "symbol_shipping", "symbol_fixed", "unify_shipping", "unify_fixed", "refine_shipping",
         "symbol_shared", "symbol_local", "exact", "exact_allglobal", "exact_optimized", "unify_safe", "lossy_optimized")
MODES = BASE_MODES + tuple(
    f"{mode}_group{size}" for mode in BASE_MODES for size in (32,64,128,"whole"))
FIELDS = (
    "mode", "pages", "auto_thresh", "encoded_bytes", "global_bytes", "local_bytes",
    "residual_bytes", "encode_seconds", "differing_pixels", "cold_decode_seconds",
    "warm_decode_seconds", "decoded_global_bitmap_bytes", "comparator_calls",
    "exact_hits", "refined_hits", "jbig2dec_differing_pixels",
)


def synthetic_page(kind, variation):
    """Integer-only original geometric shapes; no copied font or scan assets."""
    width, height = 321, 257
    pixels = bytearray(width * height)

    def ink(x, y):
        if 0 <= x < width and 0 <= y < height:
            pixels[y * width + x] = 1

    for row in range(12):
        for col in range(21):
            shape = (col + row * 3 + variation) % 11
            ox, oy = 5 + col * 15, 5 + row * 20
            if kind == "sparse" and (row, col) not in ((0, 0), (11, 20)):
                continue
            for y in range(12):
                for x in range(8):
                    # Repeated closed/open glyph-like shapes, connected strokes,
                    # holes and one-pixel openings expose substitution damage.
                    black = x == 0 or y in (0, 11) or (x == 7 and shape % 2 == 0)
                    if kind == "symbol_diversity":
                        black |= (x + y + shape) % 7 == 0
                    if kind == "math":
                        black = y == 5 or (shape % 2 == 0 and x == 4) or x == y
                    if black:
                        ink(ox + x, oy + y)
            if kind == "noisy":
                ink(ox + 1, oy + 2 + (shape % 8))
    # Isolated dots, page corners, and marks that must survive in exact modes.
    for x, y in ((0, 0), (320, 256), (1, 250), (157, 253), (319, 2)):
        ink(x, y)
    if kind == "noisy":
        state = 0x12345678 + variation
        for _ in range(150):
            state = (1664525 * state + 1013904223) & 0xFFFFFFFF
            ink(state % width, (state >> 16) % height)
    stride = (width + 7) // 8
    packed = bytearray(stride * height)
    for y in range(height):
        for x in range(width):
            packed[y * stride + x // 8] |= pixels[y * width + x] << (7 - x % 8)
    return f"P4\n{width} {height}\n".encode() + packed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="new results directory (must not exist)")
    parser.add_argument("--corpus", type=Path, help="flat directory of fixed scanned P4 PBMs; otherwise synthetic")
    parser.add_argument("--pages", type=int, default=10, help="synthetic page count; use 350 for long-book stress")
    parser.add_argument("--modes", nargs="+", choices=MODES, default=list(BASE_MODES))
    parser.add_argument("--require-oracle", action="store_true", help="fail unless jbig2dec is runnable")
    parser.add_argument("--require-pdf-oracles", action="store_true", help="also require PDFTOPPM and PDFIUM_PYTHON and compare every rendered page")
    parser.add_argument("--debug", action="store_true", help="debug build for harness checks, not performance evidence")
    parser.add_argument("--save-streams", action="store_true", help="retain encoded PDF/JBIG2 and decoded PBMs in the output directory for diagnosis")
    parser.add_argument("--global-bitmap-limit", type=int, help="explicit decoded-global bitmap cap in bytes; no default reader budget")
    args = parser.parse_args()
    if args.pages < 1:
        parser.error("--pages must be positive")
    if args.global_bitmap_limit is not None and args.global_bitmap_limit < 0:
        parser.error("--global-bitmap-limit must be nonnegative")
    if platform.system() != "Linux" or not hasattr(os, "wait4"):
        parser.error("peak RSS measurement currently requires Linux wait4")
    require_oracle = args.require_oracle or args.require_pdf_oracles
    if require_oracle and not (os.environ.get("JBIG2DEC") or shutil.which("jbig2dec")):
        parser.error("--require-oracle needs JBIG2DEC or jbig2dec on PATH")
    if args.require_pdf_oracles and not all(os.environ.get(name) for name in ("PDFTOPPM", "PDFIUM_PYTHON")):
        parser.error("--require-pdf-oracles needs PDFTOPPM and PDFIUM_PYTHON")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    if args.corpus:
        corpus = args.corpus.resolve()
    else:
        corpus = output / "synthetic-corpus-v1"
        corpus.mkdir()
        kinds = ("text", "noisy", "symbol_diversity", "math", "sparse")
        for index in range(args.pages):
            kind = kinds[index % len(kinds)]
            (corpus / f"{index:04}-{kind}.pbm").write_bytes(synthetic_page(kind, index % 17))
    pages = sorted(corpus.glob("*.pbm"))
    if not pages:
        parser.error(f"no PBMs in {corpus}")
    manifest = [{"file": page.name, "bytes": page.stat().st_size,
                 "sha256": hashlib.sha256(page.read_bytes()).hexdigest()} for page in pages]
    (output / "corpus.json").write_text(json.dumps(manifest, indent=2) + "\n")
    command = ["cargo", "test", "--manifest-path", str(CRATE / "Cargo.toml"),
               "--test", "quality_benchmark", "--no-run", "--message-format=json"]
    if not args.debug:
        command.append("--release")
    build = subprocess.run(command, cwd=CRATE, text=True, stdout=subprocess.PIPE, check=True)
    executables = [message["executable"] for line in build.stdout.splitlines()
                   if (message := json.loads(line)).get("executable")
                   and message.get("target", {}).get("name") == "quality_benchmark"]
    if len(executables) != 1:
        raise RuntimeError(f"expected one quality worker, got {executables}")
    metadata = {"corpus": str(corpus), "synthetic": not bool(args.corpus),
                "pages": len(pages), "debug": args.debug, "modes": args.modes,
                "global_bitmap_limit": args.global_bitmap_limit,
                "require_oracle": require_oracle, "require_pdf_oracles": args.require_pdf_oracles, "platform": platform.platform(),
                "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=CRATE, text=True).strip(),
                "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
                "cpu": next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                             if line.startswith("model name")), "unknown"),
                "source_diff_sha256": hashlib.sha256(subprocess.check_output(
                    ["git", "diff", "HEAD", "--", str(CRATE)], cwd=CRATE)).hexdigest(),
                "worker_sha256": hashlib.sha256((CRATE / "tests/quality_benchmark.rs").read_bytes()).hexdigest(),
                "worker_binary_sha256": hashlib.sha256(Path(executables[0]).read_bytes()).hexdigest(),
                "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                "lockfile_sha256": hashlib.sha256((CRATE / "Cargo.lock").read_bytes()).hexdigest(),
                "source_files": {str(path.relative_to(CRATE)): hashlib.sha256(path.read_bytes()).hexdigest()
                                 for path in sorted([*CRATE.joinpath("src").rglob("*.rs"),
                                                     CRATE / "build.rs", CRATE / "Cargo.toml"])},
                "memory_scope": "whole worker peak RSS, including input loading and verification; excludes build",
                "cold_scope": "native globals + page decode per call; no OS cache flush"}
    oracle = os.environ.get("JBIG2DEC") or shutil.which("jbig2dec")
    metadata["jbig2dec"] = (subprocess.check_output([oracle, "--version"], text=True).strip()
                           if oracle else "unavailable")
    if args.require_pdf_oracles:
        poppler = subprocess.run([os.environ["PDFTOPPM"], "-v"], text=True, capture_output=True, check=True)
        metadata["poppler"] = (poppler.stdout + poppler.stderr).strip()
        metadata["pdfium"] = subprocess.check_output([os.environ["PDFIUM_PYTHON"], str(CRATE / "tools/pdfium_oracle.py"), "--version"], text=True).strip()
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    with (output / "results.csv").open("w", newline="") as result_file:
        writer = csv.writer(result_file)
        writer.writerow((*FIELDS, "worker_peak_rss_kib"))
        for mode in args.modes:
            env = dict(os.environ, QUALITY_CORPUS=str(corpus), QUALITY_MODE=mode,
                       QUALITY_REQUIRE_ORACLE=str(int(require_oracle)),
                       QUALITY_PDF_ORACLES=str(int(args.require_pdf_oracles)))
            if args.save_streams:
                env["QUALITY_ARTIFACT_DIR"] = str(output / "streams" / mode)
            env.pop("QUALITY_GLOBAL_BITMAP_LIMIT",None)
            if args.global_bitmap_limit is not None:
                env["QUALITY_GLOBAL_BITMAP_LIMIT"] = str(args.global_bitmap_limit)
            log = output / f"{mode}.log"
            # wait4 returns resource usage for this child alone; cumulative
            # RUSAGE_CHILDREN would contaminate later modes with earlier peaks.
            # Write to a file so a full pipe cannot deadlock the worker.
            with log.open("w") as stream:
                with subprocess.Popen([executables[0], "quality_corpus_worker", "--exact",
                                       "--ignored", "--nocapture"], cwd=CRATE, env=env,
                                      stdout=stream, stderr=subprocess.STDOUT) as worker:
                    _, status, usage = os.wait4(worker.pid, 0)
                    worker.returncode = os.waitstatus_to_exitcode(status)
                    if worker.returncode:
                        raise RuntimeError(f"worker failed ({worker.returncode}); see {log}")
            stdout = log.read_text()
            (output / f"{mode}.rss-kib.txt").write_text(f"{usage.ru_maxrss}\n")
            # Detect input changes during a run, not just differences at startup.
            for page, entry in zip(pages, manifest):
                if hashlib.sha256(page.read_bytes()).hexdigest() != entry["sha256"]:
                    raise RuntimeError(f"corpus changed while benchmarking: {page}")
            if sorted(corpus.glob("*.pbm")) != pages:
                raise RuntimeError("corpus page list changed while benchmarking")
            rows = [line.split("\t")[1:] for line in stdout.splitlines() if line.startswith("QUALITY_RESULT\t")]
            if len(rows) != 1 or len(rows[0]) != len(FIELDS):
                raise RuntimeError(f"invalid worker result for {mode}: {rows}")
            if int(rows[0][1]) != len(pages):
                raise RuntimeError(f"worker encoded a different corpus page count: {rows[0][1]}")
            writer.writerow((*rows[0], usage.ru_maxrss))
            result_file.flush()
            print(mode, "bytes=" + rows[0][3], "pixel_differences=" + rows[0][8], flush=True)
    print(output / "results.csv")


if __name__ == "__main__":
    main()
