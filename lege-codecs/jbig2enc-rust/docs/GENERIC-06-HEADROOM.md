# Bounded generic headroom pass for 0.6 — 2026-11-02

Neither tested hypothesis produced an unambiguous improvement suitable for this
release. Both experiments were reverted. The previously verified blank-span
batching and delayed transition loads remain unchanged, byte-for-byte.

The user specified that the machine was under partial load and that it could not
be changed. No load, governor or global-system settings were changed. This was
one implementation per hypothesis, without further tuning or combinations.

## Experiments

Candidate/previous ratios below are medians of five paired process runs per
input. Lower is better. Each process encodes 32 timed rounds and one warm-up.
The same CPU 0 and seeded interleaved order are used. Perf counts the complete
process, while the internal wall timer excludes loading/output verification.

| Hypothesis | Mixed17 wall | Mixed17 cycles | Noise wall | Noise cycles | Disposition |
| --- | ---: | ---: | ---: | ---: | --- |
| Cache Qe with each bitmap context | 0.9881 | 1.0064 | 1.0163 | 1.0172 | Rejected |
| Batch renormalization shifts | 1.0026 | 0.9815 | 1.0626 | 1.0385 | Rejected |

**Cached probability data:** a packed 32-bit context stored its table index and
Qe, reducing the bitmap context bank from 512 to 256 KiB and removing the hot
dependent Qe lookup. State changes refreshed both fields. Mixed instructions
fell 5.65%, but cycles increased in every pair. Noise instructions increased
5.27%, with a consistent small cycle regression. A roughly 1.2% mixed wall-time
gain does not justify that tradeoff under the stated release criterion.

**Batched renormalization:** leading-zero count determined the total required
shift, applied in chunks ending exactly at each BYTEOUT boundary. Mixed cycles
fell about 1.85%, but median wall time was essentially neutral. Noise instructions
increased 18.24%, cycles 3.85%, and median wall time 6.26%. One noise pair was
faster; no pairs were removed. This is not a consistent broad win.

Both candidates passed the independent coordinate/context cases, million-pixel
blank/sparse case and original 40-stream byte manifest. Separate untimed checks
also compared mixed17 raw payloads to the prior worker and pinned C. The final
source hash equals the starting source hash. There is no new arithmetic code,
API, dependency, format or size change from this pass.

The existing fixed Ghostscript inputs and pinned C reference from
[the prior report](GENERIC-PERF-FOLLOWUP.md) are reused. The baseline is the
completed blank-batching release candidate, not the older pre-optimization
kernel. Counter files, worker/source hashes, all pairs, candidate patches and
the probe script are retained in
[the measurement directory](benchmarks/2026-11-02-06-headroom). CPU frequency and
unrelated system activity remain uncontrolled; the after-probe load snapshot is
context, not a reconstruction of the load during each pair.

The conservative result is to ship the earlier gains and defer these hypotheses.
[Final validation](validation/2026-11-02-06-headroom) covers the restored source
and release-note packaging.
