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
    memorysafe-policy/          # [In Progress] Baseline governance policy implementation
    memorysafe-engine/          # [In Progress] Pipeline orchestrator (remember/recall/maintain)
    memorysafe-mcp/             # [Planned] Model Context Protocol server (stdio & streamable HTTP)
    memorysafe-api/             # [Planned] REST API (axum)
    memorysafe-cli/             # [Planned] `msafe` command-line tool
    memorysafe-shadow/          # [Planned] Policy replay and shadow evaluation harness
```

### Crate Status

| Crate | Purpose | Status |
|---|---|---|
| [`memorysafe-core`](crates/memorysafe-core) | Core domain types (`Scope`, `MemoryItem`, `Assessment`, `Decision`, `AuditRecord`, `GovernancePolicy`). Zero I/O dependencies. | Complete (77 tests) |
| [`memorysafe-embed`](crates/memorysafe-embed) | Vector quantization (int8 SIMD), deterministic test embedder, optional static model embeddings (`model2vec-rs`). | Complete (14 tests) |
| [`memorysafe-backend`](crates/memorysafe-backend) | Unified `Backend` trait (CRUD, hybrid search, capacity, audit, export/import, purge) + test conformance suite. | Complete |
| [`memorysafe-backend-sqlite`](crates/memorysafe-backend-sqlite) | Single-tenant SQLite storage engine, connection pooling, and atomic transaction execution. | Active (Persistence, isolation, & atomicity conformance passing) |

---

## Quickstart (Rust API)

Here is how to initialize the SQLite backend, construct a scoped memory item, and commit it with an atomic audit trail:

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;
use time::OffsetDateTime;
use memorysafe_core::{
    Actor, AuditEvent, AuditRecord, ItemId, ItemRef,
    MemoryItem, Protection, Scope, SensitivityLevel, Source, SourceKind,
};
use memorysafe_backend::{Backend, ItemWrite, WriteTransaction};
use memorysafe_backend_sqlite::SqliteBackend;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Open the SQLite backend (stores one file per tenant in the target directory)
    let backend = SqliteBackend::open(PathBuf::from("./data/tenants"));

    // 2. Define a target scope: tenant -> subject -> namespace
    let scope = Scope::new("tenant_alpha", "user_42", "preferences")?;
    let now = OffsetDateTime::now_utc();

    // 3. Construct a memory item
    let item = MemoryItem {
        id: ItemId::new(),
        scope: scope.clone(),
        body: "User prefers concise code snippets with Rust 2024 edition syntax.".to_string(),
        kind: "preference".to_string(),
        source: Source {
            kind: SourceKind::Agent,
            id: Some("session-abc".to_string()),
        },
        occurred_at: Some(now),
        created_at: now,
        tags: vec!["coding".to_string(), "rust".to_string()],
        attrs: BTreeMap::new(),
        sensitivity: SensitivityLevel::Personal,
        ttl: None,
        protection: Protection::Normal,
        pending_embedding: false,
    };

    // 4. Create an audit record (audit stores content digests, never plaintext bodies)
    let audit = AuditRecord::new(
        scope.clone(),
        AuditEvent::Admitted,
        vec![ItemRef::from_item(&item)],
        Actor::system(),
        now,
    );

    // 5. Commit item write, evictions, and audit together atomically
    let mut txn = WriteTransaction::new(scope.clone(), audit);
    txn.upsert = Some(ItemWrite {
        item: item.clone(),
        vector: None, // Or attach an int8 QuantizedVector
    });

    let applied = backend.apply(txn).await?;
    println!("Committed memory item: {:?}", applied.item_id);
    println!("Audit record created: {}", applied.audit_id);

    // 6. Retrieve stored item
    if let Some(retrieved) = backend.get(&scope, &item.id).await? {
        println!("Retrieved body: {}", retrieved.body);
    }

    Ok(())
}
```

---

## Development & Verification

MemorySafe requires **Rust 1.97.1** or newer (Rust 2024 edition).

### Build

```bash
cargo build --workspace
```

### Run Tests

Run the full workspace test suite, including pure domain tests and SQLite backend conformance tests:

```bash
cargo test
```

### Run Conformance Suite Only

The backend conformance suite verifies that a storage backend correctly implements structural isolation, transactional atomicity, and audit guarantees:

```bash
cargo test -p memorysafe-backend-sqlite --test conformance
```

### Lints and Formatting

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

---

## License

This project is licensed under the [Apache License, Version 2.0](LICENSE).
