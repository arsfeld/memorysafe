# MemorySafe

**Governed memory for AI agents.** Every memory that enters is assessed, a policy decides what
happens to it, and the reason is recorded and queryable.

[![ci](https://github.com/arsfeld/memorysafe/actions/workflows/ci.yml/badge.svg)](https://github.com/arsfeld/memorysafe/actions/workflows/ci.yml)

> **Early — there is nothing to install yet.** The type layer, the storage trait and its
> conformance suite, and part of the SQLite backend are in the tree and green. The engine, the
> policy, and the MCP/HTTP/CLI surfaces are designed but not built. See [Status](#status) for
> exactly what exists.

---

## The problem

Most agent memory is a vector store with an append-only write path: everything the agent sees
gets embedded and kept, retrieval is nearest-neighbour, and the only thing resembling a policy is
a `top_k`. That works until it doesn't:

- **The corpus never stops growing.** Nothing decides what is not worth keeping, so cost and
  retrieval quality both degrade with age.
- **Recall is unaccountable.** When the agent surfaces the wrong thing, "it was similar" is the
  entire explanation available.
- **Deletion is a hope.** "Forget what I told you about my health" has no mechanism behind it
  beyond a filter someone remembered to write.
- **Sensitive and fragile pull in opposite directions.** A rare medical detail is both the most
  valuable thing to retain and the most dangerous thing to surface. A single "importance" score
  cannot represent that conflict, let alone explain it.

## What MemorySafe does

MemorySafe is not primarily a retrieval system — it is **admission and eviction control** for
memory:

```
assess  →  value, fragility, sensitivity, redundancy  (item)  +  capacity  (scope)
apply   →  retain | protect | replay | merge | forget
record  →  a structured, queryable reason for every decision
```

The same policy runs on the read path. A recall is a *composition* under an explicit budget —
which items earned a context slot, which were left out, and why — and that composition is
recorded too.

Reasons are machine-readable, not prose: every decision carries a `ReasonCode`
(`NovelContent`, `HighRedundancy`, `CapacityPressure`, `SensitivityCap`, `ProtectedFragile`,
`DiversityCut`, `BudgetExhausted`, …), a human-readable detail string, and the feature values the
policy actually scored on. You can query the audit log for *why* a memory was dropped, and get an
answer with numbers in it.

The lineage is continual-learning replay-buffer selection, generalised to agent memory. The
differentiator is not "we store memories" but "we decide, defensibly and auditably, which
memories are worth keeping and which are worth surfacing."

## Who it is for

Developers building agent products, where memory belongs to *their* end-users. Three intended
ways in — an in-process Rust crate, an MCP server, and an HTTP API — over one engine, so a laptop
and a production deployment run the same governance.

## Design principles

These are the choices the rest of the system is built to protect.

- **Scope is `tenant → subject → namespace`.** Tenant is the isolation unit, subject the
  delete/export unit, namespace the budget unit. Multi-tenancy is not retrofittable, so it is
  present from the first type.

- **Isolation is structural, not a code invariant.** The SQLite backend is one database file per
  tenant. Backup is `cp`, tenant deletion is `rm`, per-tenant encryption is a key per file. A
  missing `WHERE tenant_id = ?` cannot leak across tenants because there is no shared table to
  leak from.

- **Policies are pure; the engine performs all I/O.** Everything a policy needs — neighbours,
  capacity, corpus statistics, the clock — arrives through context structs. Policies are testable
  against fixtures, audit logs can be replayed against a new policy version, and a third-party
  policy has no I/O capability at all.

- **Hard filters run in the backend query, below the policy.** A policy can only ever narrow a
  candidate set, never widen it, so a bug in a scorer cannot become a data leak.

- **No model in the write path.** Memories are caller-authored discrete items, embedded locally.
  No LLM call, no network, predictable cost, works offline.

- **Fragility and sensitivity are separate axes.** Collapsing them into one "importance" number
  makes the conflicts invisible and the audit unexplainable.

- **Audit rows never store item bodies** — only ids, content digests, and feature numbers. The
  log stays safe to retain after the memory itself is gone.

- **Your data stays yours.** Portable export and import are v1 features, not a later migration
  tool.

## Status

**Under active development.** Nothing is published to crates.io and the APIs change without
notice.

The table below is accurate as of commit
[`4b2ee19`](https://github.com/arsfeld/memorysafe/commit/4b2ee19) — pinned deliberately, because
"currently" is not a fact anyone can check later.

| Crate | State at `4b2ee19` |
|---|---|
| `memorysafe-core` — ids, scope, items, assessments, decisions, audit types, and the `GovernancePolicy` trait | **landed**, no I/O dependencies (enforced in CI) |
| `memorysafe-embed` — `Embedder` trait, int8 quantization, deterministic test embedder, optional Model2Vec | **landed**, reaches no network stack (enforced in CI) |
| `memorysafe-backend` — the `Backend` trait, query/write types, and the 50-test conformance suite | **landed**, suite frozen |
| `memorysafe-backend-sqlite` — the SQLite backend, one database file per tenant | **partial** — per-tenant files, schema, pooled connections, items, audit, aggregates. Vector and keyword retrieval, merge, capacity accounting, idempotency, purge, and export/import are not implemented yet |
| `memorysafe-policy` — the baseline policy | not started |
| `memorysafe-engine` — orchestration, the component that ties the above together | not started |
| MCP server, HTTP API, CLI | not started |

Verified at that commit: `cargo fmt --all -- --check`, `cargo clippy --all-targets
--all-features -- -D warnings`, and `cargo test --workspace` all pass — **169 unit tests** (core
77, embed 14, backend 37, sqlite 41), plus **7 of the 50-test conformance suite** executing
against the SQLite backend. The other 43 compile and do not yet run: each is bound as the method
it exercises lands. Seven passing is seven, not a conformant backend.

**What this means in practice:** you cannot store or recall a memory yet. There is no engine to
call and no policy to decide anything. What you *can* do is read the design, read the conformance
suite to see what a backend is required to guarantee, and implement `Backend` against it.

## Building

```sh
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Rust 1.97.1, edition 2024, pinned in `rust-toolchain.toml`. `unsafe_code` is forbidden across the
workspace.

Optional features:

- `memorysafe-embed/model2vec` — a real static embedder (Model2Vec). Off by default. It pulls
  `model2vec-rs` → `onig`, which builds native code, so enabling it (or building with
  `--all-features`) needs a C++ toolchain and `libstdc++` available. With the feature off,
  `DeterministicEmbedder` covers tests and development.

## Implementing a backend

`Backend` is one trait covering persistence *and* retrieval — deliberately not split into
`Store` + `Index`, because databases that search inside themselves would have to fake the split.

The contract is executable. `memorysafe_backend::conformance` ships 50 tests that any
implementation must pass unmodified, covering tenant/subject/namespace isolation, transaction
atomicity, idempotent replay, retrieval semantics and ranking, capacity accounting under
concurrency, audit paging and cursors, subject purge, and byte-exact export/import round trips:

```rust
use memorysafe_backend::conformance::{BackendFactory, run_conformance_suite};

struct MyFactory;

impl BackendFactory for MyFactory {
    type B = MyBackend;
    async fn create(&self) -> Self::B { /* a fresh, empty backend per call */ }
}

#[tokio::test]
async fn my_backend_is_conformant() {
    run_conformance_suite(&MyFactory).await;
}
```

The suite is measured against a null backend that stores nothing and enforces nothing, and every
test's verdict is on record. A test that *passes* the null backend has to justify itself by
naming a sibling that fails it — so no test can quietly decay into one that asserts nothing your
implementation could fail.

## Design documents

The full design is in [`docs/superpowers/specs/`](docs/superpowers/specs/); the implementation
plans are in [`docs/superpowers/plans/`](docs/superpowers/plans/). They are written to be read,
and they record the rejected alternatives and the reasons — usually the more useful half.

## Licence

Apache-2.0.
