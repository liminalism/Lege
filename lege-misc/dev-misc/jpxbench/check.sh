#!/bin/bash
# Rebuild the benchmark, verify jp2lam raster hashes against a recorded
# baseline, then print serial P-core-pinned timings (Windows; `start /affinity
# FFF` pins to the six P-cores of the reference host, adjust for others).
#
# usage: check.sh [--baseline] [extra jpxbench args...]
#   --baseline   record streams/baseline-hashes.txt instead of comparing
#   JPXBENCH_STREAMS   stream directory (default: ./streams)
set -u
cd "$(dirname "$0")" || exit 1
STREAMS_DIR="${JPXBENCH_STREAMS:-$PWD/streams}"
record=0
if [ "${1:-}" = "--baseline" ]; then record=1; shift; fi
cargo build --release 2>&1 | grep -E "^(error|warning)|Finished" -A7 | head -60
BIN="target/release/jpxbench.exe"
STREAMS=$(ls "$STREAMS_DIR"/*.jpx | tr '\n' ' ')
RAYON_NUM_THREADS=1 cmd //c "start /affinity FFF /b /wait $BIN --iters 1 --only jp2 $STREAMS" 2>&1 \
  | grep "raster hash" > "$STREAMS_DIR/current-hashes.txt"
if [ $record = 1 ]; then
  cp "$STREAMS_DIR/current-hashes.txt" "$STREAMS_DIR/baseline-hashes.txt"
  echo "HASHES: baseline recorded ($(wc -l < "$STREAMS_DIR/baseline-hashes.txt") rasters)"
elif diff -q "$STREAMS_DIR/baseline-hashes.txt" "$STREAMS_DIR/current-hashes.txt" > /dev/null; then
  echo "HASHES: identical to baseline ($(wc -l < "$STREAMS_DIR/baseline-hashes.txt") rasters)"
else
  echo "HASHES: DIFFER"
  diff "$STREAMS_DIR/baseline-hashes.txt" "$STREAMS_DIR/current-hashes.txt"
fi
RAYON_NUM_THREADS=1 cmd //c "start /affinity FFF /b /wait $BIN $* $STREAMS" 2>&1 | grep -v "raster hash"
