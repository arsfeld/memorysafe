# MemorySafe

**Governed memory infrastructure for AI agents.**

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.97.1%20(2024%20edition)-orange.svg)](rust-toolchain.toml)

MemorySafe is an embeddable, governed memory engine for developers building AI agents.

Conventional vector stores act as passive, unbounded dumps: memories accumulate without limit, near-duplicates proliferate, context windows fill with stale facts, and sensitive details enter prompts without boundary.

MemorySafe treats memory not as a passive storage bucket, but as an **admission, eviction, and working-set control problem**. Every memory submitted is assessed for value, fragility, sensitivity, and redundancy. A policy decides what to keep, merge, or reject under explicit capacity budgets, and a privacy-preserving audit trail records every decision.

---

## Key Guarantees

* **No LLM in the Write Path:** Fast, deterministic, and offline-capable. Memories are embedded and governed in-process without network calls, external API costs, or LLM latency.
* **Pure Governance Policies:** Policy logic has zero I/O capability. The engine gathers necessary context (nearest neighbours, scope statistics, capacity pressure), feeds it into the policy, validates the returned decision, and commits writes atomically.
* **Structural Tenant Isolation:** In the SQLite backend, each tenant is stored in an independent database file (`<tenant>.db`). Multi-tenant isolation is enforced at the filesystem level, not via filter clauses in queries. Tenant backup is `cp`; tenant deletion is `rm`.
* **Privacy-First Auditability:** The audit log records item IDs, BLAKE3 content digests, feature scores, and reason codes—**never** plaintext memory bodies. Audit trails remain fully queryable for compliance without exposing sensitive data.
* **Deterministic Testability:** Includes a built-in `DeterministicEmbedder` and a comprehensive backend conformance suite, enabling reproducible testing of memory lifecycles without external model downloads or network dependencies.

---

## Core Lifecycle

```
WRITE:    item  ──►  assess  ──►  admit / merge / reject  ──►  atomic commit + audit
READ:    query  ──►  retrieve candidates  ──►  compose working set  ──►  record recall
CLEANUP: maintain  ──►  decay & re-score  ──►  capacity reclaim / TTL expiry
```

1. **Assess (`item`):**
   - **Value:** Weighted combination of content specificity, source authority, caller weighting, and recency.
   - **Fragility:** Identifies atypical, rare, or hard-to-relearn memories based on embedding-space neighbourhood sparsity.
   - **Sensitivity:** Pattern detection (credentials, IDs, health, financial markers) combined with caller-specified sensitivity hints.
   - **Redundancy:** Cosine similarity against existing neighbours in scope to identify merge targets or exact duplicates.
2. **Admit (`capacity`):**
   - Evaluates budget pressure for the target namespace.
   - Decides whether to `Retain`, `Merge`, or `Reject`.
   - Evicts lower-value, non-fragile items under capacity pressure while strictly respecting `Pinned` and time-boxed `Protected` guarantees.
3. **Compose (`working set`):**
   - Assembles a governed working set under strict token and item budgets using hybrid retrieval (vector similarity + keyword search) and diversity ranking.
   - Allocates dedicated replay quotas for fragile or long-unaccessed memories.
4. **Audit (`evidence`):**
   - Produces an immutable, structured record of every write, eviction, merge, and recall.

---

## Scoping Hierarchy

MemorySafe enforces a three-level hierarchy for all stored memories:

```
Tenant (TenantId)
  └── Subject (SubjectId)
        └── Namespace (Namespace)
```

| Scope Level | Purpose | Invariant |
|---|---|---|
| **Tenant** | Isolation boundary (customer / organization) | Stored in dedicated database files (`<tenant>.db`). No cross-tenant access. |
| **Subject** | Entity / end-user boundary | Unit of GDPR erasure (`purge_subject`) and portable export/import. |
| **Namespace** | Context / domain partition (e.g. `chat`, `code`, `prefs`) | Unit of capacity budgeting (`max_items`, `max_bytes`) and default retrieval. |

Scope identifiers must be lowercase ASCII strings containing only alphanumerics, `-`, `_`, or `.`, up to 240 bytes (to guarantee valid filenames across operating systems).

---

## Workspace Architecture

MemorySafe is structured as a modular Cargo workspace:

```
memorysafe/
  crates/
    memorysafe-core/            # Pure types, IDs, scoping, item models, and trait definitions
    memorysafe-embed/           # Int8 quantization, DeterministicEmbedder, model2vec integration
    memorysafe-backend/         # Backend trait contract and reusable conformance suite
    memorysafe-backend-sqlite/  # Single-tenant SQLite backend with WAL mode and atomic writes
    memorysafe-policy/          # Baseline governance policy implementation (assess, admit, compose, maintain)
    memorysafe-engine/          # Pipeline orchestrator (remember, recall, maintain, export/import)
    memorysafe-auth/            # Tenant-scoped API keys, secret hashing, and scope validation
    memorysafe-mcp/             # [Planned] Model Context Protocol server (stdio & streamable HTTP)
    memorysafe-api/             # [Planned] REST API (axum)
    memorysafe-cli/             # [Planned] `msafe` command-line tool
    memorysafe-shadow/          # [Planned] Policy replay and shadow evaluation harness
```

