# Integration scoping (worktree .../scratchpad/integrate, HEAD 16a635cc)
Base dazzling-planck; + gifted-pascal (bd4611f7); + jolly-ride (2a370a2b); + fix 16a635cc.
(a) Conflicts
- gifted: lege-pdf/render/crates/pdf-cli/tests/pipeline.rs ~534-585. dazzling added ink_width + horizontal_scaling_scales_glyph_shapes; gifted added embedded_type1_with_hostile_subr_index_still_renders at the same spot. Resolved: keep both (close first fn, then second). prepared.rs auto-merged.
- jolly: Cargo.lock ~3956 (pdf-render-cpu deps: foldhash 0.2.0 vs jp2lam). Keep both, sorted. Regenerate if in doubt.
- jolly: lege-pdf/render/CHANGELOG.md 13-78. gifted "Hardening 2026-11-08 (Type 1 fonts)" section vs jolly "Fixes, 2026-11-08". Kept both sections, blank line between.
- No other textual conflicts (prepared.rs, exec.rs, session.rs, jp2lam decode/mod.rs, jp2lam CHANGELOG auto-merged).
(b) Semantic
- Compile break: dazzling exec.rs test literal of CpuPreparedPage lacked jolly's new field decode_floor. Fixed (decode_floor: None) in 16a635cc.
- Not inspected in depth (compile+tests pass, no behavioural review): A5 Gray8 promotion vs promote_through_cache (prepared.rs ~2686, uses PROMOTION_CACHE_CODEC 0x80 keys); RenderSession RGB8 packing vs jolly session.rs decode reference; stale "keyed by footprint only for JPX" comments in prepared.rs (~574-577, 2412) already reworded by jolly. Recommend a manual read of promote_through_cache + with_rgb8_samples vs A5 and a changelog entry for dazzling's items.
(c) Build/test: cargo check (-p pdf-render-cpu pdf-content pdf-font pdf-image lege-pdf-read --tests) OK after fix. cargo test: 55 suites, 665 passed, 0 failed. Caveat: pdf-text build.rs digest check fails on a CRLF checkout; I re-checked out lege-pdf/render/crates/pdf-text/upstream with autocrlf=false to build (not committed). Not run: pdf-cli, lege (lege-process page_output_plan.rs from jolly was not compiled).
(d) Order: gifted -> jolly onto dazzling (or dazzling, gifted, jolly) all equivalent; expect same 3 conflicts + decode_floor fix. Then cargo check -p lege (page_output_plan.rs) and pdf-cli tests under LF.
(e) CHANGELOG: gifted and jolly both add top sections in lege-pdf/render/CHANGELOG.md (collide, trivially keep both); jolly also edits jp2lam CHANGELOG (auto-merged). Dazzling did not collide in the changelog.
