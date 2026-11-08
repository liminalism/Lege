#!/usr/bin/env bash
# usage: gate.sh <probeA.exe> <probeB.exe> <outdir> <pdf>...
# Renders every page of each pdf at scales 1.0/1.37/2.0, RGBA and Gray8,
# fresh backend, with both probes; prints per (file, scale, fmt) whether all
# page hashes match, and lists differing pages.
A=$1; B=$2; OUT=$3; shift 3
mkdir -p "$OUT"
total=0; same=0
for pdf in "$@"; do
  name=$(basename "$pdf" .pdf)
  for scale in 1.0 1.37 2.0; do
    for fmt in rgba gray8; do
      for side in A B; do
        exe=${!side}
        if [ $fmt = gray8 ]; then export PROBE_GRAY8=1; else unset PROBE_GRAY8; fi
        "$exe" run "$pdf" --scale $scale --hash --fresh-backend 2>/dev/null \
          | awk -F'\t' '!/^#/ {print $2"\t"$NF}' > "$OUT/$name.$scale.$fmt.$side.tsv"
      done
      unset PROBE_GRAY8
      n=$(wc -l < "$OUT/$name.$scale.$fmt.A.tsv")
      if [ "$n" -eq 0 ]; then echo "EMPTY $name $scale $fmt"; continue; fi
      total=$((total + n))
      d=$(diff "$OUT/$name.$scale.$fmt.A.tsv" "$OUT/$name.$scale.$fmt.B.tsv" | grep '^<' | cut -f1 | tr -d '< ' | tr '\n' ' ')
      if [ -z "$d" ]; then
        same=$((same + n)); echo "same  $name scale=$scale $fmt pages=$n"
      else
        echo "DIFF  $name scale=$scale $fmt pages=$n differing_pages: $d"
      fi
    done
  done
done
echo "renders identical: $same / $total"
