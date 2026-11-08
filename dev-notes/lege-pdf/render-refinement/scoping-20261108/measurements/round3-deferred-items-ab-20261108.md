# Round 3: timing of the deferred items (2026-11-08)

Paired A/B of `sessionbench` built at `7ea2f836` (A, the end of the local
round) against the working tree with A20, A7, A12 and A9 applied (B), through
`harness/ab.cmd` (P-core pinning, one Rayon thread, serial JPX), 9 interleaved
rounds each, Gray8 unless noted. The warm pass (page cache and image cache
already populated) cannot see a compile- or decode-side change, so the cold
first pass of a fresh session is the figure that matters; both are given.

Fixtures (all under `corpus/`, stdlib Python):

- `mkgrayjpx.py` → `gray_jpx.pdf`: a 2000x2800 8-bit gray scan-like page
  encoded by `jp2lam encode ... q60` (irreversible 9/7, 5 levels, 542 KB),
  drawn full-page, as a 2x window and minified: the A12 path
  (`reconstruct_grayscale_u8_into`, packed Gray8 output).
- `mkrepeat.py` → `repeat_image.pdf`: one 1200x1600 gray Flate image drawn 12
  times per page (page 2 through a `/CS0` resource name, which A7 must not
  cache; page 3 an RGB image): the A7 path.
- Controls: beans (JPX colour, no repeats), vogtle (RGB8).

## Cold first pass (`--pass 0`, one pass per run)

| Document | A | B | B/A median | B faster | Output |
|---|---|---|---|---|---|
| gray_jpx 150 dpi | 213.91 ms | 206.69 ms | **0.984** | 7/9 | identical |
| gray_jpx 300 dpi | 244.21 ms | 237.26 ms | **0.977** | 8/9 | identical |
| repeat_image 150 dpi | 429.25 ms | 270.26 ms | **0.615** | 9/9 | identical |
| beans 150 dpi | 38.73 ms | 40.51 ms | 1.025 | 3/9 | identical |
| vogtle 150 dpi RGB8 | 226.55 ms | 228.09 ms | 1.001 | 4/9 | identical |

## Warm third pass (`--pass 2`, three passes per run)

| Document | A | B | B/A median | B faster |
|---|---|---|---|---|
| gray_jpx 150 dpi | 46.27 ms | 45.87 ms | 1.007 | 4/9 |
| gray_jpx 300 dpi | 110.44 ms | 111.38 ms | 1.037 | 2/9 |
| repeat_image 150 dpi | 152.13 ms | 151.96 ms | 1.001 | 4/9 |
| beans 150 dpi | 26.67 ms | 25.50 ms | 0.956 | 7/9 |
| vogtle 150 dpi RGB8 | 123.12 ms | 128.62 ms | 1.047 | 3/9 |

## Reading

- **A7** removes 11 of 12 inflates and lowerings on the repeated-image pages:
  the cold compile of `repeat_image.pdf` drops to 0.62x. Page 2 (`/CS0`) is
  deliberately not cached and is part of that total.
- **A12** is worth 2 to 3% of a cold gray-JPX page render end to end at this
  size (the inverse DWT's finest level is one of several decode stages; the
  saving is the two plane-sized passes the planar path spent writing and
  reading back the full-resolution `f32` plane). The gain grows with the
  decode size relative to the rest of the page.
- **A20** has no measurable effect at these cache sizes (eviction rarely runs
  on a single document); it bounds the eviction cost when it does.
- **A9** does not show in `sessionbench`, which already opened one session;
  it removes one of two document parses per `lege` job.
- The warm pass is noise on every fixture, as expected: nothing in this round
  touches the rasteriser.
- Controls are flat, with the usual ±3% of this host.

Gate: `gate.sh`, probe at `7ea2f836` against the working tree, 16 documents
(the corpus and every fixture including `groups.pdf`, `gray_jpx.pdf` and
`repeat_image.pdf`), scales 1.0/1.37/2.0, RGBA and Gray8: **2,202 of 2,202
renders identical.**
