# Cloud continuation — JBIG2 0.6

Portable AKR handoff export. The original packet/session files are local and ignored;
use this briefing and the live planning key
`lege-ecosystem.work.jbig2-06-cloud-readme-and-preflight` in a fresh clone.
Continue from the checked-out HEAD, not the older capsule snapshot revision.
The user will push the local commit; no publication is authorized.

Post-checkpoint note: generated AKR views were refreshed after the source commit.
Historical unmatched-path and command-lint warnings remain; strict CLI checking
can fail on promoted command warnings. Codec correctness checks pass independently.
Do not treat those historical lint warnings as a renewed licensing gate.

WORKER PACKET wk-6908fe54c54a
role         cloud 0.6 release preflight and README editor
cut          2026-11-02 by codex
inherits     sx-f8d22f5e36d2
workspace    95b57137 Make renderer and processor lifecycles explicit and migrate GUI to liminalUI — 811 dirty paths (exact)

YOU ARE
  You are continuing work another agent began. Its findings and its coverage are included so you do not pay for them twice. They are not authority: where they and the code disagree, the code decides.

ORIGINAL REQUEST (from the session, verbatim)
  im going to continue this in a cloud session that wont have system load. commit everything so i can push and get the cloud session working. write me a short handoff prompt for that agent. i am also interested in rewriting the readme for the new version, especially to underemphasize competitive timing as much, even as it has improved.

YOUR ASSIGNMENT (narrowed from the above)
  Continue the proposed jbig2-06-cloud-readme-and-preflight work. Prioritize a new-version README that leads with capabilities and usage, underemphasizing competitive timings. Optionally recheck the two documented rejected optimizations under an unloaded cloud setup, bounded to clear wins. Validate and keep release notes accurate; do not publish.

