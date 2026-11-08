"""ppmdiff.py A.ppm B.ppm: differing pixels, max delta, and how many lie
on 1-px edge lines (columns/rows holding >= 4 diffs whose neighbours on the
perpendicular side do not differ)."""
import sys
def load(p):
    d = open(p, 'rb').read()
    parts = d.split(maxsplit=4)
    assert parts[0] in (b'P6', b'P5'), parts[0]
    w, h, mx = int(parts[1]), int(parts[2]), int(parts[3])
    ch = 3 if parts[0] == b'P6' else 1
    return w, h, ch, parts[4]
w, h, ch, a = load(sys.argv[1]); _, _, _, b = load(sys.argv[2])
diffs = {}
maxd = 0
for i in range(0, len(a), ch):
    if a[i:i+ch] != b[i:i+ch]:
        px = i // ch
        d = max(abs(a[i+k] - b[i+k]) for k in range(ch))
        diffs[(px % w, px // w)] = d
        maxd = max(maxd, d)
from collections import Counter
cols = Counter(x for x, y in diffs); rows = Counter(y for x, y in diffs)
edge_cols = {x for x, c in cols.items() if c >= 4}
edge_rows = {y for y, c in rows.items() if c >= 4}
# a ring line is one pixel thick: no diff at both x-1 and x+1 (cols) etc.
unexplained = [(x, y) for (x, y) in diffs if x not in edge_cols and y not in edge_rows]
thick = [(x, y) for (x, y) in diffs if (x-1, y) in diffs and (x+1, y) in diffs and (x, y-1) in diffs and (x, y+1) in diffs]
print(f"{sys.argv[2]}: {w}x{h} diff_px={len(diffs)} max_delta={maxd} edge_cols={sorted(edge_cols)[:12]} edge_rows={sorted(edge_rows)[:12]} unexplained={len(unexplained)} {unexplained[:6]} interior_like={len(thick)}")
