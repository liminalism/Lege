#!/usr/bin/env python3
"""Paired A/B timing of two harness binaries.

    python3 -I ab.py ROUNDS A_BIN B_BIN -- ARGS...

Runs A and B alternately ROUNDS times each (order swapped every round to
cancel drift), with the same ARGS, and parses the first number after
`pass_ms` (pass index set by --pass, default the last) from each run's
stdout. Reports per-pass medians, the median of the per-round B/A ratios,
and how many rounds B won. Also checks that both binaries print the same
`checksum`. Run under the single-thread environment from the README.
"""
import statistics
import subprocess
import sys


def run(binary, args, pass_index):
    out = subprocess.run([binary, *args], capture_output=True, text=True, check=True).stdout
    line = next(l for l in out.splitlines() if l.startswith("pass_ms"))
    fields = line.split()
    times = []
    for f in fields[1:]:
        try:
            times.append(float(f))
        except ValueError:
            break
    checksum = fields[fields.index("checksum") + 1] if "checksum" in fields else None
    return times[pass_index], checksum


def main():
    argv = sys.argv[1:]
    pass_index = -1
    if argv and argv[0] == "--pass":
        pass_index = int(argv[1])
        argv = argv[2:]
    rounds, a_bin, b_bin = int(argv[0]), argv[1], argv[2]
    args = argv[argv.index("--") + 1:]
    a_times, b_times, ratios, sums = [], [], [], set()
    for r in range(rounds):
        order = [("A", a_bin), ("B", b_bin)] if r % 2 == 0 else [("B", b_bin), ("A", a_bin)]
        got = {}
        for name, binary in order:
            got[name] = run(binary, args, pass_index)
        a_times.append(got["A"][0])
        b_times.append(got["B"][0])
        ratios.append(got["B"][0] / got["A"][0])
        sums.add((got["A"][1], got["B"][1]))
    same = all(a == b for a, b in sums)
    wins = sum(1 for x in ratios if x < 1.0)
    print(
        f"A {statistics.median(a_times):.2f} ms  B {statistics.median(b_times):.2f} ms  "
        f"B/A median {statistics.median(ratios):.3f}  B faster in {wins}/{rounds}  "
        f"output {'identical' if same else 'DIFFERS'}"
    )


if __name__ == "__main__":
    main()