### Crate Status

| Crate | Purpose | Status |
|---|---|---|
| [`memorysafe-core`](crates/memorysafe-core) | Core domain types (`Scope`, `MemoryItem`, `Assessment`, `Decision`, `AuditRecord`, `GovernancePolicy`). Zero I/O dependencies. | Complete (80 tests) |
| [`memorysafe-embed`](crates/memorysafe-embed) | Vector quantization (int8 SIMD), deterministic test embedder, optional static model embeddings (`model2vec-rs`). | Complete (17 tests) |
| [`memorysafe-backend`](crates/memorysafe-backend) | Unified `Backend` trait (CRUD, hybrid search, capacity, audit, export/import, purge) + test conformance suite. | Complete (41 tests, 50-test conformance contract) |
| [`memorysafe-backend-sqlite`](crates/memorysafe-backend-sqlite) | Single-tenant SQLite storage engine, connection pooling, FTS5 + exact vector search, and atomic transactions. | Complete (104 unit tests, full conformance suite passing) |
| [`memorysafe-policy`](crates/memorysafe-policy) | `BaselinePolicy` implementation: value scoring, corpus-calibrated fragility, pattern-based sensitivity detection, redundancy/merge classification, MMR diversity working set composition, and background maintenance. | Complete (152 tests) |
| [`memorysafe-engine`](crates/memorysafe-engine) | Pipeline orchestrator (`Engine`): `remember`, `recall`, `maintain`, content-addressed embedding cache, policy validation, panic safety, portable ndjson export/import, audit retention profiles, and re-embedding migrations. | Complete (157 tests) |
| [`memorysafe-auth`](crates/memorysafe-auth) | Tenant-scoped API keys, secret hashing, and scope authorization across tenant boundaries. | Complete (14 tests) |

---

## Quickstart (Rust API)

Here is how to initialize the storage engine, embedder, and baseline policy into an `Engine`, submit a memory through the governance pipeline, and recall a governed working set:

```rust
use std::path::PathBuf;
use std::sync::Arc;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize the SQLite backend, embedder, and baseline governance policy
    let backend = Arc::new(SqliteBackend::open(PathBuf::from("./data/tenants")));
    let embedder = Arc::new(DeterministicEmbedder::new(256));
    let policy = Arc::new(BaselinePolicy::default());

    // 2. Instantiate the orchestrating Engine
    let engine = Engine::new(EngineConfig::new(backend, embedder, policy));

    // 3. Define a target scope: tenant -> subject -> namespace
    let scope = Scope::new("tenant_alpha", "user_42", "preferences")?;

    // 4. Remember a memory item (assessed, governed, and committed atomically with audit)
    let outcome = engine
        .remember(RememberRequest::new(
            scope.clone(),
            "User prefers concise code snippets with Rust 2024 edition syntax.",
        ))
        .await?;

    match outcome.action {
        Action::Retain { .. } => {
            println!(
                "Admitted memory item {:?} (audit record: {})",
                outcome.item_id, outcome.audit_id
            );
        }
        Action::Merge { target, .. } => {
            println!(
                "Merged into existing item {target} (audit record: {})",
                outcome.audit_id
            );
        }
        Action::Reject => {
            println!(
                "Rejected by governance policy (audit record: {})",
                outcome.audit_id
            );
        }
    }

    // 5. Governed recall with MMR diversity, replay quotas, and budget limits
    let working_set = engine
        .recall(RecallRequest {
            scope: scope.clone(),
            query: Some("coding preferences".to_string()),
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            mode: RecallMode::WorkingSet,
            budget: RecallBudget::default(),
            sensitivity_ceiling: SensitivityLevel::Restricted,
        })
        .await?;

    for item in &working_set.items {
        println!("Recalled: {} (reason: {:?})", item.item.body, item.reason.code);
    }

    Ok(())
}
```

### Low-Level Storage Access

For custom storage backend implementations or low-level transaction control, `SqliteBackend` also directly satisfies the `Backend` contract from `memorysafe-backend` with atomic `apply(WriteTransaction)` execution:

```rust
use std::path::PathBuf;
use memorysafe_backend_sqlite::SqliteBackend;

// Direct atomic write transactions: items, evictions, and audit records commit together.
let backend = SqliteBackend::open(PathBuf::from("./data/tenants"));
```

---

## Development & Verification

MemorySafe requires **Rust 1.97.1** or newer (Rust 2024 edition).

### Build

```bash
cargo build --workspace
cargo build --workspace --all-features
```

### Run Tests

Run the full workspace test suite, including pure domain tests, policy scoring tests, engine integration tests, and SQLite backend conformance tests:

```bash
cargo test --workspace --all-features
```

### Run Conformance Suite Only

The backend conformance suite verifies that a storage backend correctly implements structural isolation, transactional atomicity, and audit guarantees:

```bash
cargo test -p memorysafe-backend-sqlite --test conformance
```

### Lints and Formatting

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

---

## License

This project is licensed under the [Apache License, Version 2.0](LICENSE).
