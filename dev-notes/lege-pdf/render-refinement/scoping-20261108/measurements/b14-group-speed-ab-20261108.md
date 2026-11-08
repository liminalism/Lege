# B14 speed A/B on group-heavy pages (2026-11-08)

B14 (`42e69c9a`) rasterises transparency groups and soft masks against the
device-plane width so content crossing the offscreen's right edge keeps its
coverage. The implementation log noted it had not been timed on group-heavy
pages. This is that measurement.

## Setup

- Binaries: `sessionbench` built at `7a21b4fb` (the commit before B14) and at
  `42e69c9a` (B14), `debug-fast` profile, each in its own worktree.
- Harness: `harness/ab.cmd` (P-core pinning, `RAYON_NUM_THREADS=1`,
  serial JPX, no image row parallelism), 9 interleaved rounds, pass 3 of 3
  (warm session), 150 dpi RGB8.
- Fixture: `corpus/mkgroups.py` → `groups.pdf`, three Letter pages:
  1. 400 isolated 40x40 pt transparency groups on a grid, each with a fill
     and a shallow stroke running from inside the BBox to past the right page
     edge (the B14 case: clipped group content crossing the offscreen's right
     boundary);
  2. the same grid under 400 luminosity soft masks whose content also runs
     past the mask BBox;
  3. 2000 tiny 8x8 pt groups with short paths (many small offscreens far
     from the right edge).
- Control: `D:\pdf-test\vogtle.pdf` (no groups crossing the right edge).

## Result

| Document | A (before B14) | B (B14) | B/A median | B faster | Output |
|---|---|---|---|---|---|
| groups.pdf, 3 pages | 92.72 ms | 92.67 ms | 1.005 | 4/9 | differs (B14's fix: edge coverage) |
| vogtle.pdf | 154.44 ms | 158.41 ms | 1.007 | 3/9 | identical |

Both ratios are inside the run-to-run noise of this host (±1-2%); B14 is
speed-neutral on 2,800 groups and 400 soft masks per document. The output
difference on `groups.pdf` is the intended one: every page has group content
crossing the offscreen's right edge, which B14 now paints with its true
coverage.
