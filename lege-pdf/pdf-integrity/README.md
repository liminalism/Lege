# pdf-integrity

Read-only PDF integrity triage. This is its own Cargo workspace, nested at `lege-pdf/pdf-integrity` and excluded from the Lege root workspace so its dependency set and release cadence stay separate. License: Apache-2.0.

| Crate | Role |
|---|---|
| `pdf-forensics` | Report schema, findings, dispositions |
| `lege-pdf-audit` | Read-only `inspect` CLI over that model |

It consumes the in-tree renderer through path dependencies. The ledger under `.akr/` is the plan of record. `CLAUDE.md` is the agent protocol for this workspace. The original shape note is [../../dev-notes/lege-pdf/pdf-integrity-initial-plan-shape.md](../../dev-notes/lege-pdf/pdf-integrity-initial-plan-shape.md).

```sh
cargo check --manifest-path lege-pdf/pdf-integrity/Cargo.toml
```
