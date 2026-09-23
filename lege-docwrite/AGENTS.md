# Agent protocol

This is lege-docwrite, a nested Cargo workspace inside the Lege-ecosystem
checkout (`Lege-ecosystem/lege-docwrite/`). It is tracked by the parent git
repository but has its own AKR ledger and codegraph index.

Build from here with `cargo check --all-targets`, or from the ecosystem root
with `cargo docwrite-check`. The pixelkit crates are expected at
`../../pixelkit` relative to this directory.

## Project knowledge (AKR)

Durable project knowledge lives in `.akr/` as typed records, not in Markdown.
`docs/generated/` is build output. Follow this protocol.

**Before starting any task**
1. If you know the exact planning key, call `knowledge.context` with that key and the
   paths you expect to touch.
2. Otherwise call `knowledge.start` with the task and paths. Read its session head, pick
   a live candidate (or an explicitly relevant proposal), then call `knowledge.context`
   with that exact key.
3. Read context bundles in full. Contradictions and staleness warnings are always
   included and are never noise.

**While working**
- Look things up with `knowledge.get`; find them with `knowledge.search`.
  Search ranks results; it never grants authority. A record's standing comes from its
  state, its scope, and its relations.
- Scratch notes go in `.agent/scratch/`. Nobody reviews them and nothing depends on them
  — but **nothing empties it either**. It is a gitignored directory inside the repository,
  not the OS temp directory and not `target/`, so it survives every session and grows
  until somebody deletes it by hand. Before handing work back, run `akr scratch prune`,
  and `akr scratch keep <name> --reason "<why>"` for anything the next session needs.
  `akr check` reports the total; `akr check --scratch-clean` fails when anything prunable
  is left.

**When something becomes durable**
- New knowledge: `knowledge.propose`. Observations need `observed_at` and, if they can
  go out of date, `watches`.
- Changed knowledge: `knowledge.revise`. Never edit a `.akr` file directly, and never
  edit a record that is not `proposed`.
- Replacing a plan: `knowledge.supersede`, with a disposition for every unfinished
  child. The tool will list them; answer each one.
- Finishing work: record what you observed with `knowledge.evidence_add`, then
  `knowledge.complete` with evidence for every acceptance check. Evidence records
  state what was observed; they never state what they verify.
- Unsure what a kind requires? `akr explain <kind>` prints its schema.

**Papercuts**
- When you hit a small friction while working — a tool call that missed and had to be
  retried, a confusing or undocumented setup step, a flaky command, a stale cache, a
  misleading error, a non-obvious gotcha — log it with `knowledge.papercut` (or
  `akr papercut -m <agent> "message"`). One or two sentences: what you were doing,
  what got in the way (a guess at the cause/fix is a bonus). Do this proactively, in
  the moment, even though none of these are blocking — logged together they show where
  the project needs sanding down. This is distinct from durable records (knowledge) and
  from `.agent/scratch/` (working notes).

**Never**
- Never edit `docs/generated/` — it is regenerated and CI checks it.
- Never read `.akr/cache/` — it is a private cache.
- Never delete a record. Move it to a terminal state instead.

**Before handing back**
- `knowledge.validate`. If it reports diagnostics, fix them or say so explicitly.
