#!/usr/bin/env python3
"""Compare empty-queue GUI processes on Linux/X11 at equal physical sizes.

Build both with the same Cargo profile. This measures the frontend only;
worker/renderer/OCR peak memory requires a separate processing workload.
"""
import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


def xdotool(*args):
    return subprocess.check_output(["xdotool", *map(str, args)], text=True).strip()


def sample(label, binary, iteration, width, height, settle):
    with tempfile.TemporaryDirectory(prefix="lege-gui-memory-") as data_dir:
        env = os.environ.copy()
        env["XDG_DATA_HOME"] = data_dir
        with tempfile.TemporaryFile(mode="w+") as log:
            process = subprocess.Popen([str(binary.resolve())], env=env, stdout=log, stderr=log)
            try:
                time.sleep(settle)
                if process.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(f"{label} exited {process.returncode}: {log.read()}")
                window = xdotool("search", "--onlyvisible", "--pid", process.pid).splitlines()[-1]
                xdotool("windowsize", window, width, height)
                xdotool("mousemove", 0, 0)
                time.sleep(1)
                geometry = dict(line.split("=", 1) for line in xdotool("getwindowgeometry", "--shell", window).splitlines())
                actual = int(geometry["WIDTH"]), int(geometry["HEIGHT"])
                if actual != (width, height):
                    raise RuntimeError(f"{label} constrained size to {actual}; choose a size both accept")
                metrics = {}
                for line in Path(f"/proc/{process.pid}/smaps_rollup").read_text().splitlines():
                    if ":" in line:
                        key, value = line.split(":", 1)
                        if key in {"Rss", "Pss", "Private_Clean", "Private_Dirty", "Shared_Clean"}:
                            metrics[key + "_KiB"] = int(value.split()[0])
                status = Path(f"/proc/{process.pid}/status").read_text().splitlines()
                metrics["threads"] = int(next(line.split()[1] for line in status if line.startswith("Threads:")))
                return {"frontend": label, "iteration": iteration, "width": width, "height": height, "metrics": metrics}
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--freya-bin", type=Path, required=True)
    parser.add_argument("--pixelkit-bin", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--width", type=int, default=1980)
    parser.add_argument("--height", type=int, default=1452)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--settle", type=float, default=3)
    parser.add_argument("--profile", default="unspecified", help="Cargo profile used to build both binaries")
    args = parser.parse_args()
    if min(args.width, args.height, args.runs) < 1 or not 0 < args.settle <= 30:
        parser.error("dimensions/runs must be positive; settle must be in (0,30]")
    records = []
    for iteration in range(args.runs):
        for label, binary in [("freya", args.freya_bin), ("pixelkit", args.pixelkit_bin)]:
            records.append(sample(label, binary, iteration, args.width, args.height, args.settle))
    medians = {
        label: {key: statistics.median(row["metrics"][key] for row in records if row["frontend"] == label)
                for key in ["Rss_KiB", "Pss_KiB", "threads"]}
        for label in ["freya", "pixelkit"]
    }
    result = {"workload": "empty queue, idle, matched physical window size", "platform": "Linux/X11",
              "profile": args.profile,
              "medians": medians, "samples": records}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(medians, indent=2))


if __name__ == "__main__":
    main()
