# MemorySafe

Governed memory infrastructure for AI agents.

MemorySafe is not primarily a retrieval system — it is **admission and eviction control** for
memory. Every memory that enters is assessed, a policy decides what happens to it, and the reason
is recorded:

```
assess  →  value, fragility, sensitivity, redundancy  (item)  +  capacity  (scope)
apply   →  retain | protect | replay | merge | forget
record  →  a structured, queryable reason for every decision
```

The lineage is continual-learning replay-buffer selection, generalised to agent memory. The
differentiator is not "we store memories" but "we decide, defensibly and auditably, which memories
are worth keeping and which are worth surfacing."

## Status

**Early. Under active development.** The core type layer is landing; the engine, the SQLite
backend, and the MCP/HTTP surfaces are specified but not yet built.

| Component | State |
|---|---|
| `memorysafe-core` — types, scope hierarchy, governance trait | in progress |
| `memorysafe-embed` — embedder trait, int8 quantization | planned |
| `memorysafe-backend` — `Backend` trait + conformance suite | planned |
| `memorysafe-backend-sqlite` — the open-source backend | planned |
| `memorysafe-policy` — `BaselinePolicy` | planned |
| `memorysafe-engine` — orchestration | planned |
| MCP server, HTTP API, CLI | planned |

## Design

The full design is in [`docs/superpowers/specs/`](docs/superpowers/specs/), and the
implementation plan for the engine and SQLite backend is in
[`docs/superpowers/plans/`](docs/superpowers/plans/). Both are written to be read — they record
the decisions and, more usefully, the alternatives that were rejected and why.

Some of the load-bearing choices:

- **Scope is `tenant → subject → namespace`.** Tenant is the isolation unit, subject the
  delete/export unit, namespace the budget unit.
- **Policies are pure; the engine performs all I/O.** Everything a policy needs — neighbours,
  capacity, corpus statistics, the clock — arrives through context structs. This makes policies
  testable against fixtures, makes audit-log replay against a new policy version possible, and
  means a closed-source policy has no I/O capability at all.
- **Hard filters run in the backend query, below the policy.** A policy can only ever narrow a
  candidate set, never widen it, so a bug in a scorer cannot become a data leak.
- **Audit rows never store item bodies** — only ids, content digests, and feature numbers.

## Building

```sh
cargo test --workspace --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Rust 1.97.1, edition 2024. The toolchain is pinned in `rust-toolchain.toml`.

## Licence

Apache-2.0.