SCOPE
  lege-codecs/jbig2enc-rust/**
  Look wherever this reaches. Nothing here narrows it to what another agent
  already examined.

PROJECT lege-ecosystem (pc-cb02f469f774)
  toolchain    rust (cargo), edition 2024
  namespaces   lege-ecosystem, lege-ecosystem-perf
  layout
    assets/
    docs/  documentation
    freya-main/
    lege-android/
    lege-codecs/
    lege-document-ocr/
    lege-gpu/
    lege-misc/
    lege-ocr/
    lege-pdf/
    lege-process/
    scripts/  tooling
    sources/  registered source library (immutable)
    tmp/
    to-do-plans/
  commands
    cargo build
    cargo test
    cargo clippy --all-targets
    cargo fmt --all
  authoritative
    .akr/records/**
    sources/** (immutable)
  generated (never hand-edit)
    docs/generated/**
    .akr/cache/**
    .akr/akr.lock
  instructions
    AGENTS.md
    CLAUDE.md

PROJECT STATE
  AKR SESSION HEAD
  snapshot    95b57137 Make renderer and processor lifecycles explicit and migrate GUI to liminalUI — working_tree
  ledger      sha256:e6a980eb0eb9c1694cc02c7b6837da81c6c2051256b2902a26b91b6cca14decc (validated overlay)
  namespaces  lege-ecosystem, lege-ecosystem-perf
  recent AKR  b9fed743 — chore: Log the two AKR frictions hit while healing the ledger
  focus
    verified @lege-ecosystem.papercut.akr-revise-from-body-on-a-completed-work-record/1 — `akr revise --from <body>` on a completed work record silently reset it…
    verified @lege-ecosystem.papercut.healing-an-inherited-akr-t023-on-an-evidence/1 — Healing an inherited AKR-T023 on an evidence record that another record…
  outstanding  12 live planning heads
    active   @lege-ecosystem.work.pixelkit-gui-1-5-2/5 — Replace the Freya processor GUI with PixelKit in 1.5.2
    active   @lege-ecosystem.work.preservation-mode/1 — Preservation mode: an MRC document beside the e-ink preparation mode
    active   @lege-ecosystem.work.thorough-ppocrv6-performance-and-output-preflight/3 — Audit thorough PP-OCRv6 throughput without sacrificing accuracy
    proposed @lege-ecosystem.encode-quality.improve-photographic-rate-control/1 — Rework jp2lam lossy rate control for perceptual, resolution-stable photographic quality
    proposed @lege-ecosystem.work.glyph-font-text-output/1 — Improve glyph-font quality and long-book growth
    proposed @lege-ecosystem.work.jbig2-06-cloud-readme-and-preflight/1 — Prepare the JBIG2 0.6 README and conservative cloud preflight
    proposed @lege-ecosystem.work.jp2lam-jpeg-2000-decoder-optimization-plan/1 — jp2lam decoder optimisation: the remaining tail
    proposed @lege-ecosystem.work.open-source-monorepo-relaunch/1 — Relaunch Lege as the canonical open-source codec monorepo
    proposed @lege-ecosystem.work.pagepress-pdf-import-followup/1 — PagePress followup: PDF page import via lege-pdf-read
    proposed @lege-ecosystem.work.pdf-renderer-follow-ups-outside-pure-codec/1 — PDF renderer follow-ups (outside pure codec)
    proposed @lege-ecosystem.work.pipeline-memory-follow-ups/1 — Deferred pipeline memory follow-ups
    proposed @lege-ecosystem.work.renderer-efficiency-deferrals/1 — Disposition the deferred renderer efficiency findings
  attention
    at-risk @lege-ecosystem.assessment.glyph-outlines-rasterized-into-a-jbig2-layer/1 depth 2
    at-risk @lege-ecosystem.decision.jbig2-is-the-default-pdf-text-format-again/1 depth 1
    at-risk @lege-ecosystem.decision.truetyping-leaves-bilevel-sources-alone/1 depth 2
    at-risk @lege-ecosystem.evidence.glyphfont-speck-filter-crusades-20260906/1 depth 1
    at-risk @lege-ecosystem.evidence.truetyping-skips-a-jbig2-source-20260906/1 depth 3
    open @lege-ecosystem.question.cups-may-double-apply-the-hardware-margin-to-a-composed-sheet/1 — Does CUPS scale a composed sheet again, shrinking the margin we already left?
    stale @lege-ecosystem.observation.doclayout-m-underdetects-text-and-images/3
    stale @lege-ecosystem.observation.grayscale-mode-makes-truetyping-smaller-and-cleaner/2
    stale @lege-ecosystem.observation.truetyping-dictionary-lock-serialized-every-page/2
    stale @lege-ecosystem.observation.truetyping-halftone-review-fixes-20260905/2
    stale @lege-ecosystem.observation.truetyping-text-identity-moved-letters-off-their-baseline/2
    stale @lege-ecosystem.observation.windows-ocr-200-page-benchmark-20260814/3
    unsatisfied @lege-ecosystem.work.jbig2-06-cloud-readme-and-preflight/1#preflight — no evidence
    unsatisfied @lege-ecosystem.work.jbig2-06-cloud-readme-and-preflight/1#readme — no evidence
    unsatisfied @lege-ecosystem.work.jbig2-06-cloud-readme-and-preflight/1#release-notes — no evidence
    unsatisfied @lege-ecosystem.work.open-source-monorepo-relaunch/1#canonical-codec-source — no evidence
    unsatisfied @lege-ecosystem.work.open-source-monorepo-relaunch/1#integrated-djvu-api — no evidence
    unsatisfied @lege-ecosystem.work.open-source-monorepo-relaunch/1#license-consistency — no evidence
    unsatisfied @lege-ecosystem.work.open-source-monorepo-relaunch/1#public-cutover — no evidence
    unsatisfied @lege-ecosystem.work.open-source-monorepo-relaunch/1#sanitized-public-tree — no evidence
    unsatisfied @lege-ecosystem.work.open-source-monorepo-relaunch/1#verified-model-bootstrap — no evidence
    unsatisfied @lege-ecosystem.work.pixelkit-gui-1-5-2/5#controls — no evidence
  overlay      89 added, 3 revised, 78 transitions, 811 dirty paths

GOVERNING CONTEXT
  goal  lege-ecosystem.work.jbig2-06-cloud-readme-and-preflight

CONSTRAINTS
  Keep optimization bounded for 0.6; retain only unambiguous byte-identical gains. README should underemphasize competitive timing.
  MIT project / retained Apache arithmetic notices; manifest MIT AND Apache-2.0. Root/app/DjVu AGPL unchanged; no renewed licensing gate.
  No publication without a separate request. Local private corpus, external oracles and scratch workers will not transfer to cloud.

ALREADY ESTABLISHED
  261 functional passes, 8 intentional ignores, 2 known template-one-prefix filters; 7 release passes/1ignore; package verification passes.
  Current arithmetic restored exactly after rejecting cached probability data and chunked renormalization under partial load.
  evidence  @lege-ecosystem.evidence.jbig2-06-headroom-disposition-20261102/1
  evidence  @lege-ecosystem.evidence.jbig2-06-restored-validation-20261102/1
  evidence  @lege-ecosystem.evidence.jbig2-crates-release-notes-20261102/1
  artifact  lege-codecs/jbig2enc-rust/docs/GENERIC-06-HEADROOM.md
  artifact  lege-codecs/jbig2enc-rust/docs/GENERIC-PERF-FOLLOWUP.md
  artifact  lege-codecs/jbig2enc-rust/CHANGELOG.md

ALSO ESTABLISHED
  Use the checked-out cloud HEAD. This packet was cut before the local commit; do not check out the older snapshot commit shown in capsule metadata.
  The current coder is the final verified 0.6 baseline; both last-pass candidates were reverted. Candidate patches, counter CSVs and dispositions are in docs/benchmarks/2026-11-02-06-headroom.
  Default tests use three original MIT PBMs. Legacy external PBMs and raw book corpus are ignored/local; do not assume documented /tmp or /run/media paths exist.
  crates.io has no per-version release-note field. Draft CHANGELOG.md plus README summary is the selected workflow; RFC3779 remains open as checked2026-11-02.

COMMANDS FOR THIS ASSIGNMENT
  cargo test --manifest-path lege-codecs/jbig2enc-rust/Cargo.toml --features parallel --lib --tests -- --skip grtemplate1_tpgron (configure JBIG2DEC first)
  cargo fmt --manifest-path lege-codecs/jbig2enc-rust/Cargo.toml --check
  cargo package --manifest-path lege-codecs/jbig2enc-rust/Cargo.toml --allow-dirty

DO NOT REPEAT
  Do not redo the A-G roadmap or reopen the licensing gate.
  Do not repeat local-load benchmark numbers as cloud or whole-application results.
  Do not chase marginal optimizations or introduce hidden threads.

RETURN
  A capability-focused README and reviewed draft changelog.
  Explicit cloud validation results and conservative disposition of any fresh optimization measurements.

WORKER NOTES (non-authoritative: what the parent thought, not what it
established)
  (none recorded)

When you are done: akr handoff result wk-6908fe54c54a --finding ... --read ...
