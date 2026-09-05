# MemorySafe Engine + SQLite Backend — Implementation Plan (Plan 1 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a working in-process governed-memory engine — assess, admit, recall, maintain — backed by SQLite, callable from a Rust test with no server, no network, and no model files.

**Architecture:** A Cargo workspace. `memorysafe-core` holds pure types and the `GovernancePolicy` trait. `memorysafe-backend` holds the `Backend` trait plus a conformance suite that any backend must pass. `memorysafe-backend-sqlite` implements it with one database file per tenant. `memorysafe-policy` implements `BaselinePolicy`. `memorysafe-engine` orchestrates: it performs all I/O, hands pure context structs to the policy, validates every decision the policy returns, and applies writes in one atomic backend transaction with an audit record.

**Tech Stack:** Rust 1.97.1 (edition 2024), `rusqlite` 0.40 (bundled SQLite, WAL, FTS5), `tokio` 1.53, `async-trait` 0.1, `serde` 1.0, `ulid` 3.0, `time` 0.3, `moka` 0.12, `thiserror` 2.0, `proptest` 1.11, `tempfile` 3.27, `model2vec-rs` 0.2 (optional feature).

**Source spec:** `docs/superpowers/specs/2026-09-05-memorysafe-engine-design.md`

---

## Global Constraints

Every task's requirements implicitly include this section.

- **Rust edition 2024**, toolchain pinned to `1.97.1` via `rust-toolchain.toml`.
- **No LLM and no network call in the write path.** Embeddings only, computed in-process.
- **Policies are pure.** `memorysafe-policy` and `memorysafe-core` must not depend on `memorysafe-backend`, `tokio`, `rusqlite`, or any I/O crate. Enforced by a CI dependency check.
- **The `Backend` trait is `async` throughout** (`#[async_trait]`). The SQLite implementation wraps blocking calls in `tokio::task::spawn_blocking`.
- **Hard filters execute inside the backend query.** Scope, tags, kinds, time bounds, and the sensitivity ceiling are applied in SQL. A policy may only narrow a candidate set, never widen it.
- **Audit rows never contain item bodies** — only ids, content digests (BLAKE3 hex), and feature numbers.
- **Tenant isolation is structural:** one SQLite file per tenant. No query may span tenants.
- **TDD.** Every task writes a failing test first, watches it fail, then implements. Commit at the end of every task.
- **Lints:** `#![deny(warnings)]` in CI via `RUSTFLAGS="-Dwarnings"`, plus `cargo clippy --all-targets --all-features -- -D warnings`.
- **`Score` is a newtype over `f32` clamped to `[0.0, 1.0]`.** Never a bare `f32`.
- **Timestamps are `time::OffsetDateTime`,** stored as Unix seconds (`i64`) in SQLite.
- **Ids are ULIDs** rendered as their 26-character Crockford base32 string.

---

## File Structure

Locking decomposition in before tasks. Each file has one responsibility.

### `crates/memorysafe-core` — pure types + the policy trait, no I/O

| File | Responsibility |
|---|---|
| `src/lib.rs` | Re-exports. Module declarations only. |
| `src/error.rs` | `CoreError` — validation failures on type construction. |
| `src/ids.rs` | `ItemId`, `AuditId`, `TenantId`, `SubjectId`, `Namespace`, `Scope`. |
| `src/item.rs` | `MemoryItem`, `Source`, `SourceKind`, `Protection`. |
| `src/score.rs` | `Score`, `FeatureMap`. |
| `src/assessment.rs` | `Assessment`, `SensitivityLevel`, `SensitivityCategory`, `SensitivityAssessment`, `RedundancyAssessment`, `AssessorId`. |
| `src/capacity.rs` | `Budget`, `CapacityState`, `ScopeStats`. |
| `src/decision.rs` | `Action`, `Decision`, `Eviction`, `Reason`, `ReasonCode`, `MergeStrategy`, `PolicyId`. |
| `src/audit.rs` | `AuditRecord`, `AuditEvent`, `Actor`, `ItemRef`, `AuditFilter`. |
| `src/recall.rs` | `RecallRequest`, `RecallMode`, `WorkingSet`, `SelectedItem`, `OmittedItem`, `ScoredCandidate`, `Budget` usage. |
| `src/embedding.rs` | `Embedding`, `EmbedderId`. |
| `src/policy.rs` | `GovernancePolicy` trait, `Candidate`, `Assessed`, `AssessContext`, `AdmitContext`, `ComposeContext`, `MaintainContext`, `PolicyError`. |

### `crates/memorysafe-embed` — embedding + quantization

| File | Responsibility |
|---|---|
| `src/lib.rs` | `Embedder` trait, `EmbedError`. |
| `src/test_embedder.rs` | `DeterministicEmbedder` — hash-based, no model files. |
| `src/quantize.rs` | `QuantizedVector`, L2-normalize → int8, dot product. |
| `src/model2vec.rs` | `Model2VecEmbedder`, behind feature `model2vec`. |

### `crates/memorysafe-backend` — the trait + the conformance suite

| File | Responsibility |
|---|---|
| `src/lib.rs` | `Backend` trait, `BackendError`. |
| `src/query.rs` | `CandidateQuery`, `HardFilters`, `Page`. |
| `src/write.rs` | `WriteTransaction`, `ItemWrite`, `MergeWrite`, `AppliedWrite`, `PurgeReport`. |
| `src/portability.rs` | `ExportStream`, `ImportStream`, `ImportReport`, `ScopeSelector`. |
| `src/conformance/mod.rs` | `run_conformance_suite<B: Backend>(factory)` — the entry point. |
| `src/conformance/isolation.rs` | Cross-tenant and cross-subject isolation. |
| `src/conformance/atomicity.rs` | Atomic admit+evict+audit; idempotency. |
| `src/conformance/retrieval.rs` | Hard filters, sensitivity ceiling, ranking ties, pagination. |
| `src/conformance/capacity.rs` | Accounting under concurrent writes. |
| `src/conformance/lifecycle.rs` | Audit query, `purge_subject`, export/import round-trip, cross-model vector rejection. |

### `crates/memorysafe-backend-sqlite` — the OSS backend

| File | Responsibility |
|---|---|
| `src/lib.rs` | `SqliteBackend`, `Backend` impl wiring. |
| `src/schema.rs` | DDL, `schema_version`, migrations. |
| `src/tenant.rs` | `TenantManager` — LRU connection pool, per-tenant write serialization. |
| `src/items.rs` | Item read/write row mapping. |
| `src/vectors.rs` | Vector row storage, scope vector-block loading, brute-force search. |
| `src/keyword.rs` | FTS5 query construction and escaping. |
| `src/retrieve.rs` | Hybrid fusion of vector + keyword into `ScoredCandidate`. |
| `src/capacity.rs` | Capacity accounting rows, locked during apply. |
| `src/audit.rs` | Audit row write and query. |
| `src/purge.rs` | `purge_subject`. |
| `src/portability.rs` | Export/import streams. |

### `crates/memorysafe-policy` — `BaselinePolicy`

| File | Responsibility |
|---|---|
| `src/lib.rs` | `BaselinePolicy`, `GovernancePolicy` impl wiring. |
| `src/config.rs` | `BaselineConfig` — all thresholds with defaults. |
| `src/value.rs` | Value scoring. |
| `src/fragility.rs` | Fragility from neighbourhood sparsity. |
| `src/sensitivity.rs` | Pattern + lexicon detectors. |
| `src/redundancy.rs` | Max-cosine redundancy, merge/duplicate thresholds. |
| `src/admit.rs` | Admission and eviction selection. |
| `src/compose.rs` | MMR + replay quota + budget packing. |
| `src/maintain.rs` | TTL, decay, consolidation, reclaim. |

### `crates/memorysafe-engine` — orchestration

| File | Responsibility |
|---|---|
| `src/lib.rs` | `Engine`, `EngineConfig`, construction. |
| `src/error.rs` | `EngineError`. |
| `src/validate.rs` | Decision validation, `catch_unwind`, `fail_closed`/`fail_safe`. |
| `src/write.rs` | `remember` — the write pipeline. |
| `src/read.rs` | `recall` — the read pipeline. |
| `src/maintain.rs` | Resumable maintenance job with cursor. |
| `src/cache.rs` | `moka` caches and invalidation. |
| `src/retention.rs` | Retention profiles and audit retention enforcement. |
| `src/portability.rs` | Export/import orchestration. |
| `src/outcome.rs` | `WriteOutcome`, `ForgetOutcome`, `PurgeOutcome`. |
| `tests/invariants.rs` | The five proptest invariants. |

### Canonical signatures

These names are referenced across tasks. Any deviation is a bug.

```rust
// memorysafe-core::policy
#[allow(clippy::result_large_err)]
pub trait GovernancePolicy: Send + Sync {
    fn id(&self) -> PolicyId;
    fn assess(&self, cand: &Candidate, ctx: &AssessContext) -> Result<Assessment, PolicyError>;
    fn admit(&self, assessed: &Assessed, ctx: &AdmitContext) -> Result<Decision, PolicyError>;
    fn compose(&self, req: &RecallRequest, candidates: &[ScoredCandidate], ctx: &ComposeContext)
        -> Result<WorkingSet, PolicyError>;
    fn maintain(&self, ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError>;
}

// memorysafe-embed
pub trait Embedder: Send + Sync {
    fn id(&self) -> EmbedderId;
    fn dim(&self) -> u16;
    fn embed(&self, text: &str) -> Result<Embedding, EmbedError>;
}

// memorysafe-backend
#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    async fn retrieve_candidates(&self, scope: &Scope, query: &CandidateQuery)
        -> Result<Vec<ScoredCandidate>, BackendError>;
    async fn neighbours(&self, scope: &Scope, embedding: &Embedding, k: usize)
        -> Result<Vec<ScoredCandidate>, BackendError>;
    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, BackendError>;
    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, BackendError>;
    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError>;
    async fn record_recall(&self, record: AuditRecord) -> Result<AuditId, BackendError>;
    async fn get(&self, scope: &Scope, id: &ItemId) -> Result<Option<MemoryItem>, BackendError>;
    async fn list(&self, scope: &Scope, page: &Page) -> Result<Vec<MemoryItem>, BackendError>;
    async fn audit(&self, scope: &Scope, filter: &AuditFilter)
        -> Result<Vec<AuditRecord>, BackendError>;
    async fn purge_subject(&self, tenant: &TenantId, subject: &SubjectId)
        -> Result<PurgeReport, BackendError>;
    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError>;
    async fn import(&self, stream: ImportStream) -> Result<ImportReport, BackendError>;
}

// memorysafe-engine
impl Engine {
    pub async fn remember(&self, req: RememberRequest) -> Result<WriteOutcome, EngineError>;
    pub async fn recall(&self, req: RecallRequest) -> Result<WorkingSet, EngineError>;
    pub async fn forget(&self, scope: &Scope, sel: ForgetSelector)
        -> Result<ForgetOutcome, EngineError>;
    pub async fn review(&self, scope: &Scope, page: &Page)
        -> Result<Vec<MemoryItem>, EngineError>;
    pub async fn protect(&self, scope: &Scope, id: &ItemId, p: Protection)
        -> Result<WriteOutcome, EngineError>;
    pub async fn maintain(&self, scope: &Scope, cursor: Option<MaintainCursor>)
        -> Result<MaintainReport, EngineError>;
    pub async fn purge_subject(&self, tenant: &TenantId, subject: &SubjectId)
        -> Result<PurgeOutcome, EngineError>;
}
```

### SQLite schema (one file per tenant)

Referenced by Tasks 18–24. `schema_version = 1`.

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE items (
  id                TEXT PRIMARY KEY,
  subject           TEXT NOT NULL,
  namespace         TEXT NOT NULL,
  body              TEXT NOT NULL,
  kind              TEXT NOT NULL,
  source_kind       TEXT NOT NULL,
  source_id         TEXT,
  occurred_at       INTEGER,
  created_at        INTEGER NOT NULL,
  tags              TEXT NOT NULL,            -- JSON array
  attrs             TEXT NOT NULL,            -- JSON object
  sensitivity       INTEGER NOT NULL,         -- ordinal 0..4
  ttl_seconds       INTEGER,
  protection        TEXT NOT NULL,            -- 'normal' | 'protected' | 'pinned'
  protected_until   INTEGER,
  value_score       REAL NOT NULL,
  fragility_score   REAL NOT NULL,
  byte_size         INTEGER NOT NULL,
  last_access       INTEGER,
  access_count      INTEGER NOT NULL DEFAULT 0,
  pending_embedding INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_items_scope      ON items(subject, namespace);
CREATE INDEX idx_items_scope_sens ON items(subject, namespace, sensitivity);
CREATE INDEX idx_items_ttl        ON items(created_at) WHERE ttl_seconds IS NOT NULL;

CREATE VIRTUAL TABLE items_fts USING fts5(
  body, tags, content='items', content_rowid='rowid', tokenize='unicode61'
);
CREATE TRIGGER items_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, body, tags) VALUES (new.rowid, new.body, new.tags);
END;
CREATE TRIGGER items_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, body, tags)
    VALUES('delete', old.rowid, old.body, old.tags);
END;
CREATE TRIGGER items_au AFTER UPDATE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, body, tags)
    VALUES('delete', old.rowid, old.body, old.tags);
  INSERT INTO items_fts(rowid, body, tags) VALUES (new.rowid, new.body, new.tags);
END;

-- subject/namespace denormalized so a scope vector scan needs no join
CREATE TABLE vectors (
  item_id   TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  subject   TEXT NOT NULL,
  namespace TEXT NOT NULL,
  embedder  TEXT NOT NULL,
  dim       INTEGER NOT NULL,
  scale     REAL NOT NULL,
  q         BLOB NOT NULL                     -- int8, length = dim
);
CREATE INDEX idx_vectors_scope ON vectors(subject, namespace, embedder, dim);

CREATE TABLE capacity (
  subject    TEXT NOT NULL,
  namespace  TEXT NOT NULL,
  max_items  INTEGER,
  max_bytes  INTEGER,
  used_items INTEGER NOT NULL DEFAULT 0,
  used_bytes INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (subject, namespace)
);

CREATE TABLE audit (
  id         TEXT PRIMARY KEY,
  at         INTEGER NOT NULL,
  subject    TEXT NOT NULL,
  namespace  TEXT NOT NULL,
  event      TEXT NOT NULL,
  items      TEXT NOT NULL,                   -- JSON [{id, digest}]
  assessment TEXT,                            -- JSON
  decision   TEXT,                            -- JSON
  actor      TEXT NOT NULL,                   -- JSON
  policy     TEXT
);
CREATE INDEX idx_audit_scope_at ON audit(subject, namespace, at);

CREATE TABLE idempotency (
  key            TEXT PRIMARY KEY,
  subject        TEXT NOT NULL,
  namespace      TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  outcome        TEXT NOT NULL,               -- JSON WriteOutcome
  at             INTEGER NOT NULL
);
```

---

## Task Index

| # | Task | Deliverable |
|---|---|---|
| 1 | Workspace scaffold + CI | `cargo test` runs green on an empty workspace |
| 2 | Core: ids and `Scope` | Validated scope types |
| 3 | Core: `Score` and `FeatureMap` | Clamped score newtype |
| 4 | Core: `MemoryItem`, `Source`, `Protection` | The item type |
| 5 | Core: assessment types | `Assessment` and its parts |
| 6 | Core: capacity types | `Budget`, `CapacityState`, `ScopeStats` |
| 7 | Core: decision types | `Action`, `Decision`, `Reason`, `ReasonCode` |
| 8 | Core: audit types | `AuditRecord` and friends |
| 9 | Core: recall types | `RecallRequest`, `WorkingSet`, `ScoredCandidate` |
| 10 | Core: `GovernancePolicy` trait + contexts | The policy seam |
| 11 | Embed: `Embedder` trait + `DeterministicEmbedder` | Model-free embeddings for tests |
| 12 | Embed: quantization + dot product | `QuantizedVector` |
| 13 | Embed: `Model2VecEmbedder` | Optional real embedder |
| 14 | Backend: trait + query/write types | The backend seam |
| 15 | Conformance: harness + isolation | `run_conformance_suite` entry point |
| 16 | Conformance: atomicity + idempotency | |
| 17 | Conformance: retrieval + capacity | |
| 18 | Conformance: lifecycle | Audit, purge, portability, cross-model |
| 19 | SQLite: schema + `TenantManager` | Per-tenant files, LRU pool |
| 20 | SQLite: items + audit | Isolation conformance passes |
| 21 | SQLite: vectors + brute-force search | `neighbours` works |
| 22 | SQLite: FTS5 + hybrid retrieval | `retrieve_candidates` works |
| 23 | SQLite: capacity + atomic `apply` | Atomicity + capacity conformance passes |
| 24 | SQLite: purge + export/import | Full conformance suite passes |
| 25 | Policy: config + redundancy + fragility | |
| 26 | Policy: value + sensitivity | `assess` complete |
| 27 | Policy: `admit` | |
| 28 | Policy: `compose` | |
| 29 | Policy: `maintain` | `BaselinePolicy` complete |
| 30 | Engine: decision validation | `fail_closed` / `fail_safe` |
| 31 | Engine: `remember` | Write pipeline end to end |
| 32 | Engine: `recall` | Read pipeline end to end |
| 33 | Engine: `forget`, `review`, `protect`, `purge_subject` | |
| 34 | Engine: maintenance job | Resumable with cursor |
| 35 | Engine: cache + invalidation | |
| 36 | Engine: retention profiles | |
| 37 | Engine: export/import orchestration | |
| 38 | Proptest invariants | The five correctness properties |

---

## Task 1: Workspace scaffold + CI

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `clippy.toml`
- Create: `crates/memorysafe-core/Cargo.toml`, `crates/memorysafe-core/src/lib.rs`
- Create: `.github/workflows/ci.yml`
- Test: `crates/memorysafe-core/src/lib.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing.
- Produces: a compiling workspace with `memorysafe-core` as its first member.

- [ ] **Step 1: Write the failing test**

Create `crates/memorysafe-core/src/lib.rs`:

```rust
//! Pure types and the governance policy trait. No I/O.

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core`
Expected: FAIL — `error: failed to load manifest` / no such package, because no manifests exist yet.

- [ ] **Step 3: Write minimal implementation**

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.97.1"
components = ["rustfmt", "clippy"]
```

`Cargo.toml` (workspace root):

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
rust-version = "1.97.1"
license = "Apache-2.0"

[workspace.dependencies]
memorysafe-core = { path = "crates/memorysafe-core" }
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
thiserror = "2.0.20"
ulid = { version = "3.0.0", features = ["serde"] }
time = { version = "0.3.55", features = ["serde", "macros"] }
blake3 = "1.5"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
```

`crates/memorysafe-core/Cargo.toml`:

```toml
[package]
name = "memorysafe-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
ulid.workspace = true
time.workspace = true
blake3.workspace = true

[lints]
workspace = true
```

`.gitignore`:

```
/target
**/*.rs.bk
.env
```

`clippy.toml`:

```toml
avoid-breaking-exported-api = false
```

`.github/workflows/ci.yml`:

```yaml
name: ci
on: [push, pull_request]
env:
  RUSTFLAGS: "-Dwarnings"
  CARGO_TERM_COLOR: always
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.97.1
        with:
          components: rustfmt, clippy
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test --all-features --workspace
  purity:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.97.1
      - name: memorysafe-core and memorysafe-policy must have no I/O dependencies
        run: |
          cargo tree -p memorysafe-core --edges normal --prefix none \
            | grep -Ei '^(tokio|rusqlite|sqlx|reqwest|hyper) ' && exit 1
          echo "core is I/O free"
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core`
Expected: PASS — `test tests::workspace_builds ... ok`

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore clippy.toml crates/ .github/
git commit -m "chore: scaffold cargo workspace and CI"
```

---

## Task 2: Core — ids and `Scope`

**Files:**
- Create: `crates/memorysafe-core/src/error.rs`
- Create: `crates/memorysafe-core/src/ids.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: Task 1's workspace.
- Produces: `CoreError`, `ItemId::new()`, `AuditId::new()`, `TenantId::new(&str)`, `SubjectId::new(&str)`, `Namespace::new(&str)`, `Scope { tenant, subject, namespace }`, `Scope::key() -> String`.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/ids.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_ids_are_unique_and_sortable() {
        let a = ItemId::new();
        let b = ItemId::new();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 26);
    }

    #[test]
    fn scope_components_reject_empty_and_oversize() {
        assert!(TenantId::new("").is_err());
        assert!(SubjectId::new("").is_err());
        assert!(Namespace::new("").is_err());
        let long = "x".repeat(257);
        assert!(TenantId::new(&long).is_err());
    }

    #[test]
    fn scope_components_reject_path_and_control_characters() {
        // Tenant ids become filenames in the SQLite backend.
        assert!(TenantId::new("../escape").is_err());
        assert!(TenantId::new("a/b").is_err());
        assert!(TenantId::new("a\0b").is_err());
        assert!(TenantId::new("acme-corp_1").is_ok());
    }

    #[test]
    fn scope_key_is_stable_and_unambiguous() {
        let s = Scope::new("t1", "s1", "n1").unwrap();
        assert_eq!(s.key(), "t1\u{1f}s1\u{1f}n1");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core ids`
Expected: FAIL — `cannot find type ItemId in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds {max} bytes")]
    TooLong { field: &'static str, max: usize },
    #[error("{field} contains an illegal character at byte {index}")]
    IllegalChar { field: &'static str, index: usize },
    #[error("{field} must be within [0.0, 1.0], got {value}")]
    OutOfRange { field: &'static str, value: f32 },
}
```

`crates/memorysafe-core/src/ids.rs`:

```rust
use crate::error::CoreError;
use serde::{Deserialize, Serialize};

const MAX_COMPONENT_BYTES: usize = 256;

/// Scope components are used as SQLite filenames and SQL parameters, so the
/// allowed alphabet is deliberately narrow: ASCII alphanumerics, `-`, `_`, `.`,
/// with a leading `.` rejected to rule out `.` and `..`.
fn validate_component(field: &'static str, raw: &str) -> Result<(), CoreError> {
    if raw.is_empty() {
        return Err(CoreError::Empty { field });
    }
    if raw.len() > MAX_COMPONENT_BYTES {
        return Err(CoreError::TooLong { field, max: MAX_COMPONENT_BYTES });
    }
    if raw.starts_with('.') {
        return Err(CoreError::IllegalChar { field, index: 0 });
    }
    for (index, byte) in raw.bytes().enumerate() {
        let ok = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
        if !ok {
            return Err(CoreError::IllegalChar { field, index });
        }
    }
    Ok(())
}

macro_rules! scope_component {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(raw: &str) -> Result<Self, CoreError> {
                validate_component($field, raw)?;
                Ok(Self(raw.to_owned()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

scope_component!(TenantId, "tenant");
scope_component!(SubjectId, "subject");
scope_component!(Namespace, "namespace");

macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(ulid::Ulid::new().to_string())
            }
            pub fn parse(raw: &str) -> Result<Self, CoreError> {
                ulid::Ulid::from_string(raw)
                    .map_err(|_| CoreError::IllegalChar { field: stringify!($name), index: 0 })?;
                Ok(Self(raw.to_owned()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

ulid_id!(ItemId);
ulid_id!(AuditId);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Scope {
    pub tenant: TenantId,
    pub subject: SubjectId,
    pub namespace: Namespace,
}

impl Scope {
    pub fn new(tenant: &str, subject: &str, namespace: &str) -> Result<Self, CoreError> {
        Ok(Self {
            tenant: TenantId::new(tenant)?,
            subject: SubjectId::new(subject)?,
            namespace: Namespace::new(namespace)?,
        })
    }

    /// Cache-key form. Uses ASCII unit separator, which `validate_component`
    /// forbids inside components, so the encoding is unambiguous.
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}\u{1f}{}", self.tenant, self.subject, self.namespace)
    }
}
```

`crates/memorysafe-core/src/lib.rs`:

```rust
//! Pure types and the governance policy trait. No I/O.

pub mod error;
pub mod ids;

pub use error::CoreError;
pub use ids::{AuditId, ItemId, Namespace, Scope, SubjectId, TenantId};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core ids`
Expected: PASS — 4 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): scope components and ULID identifiers with validation"
```

---

## Task 3: Core — `Score` and `FeatureMap`

**Files:**
- Create: `crates/memorysafe-core/src/score.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: `CoreError` from Task 2.
- Produces: `Score::new(f32) -> Result<Score, CoreError>`, `Score::clamped(f32) -> Score`, `Score::get() -> f32`, `Score::ZERO`, `Score::ONE`, `FeatureMap` (alias for `BTreeMap<String, f64>`), `features!` macro.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/score.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_rejects_out_of_range_and_nan() {
        assert!(Score::new(-0.1).is_err());
        assert!(Score::new(1.1).is_err());
        assert!(Score::new(f32::NAN).is_err());
        assert!(Score::new(0.0).is_ok());
        assert!(Score::new(1.0).is_ok());
    }

    #[test]
    fn clamped_never_fails_and_maps_nan_to_zero() {
        assert_eq!(Score::clamped(-5.0).get(), 0.0);
        assert_eq!(Score::clamped(5.0).get(), 1.0);
        assert_eq!(Score::clamped(f32::NAN).get(), 0.0);
    }

    #[test]
    fn scores_have_a_total_order() {
        let mut v = vec![Score::clamped(0.5), Score::clamped(0.1), Score::clamped(0.9)];
        v.sort();
        assert_eq!(v[0].get(), 0.1);
        assert_eq!(v[2].get(), 0.9);
    }

    #[test]
    fn features_macro_builds_a_map() {
        let f = features! { "redundancy" => 0.93, "neighbours" => 4.0 };
        assert_eq!(f.get("redundancy"), Some(&0.93));
        assert_eq!(f.len(), 2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core score`
Expected: FAIL — `cannot find type Score in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/score.rs`:

```rust
use crate::error::CoreError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Evidence numbers attached to a reason or an assessment. Ordered so that
/// audit rows serialize deterministically.
pub type FeatureMap = BTreeMap<String, f64>;

#[macro_export]
macro_rules! features {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut m = $crate::score::FeatureMap::new();
        $( m.insert($k.to_string(), $v as f64); )*
        m
    }};
}

/// A value in `[0.0, 1.0]`. Never a bare `f32` anywhere in the API.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Score(f32);

impl Score {
    pub const ZERO: Score = Score(0.0);
    pub const ONE: Score = Score(1.0);

    pub fn new(value: f32) -> Result<Self, CoreError> {
        if value.is_nan() || !(0.0..=1.0).contains(&value) {
            return Err(CoreError::OutOfRange { field: "score", value });
        }
        Ok(Self(value))
    }

    /// Total function for internal arithmetic. NaN maps to zero.
    pub fn clamped(value: f32) -> Self {
        if value.is_nan() {
            return Self(0.0);
        }
        Self(value.clamp(0.0, 1.0))
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

impl Eq for Score {}

#[allow(clippy::derive_ord_xor_partial_ord)]
impl Ord for Score {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Safe: the constructors exclude NaN.
        self.0.partial_cmp(&other.0).unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
```

Add to `crates/memorysafe-core/src/lib.rs`:

```rust
pub mod score;
pub use score::{FeatureMap, Score};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core score`
Expected: PASS — 4 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): Score newtype and FeatureMap"
```

---

## Task 4: Core — `MemoryItem`, `Source`, `Protection`

**Files:**
- Create: `crates/memorysafe-core/src/item.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: `Scope`, `ItemId`, `CoreError`.
- Produces: `MemoryItem`, `Source`, `SourceKind`, `Protection`, `MemoryItem::byte_size()`, `MemoryItem::digest()`, `Protection::is_evictable(now)`.

**Spec note:** `protection` is engine-maintained state, never caller-supplied. `MemoryItem` therefore has no public constructor that accepts it; the engine sets it from `Action::Retain { protection }`.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/item.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn item(body: &str) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: Scope::new("t", "s", "n").unwrap(),
            body: body.to_string(),
            kind: "fact".into(),
            source: Source { kind: SourceKind::Agent, id: Some("agent-1".into()) },
            occurred_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            tags: vec![],
            attrs: Default::default(),
            sensitivity: SensitivityLevel::Internal,
            ttl: None,
            protection: Protection::Normal,
            pending_embedding: false,
        }
    }

    #[test]
    fn digest_is_stable_and_content_addressed() {
        assert_eq!(item("hello").digest(), item("hello").digest());
        assert_ne!(item("hello").digest(), item("world").digest());
        assert_eq!(item("hello").digest().len(), 64); // blake3 hex
    }

    #[test]
    fn byte_size_counts_body_and_metadata() {
        let small = item("a");
        let large = item(&"a".repeat(1000));
        assert!(large.byte_size() > small.byte_size());
        assert!(small.byte_size() > 0);
    }

    #[test]
    fn pinned_items_are_never_evictable() {
        let now = OffsetDateTime::UNIX_EPOCH;
        assert!(!Protection::Pinned.is_evictable(now));
        assert!(Protection::Normal.is_evictable(now));
    }

    #[test]
    fn protection_window_expires() {
        let now = OffsetDateTime::from_unix_timestamp(1000).unwrap();
        let future = OffsetDateTime::from_unix_timestamp(2000).unwrap();
        let past = OffsetDateTime::from_unix_timestamp(500).unwrap();
        assert!(!Protection::Protected { until: future }.is_evictable(now));
        assert!(Protection::Protected { until: past }.is_evictable(now));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core item`
Expected: FAIL — `cannot find type MemoryItem in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/item.rs`:

```rust
use crate::assessment::SensitivityLevel;
use crate::ids::{ItemId, Scope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Agent,
    Session,
    Tool,
    Human,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    pub id: Option<String>,
}

/// Engine-maintained. Set from `Action::Retain`, changed only by an explicit
/// `protect` call or by a `maintain` decision expiring a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Protection {
    Normal,
    Protected {
        #[serde(with = "time::serde::timestamp")]
        until: OffsetDateTime,
    },
    /// Absolute. No policy may evict a pinned item; the engine refuses any
    /// decision that tries.
    Pinned,
}

impl Protection {
    pub fn is_evictable(&self, now: OffsetDateTime) -> bool {
        match self {
            Protection::Normal => true,
            Protection::Pinned => false,
            Protection::Protected { until } => *until <= now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: ItemId,
    pub scope: Scope,
    pub body: String,
    pub kind: String,
    pub source: Source,
    #[serde(with = "time::serde::timestamp::option")]
    pub occurred_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp")]
    pub created_at: OffsetDateTime,
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, serde_json::Value>,
    /// Resolved by the policy; the caller's hint may only raise it.
    pub sensitivity: SensitivityLevel,
    pub ttl: Option<Duration>,
    pub protection: Protection,
    /// True when the embedder was unavailable at write time. Excluded from
    /// vector retrieval until backfilled; still visible to keyword and review.
    pub pending_embedding: bool,
}

impl MemoryItem {
    /// BLAKE3 of the content-identifying fields, hex encoded. Used in audit
    /// rows, which never store bodies.
    pub fn digest(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(self.body.as_bytes());
        h.update(b"\x1f");
        h.update(self.kind.as_bytes());
        h.update(b"\x1f");
        for tag in &self.tags {
            h.update(tag.as_bytes());
            h.update(b",");
        }
        h.finalize().to_hex().to_string()
    }

    /// Charged against `Budget::max_bytes`.
    pub fn byte_size(&self) -> u64 {
        let attrs = serde_json::to_string(&self.attrs).map(|s| s.len()).unwrap_or(0);
        let tags: usize = self.tags.iter().map(|t| t.len()).sum();
        (self.body.len() + self.kind.len() + attrs + tags + 64) as u64
    }

    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        match self.ttl {
            Some(ttl) => self.created_at + ttl <= now,
            None => false,
        }
    }
}
```

Add to `crates/memorysafe-core/src/lib.rs`:

```rust
pub mod item;
pub use item::{MemoryItem, Protection, Source, SourceKind};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core item`
Expected: PASS — 4 tests ok. (Requires Task 5's `SensitivityLevel`; implement Task 5 first if the compiler objects, then return here.)

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): MemoryItem, Source, and engine-maintained Protection"
```

---

## Task 5: Core — assessment types

**Files:**
- Create: `crates/memorysafe-core/src/assessment.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: `Score`, `FeatureMap`, `ItemId`.
- Produces: `SensitivityLevel` (ordinal, `Ord`), `SensitivityCategory`, `SensitivityAssessment`, `RedundancyAssessment`, `Assessment`, `AssessorId`.

**Note:** Task 4 depends on `SensitivityLevel`, so this task may be implemented before Task 4. The ordering in this plan is by concept, not by compile order; a subagent hitting a missing type should implement the task that defines it and return.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/assessment.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::features;

    #[test]
    fn sensitivity_levels_are_ordered_least_to_most_restrictive() {
        assert!(SensitivityLevel::Public < SensitivityLevel::Internal);
        assert!(SensitivityLevel::Internal < SensitivityLevel::Personal);
        assert!(SensitivityLevel::Personal < SensitivityLevel::Sensitive);
        assert!(SensitivityLevel::Sensitive < SensitivityLevel::Restricted);
    }

    #[test]
    fn sensitivity_ordinals_round_trip_for_sql_storage() {
        for level in SensitivityLevel::ALL {
            assert_eq!(SensitivityLevel::from_ordinal(level.ordinal()), Some(level));
        }
        assert_eq!(SensitivityLevel::from_ordinal(99), None);
    }

    #[test]
    fn a_hint_may_only_raise_the_level() {
        let resolved = SensitivityLevel::Internal.raised_by(Some(SensitivityLevel::Restricted));
        assert_eq!(resolved, SensitivityLevel::Restricted);
        let ignored = SensitivityLevel::Sensitive.raised_by(Some(SensitivityLevel::Public));
        assert_eq!(ignored, SensitivityLevel::Sensitive);
        assert_eq!(SensitivityLevel::Personal.raised_by(None), SensitivityLevel::Personal);
    }

    #[test]
    fn assessment_carries_its_evidence() {
        let a = Assessment {
            value: Score::clamped(0.7),
            fragility: Score::clamped(0.3),
            sensitivity: SensitivityAssessment {
                level: SensitivityLevel::Personal,
                categories: vec![SensitivityCategory::Pii],
                confidence: Score::clamped(0.8),
            },
            redundancy: RedundancyAssessment {
                score: Score::clamped(0.1),
                near_duplicates: vec![],
            },
            features: features! { "neighbour_count" => 3.0 },
            assessor: AssessorId::new("baseline", "0.1.0"),
        };
        assert_eq!(a.features.get("neighbour_count"), Some(&3.0));
        assert_eq!(a.assessor.to_string(), "baseline@0.1.0");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core assessment`
Expected: FAIL — `cannot find type SensitivityLevel in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/assessment.rs`:

```rust
use crate::ids::ItemId;
use crate::score::{FeatureMap, Score};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityLevel {
    Public,
    Internal,
    Personal,
    Sensitive,
    Restricted,
}

impl SensitivityLevel {
    pub const ALL: [SensitivityLevel; 5] = [
        SensitivityLevel::Public,
        SensitivityLevel::Internal,
        SensitivityLevel::Personal,
        SensitivityLevel::Sensitive,
        SensitivityLevel::Restricted,
    ];

    /// Stored as an INTEGER so SQL can express `sensitivity <= ceiling`.
    pub fn ordinal(self) -> i64 {
        match self {
            SensitivityLevel::Public => 0,
            SensitivityLevel::Internal => 1,
            SensitivityLevel::Personal => 2,
            SensitivityLevel::Sensitive => 3,
            SensitivityLevel::Restricted => 4,
        }
    }

    pub fn from_ordinal(value: i64) -> Option<Self> {
        Self::ALL.get(usize::try_from(value).ok()?).copied()
    }

    /// A caller's `sensitivity_hint` may raise the detected level, never lower it.
    pub fn raised_by(self, hint: Option<SensitivityLevel>) -> Self {
        match hint {
            Some(h) => self.max(h),
            None => self,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityCategory {
    Pii,
    Health,
    Financial,
    Credential,
    Legal,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensitivityAssessment {
    pub level: SensitivityLevel,
    pub categories: Vec<SensitivityCategory>,
    pub confidence: Score,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedundancyAssessment {
    pub score: Score,
    /// Descending by similarity.
    pub near_duplicates: Vec<(ItemId, f32)>,
}

impl RedundancyAssessment {
    pub fn best(&self) -> Option<(&ItemId, f32)> {
        self.near_duplicates.first().map(|(id, s)| (id, *s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessorId {
    pub name: String,
    pub version: String,
}

impl AssessorId {
    pub fn new(name: &str, version: &str) -> Self {
        Self { name: name.to_owned(), version: version.to_owned() }
    }
}

impl std::fmt::Display for AssessorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    pub value: Score,
    pub fragility: Score,
    pub sensitivity: SensitivityAssessment,
    pub redundancy: RedundancyAssessment,
    pub features: FeatureMap,
    pub assessor: AssessorId,
}
```

Add to `crates/memorysafe-core/src/lib.rs`:

```rust
pub mod assessment;
pub use assessment::{
    Assessment, AssessorId, RedundancyAssessment, SensitivityAssessment, SensitivityCategory,
    SensitivityLevel,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core`
Expected: PASS — Tasks 2–5 tests all green (16 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): assessment types with ordered sensitivity levels"
```

---

## Task 6: Core — capacity types

**Files:**
- Create: `crates/memorysafe-core/src/capacity.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `Budget { max_items: Option<u64>, max_bytes: Option<u64> }`, `CapacityState`, `CapacityState::pressure() -> f32`, `CapacityState::would_exceed(items, bytes) -> bool`, `ScopeStats`.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/capacity.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn state(used_items: u64, max_items: Option<u64>) -> CapacityState {
        CapacityState {
            budget: Budget { max_items, max_bytes: None },
            used_items,
            used_bytes: 0,
        }
    }

    #[test]
    fn unbounded_budget_has_no_pressure() {
        assert_eq!(state(1_000_000, None).pressure(), 0.0);
    }

    #[test]
    fn pressure_is_the_max_of_the_bounded_dimensions() {
        assert_eq!(state(50, Some(100)).pressure(), 0.5);
        assert_eq!(state(100, Some(100)).pressure(), 1.0);
        // Over budget saturates rather than exceeding 1.0.
        assert_eq!(state(200, Some(100)).pressure(), 1.0);
    }

    #[test]
    fn byte_pressure_counts_too() {
        let s = CapacityState {
            budget: Budget { max_items: Some(100), max_bytes: Some(1000) },
            used_items: 10,   // 0.1
            used_bytes: 900,  // 0.9  <- dominates
        };
        assert!((s.pressure() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn would_exceed_detects_the_admission_boundary() {
        let s = state(99, Some(100));
        assert!(!s.would_exceed(1, 0));
        assert!(s.would_exceed(2, 0));
    }

    #[test]
    fn zero_budget_is_always_full() {
        assert_eq!(state(0, Some(0)).pressure(), 1.0);
        assert!(state(0, Some(0)).would_exceed(1, 0));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core capacity`
Expected: FAIL — `cannot find type CapacityState in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/capacity.rs`:

```rust
use serde::{Deserialize, Serialize};

/// A namespace's capacity budget. `None` means unbounded on that dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Budget {
    pub max_items: Option<u64>,
    pub max_bytes: Option<u64>,
}

impl Budget {
    pub const UNBOUNDED: Budget = Budget { max_items: None, max_bytes: None };

    pub fn is_bounded(&self) -> bool {
        self.max_items.is_some() || self.max_bytes.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityState {
    pub budget: Budget,
    pub used_items: u64,
    pub used_bytes: u64,
}

fn ratio(used: u64, max: u64) -> f32 {
    if max == 0 {
        return 1.0;
    }
    (used as f32 / max as f32).min(1.0)
}

impl CapacityState {
    /// `0.0` when unbounded or empty, `1.0` at or over budget. The maximum
    /// across bounded dimensions — the tightest constraint governs.
    pub fn pressure(&self) -> f32 {
        let mut p: f32 = 0.0;
        if let Some(max) = self.budget.max_items {
            p = p.max(ratio(self.used_items, max));
        }
        if let Some(max) = self.budget.max_bytes {
            p = p.max(ratio(self.used_bytes, max));
        }
        p
    }

    /// True when admitting `items`/`bytes` more would break the budget.
    pub fn would_exceed(&self, items: u64, bytes: u64) -> bool {
        let items_over = self
            .budget
            .max_items
            .is_some_and(|max| self.used_items.saturating_add(items) > max);
        let bytes_over = self
            .budget
            .max_bytes
            .is_some_and(|max| self.used_bytes.saturating_add(bytes) > max);
        items_over || bytes_over
    }
}

/// Corpus statistics a policy needs but must not query for itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeStats {
    pub item_count: u64,
    pub total_bytes: u64,
    /// Mean pairwise cosine similarity of a sample, used to calibrate what
    /// "atypical" means in this particular corpus.
    pub mean_neighbour_similarity: f32,
    pub median_item_bytes: u64,
}

impl Default for ScopeStats {
    fn default() -> Self {
        Self {
            item_count: 0,
            total_bytes: 0,
            mean_neighbour_similarity: 0.0,
            median_item_bytes: 0,
        }
    }
}
```

Add to `crates/memorysafe-core/src/lib.rs`:

```rust
pub mod capacity;
pub use capacity::{Budget, CapacityState, ScopeStats};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core capacity`
Expected: PASS — 5 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): Budget, CapacityState with pressure, and ScopeStats"
```

---

## Task 7: Core — decision types

**Files:**
- Create: `crates/memorysafe-core/src/decision.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: `ItemId`, `FeatureMap`, `Protection`.
- Produces: `PolicyId`, `ReasonCode`, `Reason`, `Eviction`, `MergeStrategy`, `Action`, `Decision`, `Decision::retain()`, `Decision::reject()`, `Decision::has_reason(code)`.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/decision.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::features;

    #[test]
    fn reason_codes_serialize_as_stable_snake_case_strings() {
        // Audit rows are queried by these strings; they are a wire format.
        let json = serde_json::to_string(&ReasonCode::HighRedundancy).unwrap();
        assert_eq!(json, "\"high_redundancy\"");
        let back: ReasonCode = serde_json::from_str("\"capacity_pressure\"").unwrap();
        assert_eq!(back, ReasonCode::CapacityPressure);
    }

    #[test]
    fn retain_constructor_produces_a_normal_protection_decision() {
        let d = Decision::retain(
            PolicyId::new("baseline", "0.1.0"),
            Reason::new(ReasonCode::NovelContent, "no near duplicates", features! {}),
        );
        assert!(matches!(d.action, Action::Retain { protection: Protection::Normal }));
        assert!(d.evictions.is_empty());
        assert!(d.has_reason(ReasonCode::NovelContent));
    }

    #[test]
    fn reject_constructor_carries_its_reason() {
        let d = Decision::reject(
            PolicyId::new("baseline", "0.1.0"),
            Reason::new(ReasonCode::ExactDuplicate, "cosine 0.99", features! { "sim" => 0.99 }),
        );
        assert!(matches!(d.action, Action::Reject));
        assert!(d.has_reason(ReasonCode::ExactDuplicate));
        assert_eq!(d.reasons[0].evidence.get("sim"), Some(&0.99));
    }

    #[test]
    fn evictions_name_both_the_item_and_the_reason() {
        let e = Eviction {
            item: ItemId::new(),
            reason: Reason::new(ReasonCode::LowValue, "value 0.02", features! { "value" => 0.02 }),
        };
        assert_eq!(e.reason.code, ReasonCode::LowValue);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core decision`
Expected: FAIL — `cannot find type ReasonCode in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/decision.rs`:

```rust
use crate::ids::ItemId;
use crate::item::Protection;
use crate::score::FeatureMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyId {
    pub name: String,
    pub version: String,
}

impl PolicyId {
    pub fn new(name: &str, version: &str) -> Self {
        Self { name: name.to_owned(), version: version.to_owned() }
    }
}

impl std::fmt::Display for PolicyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// Machine-queryable. These strings are a wire format stored in audit rows;
/// renaming a variant is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    NovelContent,
    HighValue,
    HighRedundancy,
    ExactDuplicate,
    CapacityPressure,
    ProtectedFragile,
    SensitivityCap,
    SensitivityConflict,
    TtlExpired,
    Pinned,
    LowValue,
    ReplayDue,
    DiversityCut,
    BudgetExhausted,
    PolicyInvalid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reason {
    pub code: ReasonCode,
    pub detail: String,
    pub evidence: FeatureMap,
}

impl Reason {
    pub fn new(code: ReasonCode, detail: &str, evidence: FeatureMap) -> Self {
        Self { code, detail: detail.to_owned(), evidence }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Eviction {
    pub item: ItemId,
    pub reason: Reason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Append the new body to the target, union tags and attrs, keep the
    /// earliest `occurred_at` and the latest `created_at`.
    AppendAndUnion,
    /// Replace the target's body with the new one, union tags and attrs.
    ReplaceBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Action {
    Retain { protection: Protection },
    Merge { into: ItemId, strategy: MergeStrategy },
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub action: Action,
    pub evictions: Vec<Eviction>,
    pub reasons: Vec<Reason>,
    pub policy: PolicyId,
}

impl Decision {
    pub fn retain(policy: PolicyId, reason: Reason) -> Self {
        Self {
            action: Action::Retain { protection: Protection::Normal },
            evictions: vec![],
            reasons: vec![reason],
            policy,
        }
    }

    pub fn reject(policy: PolicyId, reason: Reason) -> Self {
        Self { action: Action::Reject, evictions: vec![], reasons: vec![reason], policy }
    }

    pub fn has_reason(&self, code: ReasonCode) -> bool {
        self.reasons.iter().any(|r| r.code == code)
            || self.evictions.iter().any(|e| e.reason.code == code)
    }
}
```

Add to `crates/memorysafe-core/src/lib.rs`:

```rust
pub mod decision;
pub use decision::{Action, Decision, Eviction, MergeStrategy, PolicyId, Reason, ReasonCode};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core decision`
Expected: PASS — 4 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): Decision, Action, and structured ReasonCode"
```

---

## Task 8: Core — audit types

**Files:**
- Create: `crates/memorysafe-core/src/audit.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: `AuditId`, `ItemId`, `Scope`, `Assessment`, `Decision`, `MemoryItem`.
- Produces: `AuditEvent`, `Actor`, `ActorKind`, `ItemRef`, `ItemRef::from_item(&MemoryItem)`, `AuditRecord`, `AuditRecord::new(...)`, `AuditFilter`.

**Spec constraint:** audit rows never contain item bodies. `ItemRef` carries an id and a digest only, and there is no field on `AuditRecord` capable of holding body text.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/audit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::assessment::SensitivityLevel;
    use crate::item::{Protection, Source, SourceKind};
    use crate::{ItemId, MemoryItem, Scope};
    use time::OffsetDateTime;

    fn item(body: &str) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: Scope::new("t", "s", "n").unwrap(),
            body: body.to_string(),
            kind: "fact".into(),
            source: Source { kind: SourceKind::Agent, id: None },
            occurred_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            tags: vec![],
            attrs: Default::default(),
            sensitivity: SensitivityLevel::Internal,
            ttl: None,
            protection: Protection::Normal,
            pending_embedding: false,
        }
    }

    #[test]
    fn item_ref_carries_a_digest_and_never_the_body() {
        let i = item("a secret diagnosis");
        let r = ItemRef::from_item(&i);
        assert_eq!(r.id, i.id);
        assert_eq!(r.digest, i.digest());
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("secret"), "audit must not carry bodies: {json}");
    }

    #[test]
    fn audit_record_serializes_without_any_body_text() {
        let i = item("patient has a rare allergy");
        let rec = AuditRecord::new(
            i.scope.clone(),
            AuditEvent::Admitted,
            vec![ItemRef::from_item(&i)],
            Actor { kind: ActorKind::Agent, id: Some("agent-7".into()) },
        );
        let json = serde_json::to_string(&rec).unwrap();
        assert!(!json.contains("allergy"), "audit must not carry bodies: {json}");
        assert!(json.contains("admitted"));
    }

    #[test]
    fn audit_events_serialize_as_snake_case() {
        assert_eq!(serde_json::to_string(&AuditEvent::SubjectPurged).unwrap(), "\"subject_purged\"");
        assert_eq!(serde_json::to_string(&AuditEvent::Recalled).unwrap(), "\"recalled\"");
    }

    #[test]
    fn default_audit_filter_matches_everything() {
        let f = AuditFilter::default();
        assert!(f.events.is_empty());
        assert_eq!(f.limit, 100);
        assert!(f.since.is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core audit`
Expected: FAIL — `cannot find type ItemRef in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/audit.rs`:

```rust
use crate::assessment::Assessment;
use crate::decision::Decision;
use crate::ids::{AuditId, ItemId, Scope};
use crate::item::MemoryItem;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvent {
    Admitted,
    Rejected,
    Merged,
    Forgotten,
    Recalled,
    Exported,
    Imported,
    SubjectPurged,
    Reembedded,
    PolicyChanged,
    MaintenanceRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Agent,
    Human,
    ApiKey,
    Cli,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    pub id: Option<String>,
}

impl Actor {
    pub fn system() -> Self {
        Self { kind: ActorKind::System, id: None }
    }
}

/// An item's identity in an audit row. Deliberately has no body field — the
/// type system is what enforces "audit never stores bodies".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRef {
    pub id: ItemId,
    pub digest: String,
}

impl ItemRef {
    pub fn from_item(item: &MemoryItem) -> Self {
        Self { id: item.id.clone(), digest: item.digest() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: AuditId,
    #[serde(with = "time::serde::timestamp")]
    pub at: OffsetDateTime,
    pub scope: Scope,
    pub event: AuditEvent,
    pub items: Vec<ItemRef>,
    pub assessment: Option<Assessment>,
    pub decision: Option<Decision>,
    pub actor: Actor,
}

impl AuditRecord {
    pub fn new(scope: Scope, event: AuditEvent, items: Vec<ItemRef>, actor: Actor) -> Self {
        Self {
            id: AuditId::new(),
            at: OffsetDateTime::now_utc(),
            scope,
            event,
            items,
            assessment: None,
            decision: None,
            actor,
        }
    }

    pub fn with_assessment(mut self, a: Assessment) -> Self {
        self.assessment = Some(a);
        self
    }

    pub fn with_decision(mut self, d: Decision) -> Self {
        self.decision = Some(d);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditFilter {
    /// Empty means "all events".
    pub events: Vec<AuditEvent>,
    pub item: Option<ItemId>,
    #[serde(with = "time::serde::timestamp::option")]
    pub since: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp::option")]
    pub until: Option<OffsetDateTime>,
    pub limit: usize,
}

impl Default for AuditFilter {
    fn default() -> Self {
        Self { events: vec![], item: None, since: None, until: None, limit: 100 }
    }
}
```

Add to `crates/memorysafe-core/src/lib.rs`:

```rust
pub mod audit;
pub use audit::{Actor, ActorKind, AuditEvent, AuditFilter, AuditRecord, ItemRef};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core audit`
Expected: PASS — 4 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): audit records that structurally cannot hold item bodies"
```

---

## Task 9: Core — embedding and recall types

**Files:**
- Create: `crates/memorysafe-core/src/embedding.rs`
- Create: `crates/memorysafe-core/src/recall.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: `Score`, `ItemId`, `MemoryItem`, `Scope`, `Reason`, `AuditId`, `SensitivityLevel`.
- Produces: `EmbedderId`, `Embedding`, `Embedding::is_comparable_to(&self, other)`, `RecallBudget`, `RecallMode`, `RecallRequest`, `ScoredCandidate`, `SelectedItem`, `OmittedItem`, `WorkingSet`, `OMITTED_CAP`.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/embedding.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn emb(id: &str, dim: u16) -> Embedding {
        Embedding { vector: vec![0.0; dim as usize], embedder: EmbedderId::new(id), dim }
    }

    #[test]
    fn vectors_from_different_embedders_are_not_comparable() {
        assert!(emb("model2vec-base", 256).is_comparable_to(&emb("model2vec-base", 256)));
        assert!(!emb("model2vec-base", 256).is_comparable_to(&emb("nomic-v1.5", 256)));
        assert!(!emb("model2vec-base", 256).is_comparable_to(&emb("model2vec-base", 384)));
    }
}
```

Append to `crates/memorysafe-core/src/recall.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_defaults_to_the_governed_working_set() {
        assert_eq!(RecallMode::default(), RecallMode::WorkingSet);
    }

    #[test]
    fn budget_tracks_both_tokens_and_items() {
        let b = RecallBudget { max_tokens: Some(2000), max_items: Some(10) };
        assert!(b.fits(1999, 9));
        assert!(!b.fits(2001, 9));
        assert!(!b.fits(1999, 11));
    }

    #[test]
    fn an_unbounded_budget_fits_anything() {
        let b = RecallBudget { max_tokens: None, max_items: None };
        assert!(b.fits(u32::MAX, usize::MAX));
    }

    #[test]
    fn omitted_list_is_capped_so_a_wide_recall_cannot_blow_up_the_response() {
        assert_eq!(OMITTED_CAP, 50);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core -- embedding recall`
Expected: FAIL — `cannot find type Embedding in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/embedding.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EmbedderId(String);

impl EmbedderId {
    pub fn new(raw: &str) -> Self {
        Self(raw.to_owned())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EmbedderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Embedding {
    pub vector: Vec<f32>,
    pub embedder: EmbedderId,
    pub dim: u16,
}

impl Embedding {
    /// Vectors from different models occupy different spaces. Comparing them
    /// produces silently meaningless similarities, so every comparison site
    /// must gate on this.
    pub fn is_comparable_to(&self, other: &Embedding) -> bool {
        self.embedder == other.embedder && self.dim == other.dim
    }
}
```

`crates/memorysafe-core/src/recall.rs`:

```rust
use crate::assessment::SensitivityLevel;
use crate::decision::Reason;
use crate::ids::{AuditId, ItemId, Scope};
use crate::item::MemoryItem;
use crate::score::Score;
use serde::{Deserialize, Serialize};

/// Cap on `WorkingSet::omitted`. A recall over a large corpus considers many
/// candidates; the response must stay bounded.
pub const OMITTED_CAP: usize = 50;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallMode {
    /// Governed composition: relevance, value, fragility, replay, diversity.
    #[default]
    WorkingSet,
    /// Raw ranked list. Still scope-filtered, still sensitivity-capped, still
    /// audited — it bypasses composition, never governance.
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallBudget {
    pub max_tokens: Option<u32>,
    pub max_items: Option<usize>,
}

impl Default for RecallBudget {
    fn default() -> Self {
        Self { max_tokens: Some(2000), max_items: Some(20) }
    }
}

impl RecallBudget {
    pub fn fits(&self, tokens: u32, items: usize) -> bool {
        self.max_tokens.is_none_or(|m| tokens <= m)
            && self.max_items.is_none_or(|m| items <= m)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallRequest {
    pub scope: Scope,
    pub query: Option<String>,
    pub tags_any: Vec<String>,
    pub kinds: Vec<String>,
    pub mode: RecallMode,
    pub budget: RecallBudget,
    /// The caller's clearance. Items above this level are excluded in SQL,
    /// below the policy.
    pub sensitivity_ceiling: SensitivityLevel,
}

/// A retrieval hit before composition. `relevance` fuses vector and keyword.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredCandidate {
    pub item: MemoryItem,
    pub relevance: f32,
    pub vector_score: Option<f32>,
    pub keyword_score: Option<f32>,
    pub value: Score,
    pub fragility: Score,
    pub estimated_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectedItem {
    pub item: MemoryItem,
    pub relevance: f32,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OmittedItem {
    pub id: ItemId,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkingSet {
    pub items: Vec<SelectedItem>,
    pub tokens_used: u32,
    /// Truncated to `OMITTED_CAP`.
    pub omitted: Vec<OmittedItem>,
    /// Set by the engine after `record_recall`; the policy leaves it `None`.
    pub audit_id: Option<AuditId>,
}

impl WorkingSet {
    pub fn empty() -> Self {
        Self { items: vec![], tokens_used: 0, omitted: vec![], audit_id: None }
    }
}
```

Add to `crates/memorysafe-core/src/lib.rs`:

```rust
pub mod embedding;
pub mod recall;
pub use embedding::{Embedding, EmbedderId};
pub use recall::{
    OMITTED_CAP, OmittedItem, RecallBudget, RecallMode, RecallRequest, ScoredCandidate,
    SelectedItem, WorkingSet,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core`
Expected: PASS — all core tests green (28 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): Embedding comparability guard and recall types"
```

---

## Task 10: Core — the `GovernancePolicy` trait and its contexts

**Files:**
- Create: `crates/memorysafe-core/src/policy.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`

**Interfaces:**
- Consumes: everything from Tasks 2–9.
- Produces: `PolicyError`, `Candidate`, `Assessed`, `AssessContext`, `AdmitContext`, `ComposeContext`, `MaintainContext`, `GovernancePolicy`.

**Spec constraint:** the trait is pure. Contexts are plain data carrying everything the policy needs — neighbours, capacity, scope statistics, and the clock. No context holds a handle, a connection, a channel, or a closure.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/policy.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::features;
    use crate::{Budget, CapacityState, ScopeStats, Scope};
    use time::OffsetDateTime;

    /// A policy that admits everything. Proves the trait is implementable with
    /// no I/O whatsoever.
    struct AlwaysAdmit;

    impl GovernancePolicy for AlwaysAdmit {
        fn id(&self) -> PolicyId {
            PolicyId::new("always-admit", "0.0.0")
        }
        fn assess(&self, _c: &Candidate, _ctx: &AssessContext) -> Result<Assessment, PolicyError> {
            Ok(Assessment {
                value: Score::ONE,
                fragility: Score::ZERO,
                sensitivity: SensitivityAssessment {
                    level: SensitivityLevel::Public,
                    categories: vec![],
                    confidence: Score::ONE,
                },
                redundancy: RedundancyAssessment { score: Score::ZERO, near_duplicates: vec![] },
                features: features! {},
                assessor: AssessorId::new("always-admit", "0.0.0"),
            })
        }
        fn admit(&self, _a: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
            Ok(Decision::retain(
                self.id(),
                Reason::new(ReasonCode::NovelContent, "always", features! {}),
            ))
        }
        fn compose(
            &self,
            _r: &RecallRequest,
            _c: &[ScoredCandidate],
            _ctx: &ComposeContext,
        ) -> Result<WorkingSet, PolicyError> {
            Ok(WorkingSet::empty())
        }
        fn maintain(&self, _ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
            Ok(vec![])
        }
    }

    fn assess_ctx() -> AssessContext {
        AssessContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            neighbours: vec![],
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn the_trait_is_implementable_without_io() {
        let p = AlwaysAdmit;
        let c = Candidate {
            body: "hello".into(),
            kind: "fact".into(),
            tags: vec![],
            attrs: Default::default(),
            sensitivity_hint: None,
            embedding: None,
            byte_size: 5,
        };
        let a = p.assess(&c, &assess_ctx()).unwrap();
        assert_eq!(a.value, Score::ONE);
        assert_eq!(p.id().to_string(), "always-admit@0.0.0");
    }

    #[test]
    fn policies_are_object_safe_so_the_engine_can_hold_a_boxed_one() {
        let boxed: Box<dyn GovernancePolicy> = Box::new(AlwaysAdmit);
        assert_eq!(boxed.id().name, "always-admit");
    }

    #[test]
    fn admit_context_exposes_capacity_without_exposing_the_backend() {
        let ctx = AdmitContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            capacity: CapacityState {
                budget: Budget { max_items: Some(10), max_bytes: None },
                used_items: 10,
                used_bytes: 0,
            },
            eviction_candidates: vec![],
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        assert_eq!(ctx.capacity.pressure(), 1.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-core policy`
Expected: FAIL — `cannot find trait GovernancePolicy in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-core/src/policy.rs`:

```rust
use crate::assessment::{
    Assessment, AssessorId, RedundancyAssessment, SensitivityAssessment, SensitivityLevel,
};
use crate::capacity::{CapacityState, ScopeStats};
use crate::decision::{Decision, PolicyId, Reason, ReasonCode};
use crate::embedding::Embedding;
use crate::ids::Scope;
use crate::item::MemoryItem;
use crate::recall::{RecallRequest, ScoredCandidate, WorkingSet};
use crate::score::Score;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("policy misconfigured: {0}")]
    Config(String),
    #[error("policy could not score candidate: {0}")]
    Scoring(String),
    #[error("policy requires an embedding but none was supplied")]
    MissingEmbedding,
}

/// A write candidate before it becomes a `MemoryItem`. Carries no id, no
/// timestamps, and no protection — those are engine-assigned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub body: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, serde_json::Value>,
    pub sensitivity_hint: Option<SensitivityLevel>,
    /// `None` when the embedder was unavailable. `assess` must still succeed.
    pub embedding: Option<Embedding>,
    pub byte_size: u64,
}

/// A candidate paired with the assessment `assess` produced for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessed<'a> {
    pub candidate: &'a Candidate,
    pub assessment: &'a Assessment,
}

/// Everything `assess` may look at. Plain data; no handles, no closures.
#[derive(Debug, Clone, PartialEq)]
pub struct AssessContext {
    pub scope: Scope,
    /// Nearest neighbours in the scope, descending by similarity. Empty when
    /// the candidate has no embedding.
    pub neighbours: Vec<ScoredCandidate>,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdmitContext {
    pub scope: Scope,
    pub capacity: CapacityState,
    /// Items the engine offers as evictable, cheapest-to-lose first. Already
    /// excludes pinned items and unexpired protection windows.
    pub eviction_candidates: Vec<ScoredCandidate>,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComposeContext {
    pub scope: Scope,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaintainContext {
    pub scope: Scope,
    /// One page of the scope's items. Maintenance is a resumable job; the
    /// engine pages through and calls `maintain` per batch.
    pub batch: Vec<MemoryItem>,
    pub capacity: CapacityState,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

/// The seam. Pure: the engine performs all I/O and hands in everything above.
pub trait GovernancePolicy: Send + Sync {
    fn id(&self) -> PolicyId;

    fn assess(&self, cand: &Candidate, ctx: &AssessContext) -> Result<Assessment, PolicyError>;

    fn admit(&self, assessed: &Assessed, ctx: &AdmitContext) -> Result<Decision, PolicyError>;

    fn compose(
        &self,
        req: &RecallRequest,
        candidates: &[ScoredCandidate],
        ctx: &ComposeContext,
    ) -> Result<WorkingSet, PolicyError>;

    fn maintain(&self, ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError>;
}
```

Add to `crates/memorysafe-core/src/lib.rs`:

```rust
pub mod policy;
pub use policy::{
    AdmitContext, AssessContext, Assessed, Candidate, ComposeContext, GovernancePolicy,
    MaintainContext, PolicyError,
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-core && cargo tree -p memorysafe-core --edges normal --prefix none | sort -u`
Expected: PASS — all core tests green. The dependency listing shows only `serde`, `serde_json`, `thiserror`, `ulid`, `time`, `blake3` and their transitive deps — no `tokio`, no `rusqlite`.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-core/src/
git commit -m "feat(core): GovernancePolicy trait with pure, data-only contexts"
```

---

## Task 11: Embed — `Embedder` trait and `DeterministicEmbedder`

**Files:**
- Create: `crates/memorysafe-embed/Cargo.toml`
- Create: `crates/memorysafe-embed/src/lib.rs`
- Create: `crates/memorysafe-embed/src/test_embedder.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `Embedding`, `EmbedderId` from Task 9.
- Produces: `EmbedError`, `Embedder` trait, `DeterministicEmbedder::new(dim: u16)`, `DeterministicEmbedder::default()` (dim 256).

**Why this exists:** the whole test suite must run with no model files and no network. Every later task's fixtures use this embedder.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-embed/src/test_embedder.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeddings_are_deterministic_across_instances() {
        let a = DeterministicEmbedder::new(256);
        let b = DeterministicEmbedder::new(256);
        assert_eq!(a.embed("hello").unwrap().vector, b.embed("hello").unwrap().vector);
    }

    #[test]
    fn different_text_yields_different_vectors() {
        let e = DeterministicEmbedder::new(256);
        assert_ne!(e.embed("hello").unwrap().vector, e.embed("world").unwrap().vector);
    }

    #[test]
    fn vectors_are_l2_normalised() {
        let e = DeterministicEmbedder::new(256);
        let v = e.embed("some memory about cats").unwrap().vector;
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn shared_tokens_produce_higher_similarity_than_disjoint_ones() {
        // Retrieval tests depend on this being a usable, if crude, semantic proxy.
        let e = DeterministicEmbedder::new(256);
        let cats1 = e.embed("the cat sat on the mat").unwrap();
        let cats2 = e.embed("the cat sat on a rug").unwrap();
        let cars = e.embed("quarterly revenue exceeded projections").unwrap();
        let sim = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let near = sim(&cats1.vector, &cats2.vector);
        let far = sim(&cats1.vector, &cars.vector);
        assert!(near > far, "near={near} far={far}");
    }

    #[test]
    fn empty_text_is_an_error_not_a_zero_vector() {
        let e = DeterministicEmbedder::new(256);
        assert!(matches!(e.embed(""), Err(EmbedError::EmptyInput)));
        assert!(matches!(e.embed("   "), Err(EmbedError::EmptyInput)));
    }

    #[test]
    fn the_embedder_reports_its_identity_and_dimension() {
        let e = DeterministicEmbedder::new(384);
        assert_eq!(e.dim(), 384);
        assert_eq!(e.id().as_str(), "deterministic-384");
        assert_eq!(e.embed("x").unwrap().embedder, e.id());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-embed`
Expected: FAIL — no such package `memorysafe-embed`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-embed/Cargo.toml`:

```toml
[package]
name = "memorysafe-embed"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[features]
default = []
model2vec = ["dep:model2vec-rs"]

[dependencies]
memorysafe-core.workspace = true
thiserror.workspace = true
blake3.workspace = true
model2vec-rs = { version = "0.2.1", optional = true }

[lints]
workspace = true
```

Add to the workspace `[workspace.dependencies]` in the root `Cargo.toml`:

```toml
memorysafe-embed = { path = "crates/memorysafe-embed" }
```

`crates/memorysafe-embed/src/lib.rs`:

```rust
//! Embedding generation and int8 quantization.

pub mod quantize;
pub mod test_embedder;

#[cfg(feature = "model2vec")]
pub mod model2vec;

use memorysafe_core::{EmbedderId, Embedding};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EmbedError {
    #[error("cannot embed empty or whitespace-only text")]
    EmptyInput,
    #[error("embedding model unavailable: {0}")]
    Unavailable(String),
    #[error("model returned {got} dimensions, expected {expected}")]
    DimensionMismatch { got: usize, expected: u16 },
}

pub trait Embedder: Send + Sync {
    fn id(&self) -> EmbedderId;
    fn dim(&self) -> u16;
    fn embed(&self, text: &str) -> Result<Embedding, EmbedError>;
}

pub use quantize::QuantizedVector;
pub use test_embedder::DeterministicEmbedder;
```

`crates/memorysafe-embed/src/test_embedder.rs`:

```rust
use crate::{EmbedError, Embedder};
use memorysafe_core::{EmbedderId, Embedding};

/// A bag-of-tokens hash embedder. Not semantic in any real sense, but
/// deterministic, dependency-free, and monotone in token overlap — which is
/// exactly what the test suite needs and nothing more.
pub struct DeterministicEmbedder {
    dim: u16,
    id: EmbedderId,
}

impl DeterministicEmbedder {
    pub fn new(dim: u16) -> Self {
        assert!(dim > 0, "dim must be positive");
        Self { dim, id: EmbedderId::new(&format!("deterministic-{dim}")) }
    }
}

impl Default for DeterministicEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

impl Embedder for DeterministicEmbedder {
    fn id(&self) -> EmbedderId {
        self.id.clone()
    }

    fn dim(&self) -> u16 {
        self.dim
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        if tokens.is_empty() {
            return Err(EmbedError::EmptyInput);
        }

        let n = self.dim as usize;
        let mut v = vec![0.0f32; n];

        for token in tokens {
            let lowered = token.to_lowercase();
            let hash = blake3::hash(lowered.as_bytes());
            let bytes = hash.as_bytes();
            // Two independent draws per token: an index and a sign, so tokens
            // spread across dimensions instead of all landing positive.
            let index = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize % n;
            let sign = if bytes[4] & 1 == 0 { 1.0 } else { -1.0 };
            v[index] += sign;
        }

        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        } else {
            // Every token cancelled out. Pin to a fixed unit vector so the
            // result stays normalised and deterministic.
            v[0] = 1.0;
        }

        Ok(Embedding { vector: v, embedder: self.id(), dim: self.dim })
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-embed test_embedder`
Expected: PASS — 6 tests ok.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-embed/
git commit -m "feat(embed): Embedder trait and deterministic test embedder"
```

---

## Task 12: Embed — quantization and dot product

**Files:**
- Create: `crates/memorysafe-embed/src/quantize.rs`
- Modify: `crates/memorysafe-embed/src/lib.rs` (already declares the module in Task 11)

**Interfaces:**
- Consumes: `Embedding`, `EmbedderId`.
- Produces: `QuantizedVector { embedder, dim, scale, q: Vec<i8> }`, `QuantizedVector::from_embedding(&Embedding)`, `QuantizedVector::dot(&self, other) -> f32`, `QuantizedVector::to_bytes()`, `QuantizedVector::from_bytes(embedder, dim, scale, &[u8])`.

**Note on "SIMD":** this is an autovectorizable scalar loop over `i32` accumulators, not hand-written intrinsics. `unsafe_code` is forbidden workspace-wide. Measure before reaching for anything more.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-embed/src/quantize.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeterministicEmbedder, Embedder};

    #[test]
    fn quantized_dot_approximates_cosine_of_normalised_vectors() {
        let e = DeterministicEmbedder::new(256);
        let a = e.embed("the cat sat on the mat").unwrap();
        let b = e.embed("the cat sat on a rug").unwrap();
        let exact: f32 = a.vector.iter().zip(&b.vector).map(|(x, y)| x * y).sum();

        let qa = QuantizedVector::from_embedding(&a);
        let qb = QuantizedVector::from_embedding(&b);
        let approx = qa.dot(&qb).unwrap();

        assert!((exact - approx).abs() < 0.02, "exact={exact} approx={approx}");
    }

    #[test]
    fn identical_vectors_score_near_one() {
        let e = DeterministicEmbedder::new(256);
        let a = e.embed("identical text").unwrap();
        let q = QuantizedVector::from_embedding(&a);
        let d = q.dot(&q).unwrap();
        assert!((d - 1.0).abs() < 0.02, "self-similarity was {d}");
    }

    #[test]
    fn vectors_from_different_embedders_refuse_to_compare() {
        let a = QuantizedVector::from_embedding(&DeterministicEmbedder::new(256).embed("x").unwrap());
        let b = QuantizedVector::from_embedding(&DeterministicEmbedder::new(384).embed("x").unwrap());
        assert!(matches!(a.dot(&b), Err(QuantizeError::NotComparable { .. })));
    }

    #[test]
    fn bytes_round_trip_exactly() {
        let e = DeterministicEmbedder::new(256);
        let q = QuantizedVector::from_embedding(&e.embed("round trip me").unwrap());
        let bytes = q.to_bytes();
        assert_eq!(bytes.len(), 256);
        let back = QuantizedVector::from_bytes(q.embedder.clone(), q.dim, q.scale, &bytes).unwrap();
        assert_eq!(back.q, q.q);
        assert_eq!(back.dot(&q).unwrap(), q.dot(&q).unwrap());
    }

    #[test]
    fn from_bytes_rejects_a_length_that_contradicts_dim() {
        let e = DeterministicEmbedder::new(256);
        let q = QuantizedVector::from_embedding(&e.embed("x").unwrap());
        let err = QuantizedVector::from_bytes(q.embedder.clone(), 256, q.scale, &[0u8; 10]);
        assert!(matches!(err, Err(QuantizeError::LengthMismatch { .. })));
    }

    #[test]
    fn an_all_zero_vector_quantizes_without_dividing_by_zero() {
        let z = memorysafe_core::Embedding {
            vector: vec![0.0; 8],
            embedder: memorysafe_core::EmbedderId::new("z"),
            dim: 8,
        };
        let q = QuantizedVector::from_embedding(&z);
        assert_eq!(q.dot(&q).unwrap(), 0.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-embed quantize`
Expected: FAIL — `cannot find type QuantizedVector in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-embed/src/quantize.rs`:

```rust
use memorysafe_core::{EmbedderId, Embedding};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum QuantizeError {
    #[error("vectors are not comparable: {left} != {right}")]
    NotComparable { left: String, right: String },
    #[error("byte length {got} does not match dim {dim}")]
    LengthMismatch { got: usize, dim: u16 },
}

/// Symmetric int8 quantization of an L2-normalised embedding. Because inputs
/// are unit vectors, the dot product of two quantized vectors approximates
/// their cosine similarity directly.
#[derive(Debug, Clone, PartialEq)]
pub struct QuantizedVector {
    pub embedder: EmbedderId,
    pub dim: u16,
    pub scale: f32,
    pub q: Vec<i8>,
}

impl QuantizedVector {
    pub fn from_embedding(e: &Embedding) -> Self {
        let max_abs = e.vector.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        // A zero vector has no scale; use 1.0 so quantization is a no-op and
        // every dot product involving it is exactly zero.
        let scale = if max_abs > 0.0 { max_abs / 127.0 } else { 1.0 };
        let q = e
            .vector
            .iter()
            .map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8)
            .collect();
        Self { embedder: e.embedder.clone(), dim: e.dim, scale, q }
    }

    pub fn dot(&self, other: &QuantizedVector) -> Result<f32, QuantizeError> {
        if self.embedder != other.embedder || self.dim != other.dim {
            return Err(QuantizeError::NotComparable {
                left: format!("{}:{}", self.embedder, self.dim),
                right: format!("{}:{}", other.embedder, other.dim),
            });
        }
        // i32 accumulator: 127*127*65535 fits comfortably. Chunked to help
        // the autovectorizer; no intrinsics, no unsafe.
        let mut acc: i32 = 0;
        let (a, b) = (&self.q, &other.q);
        let chunks = a.len() / 16;
        for i in 0..chunks {
            let (x, y) = (&a[i * 16..i * 16 + 16], &b[i * 16..i * 16 + 16]);
            for k in 0..16 {
                acc += i32::from(x[k]) * i32::from(y[k]);
            }
        }
        for i in chunks * 16..a.len() {
            acc += i32::from(a[i]) * i32::from(b[i]);
        }
        Ok(acc as f32 * self.scale * other.scale)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.q.iter().map(|v| *v as u8).collect()
    }

    pub fn from_bytes(
        embedder: EmbedderId,
        dim: u16,
        scale: f32,
        bytes: &[u8],
    ) -> Result<Self, QuantizeError> {
        if bytes.len() != dim as usize {
            return Err(QuantizeError::LengthMismatch { got: bytes.len(), dim });
        }
        Ok(Self { embedder, dim, scale, q: bytes.iter().map(|b| *b as i8).collect() })
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-embed`
Expected: PASS — 12 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-embed/src/
git commit -m "feat(embed): int8 quantization with comparability-checked dot product"
```

---

## Task 13: Embed — `Model2VecEmbedder`

**Files:**
- Create: `crates/memorysafe-embed/src/model2vec.rs`
- Modify: `crates/memorysafe-embed/src/lib.rs` (module already declared behind the feature in Task 11)

**Interfaces:**
- Consumes: `Embedder`, `EmbedError`.
- Produces: `Model2VecEmbedder::from_pretrained(path: &Path, id: &str)`, behind feature `model2vec`.

**Note:** this is the only task in Plan 1 that touches a model file, and it is feature-gated and not in the default CI job. Its test is `#[ignore]`d so the suite stays hermetic; a separate CI job runs it.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-embed/src/model2vec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_model_path_is_unavailable_not_a_panic() {
        let err = Model2VecEmbedder::from_pretrained(
            std::path::Path::new("/nonexistent/model"),
            "potion-base-8M",
        );
        assert!(matches!(err, Err(EmbedError::Unavailable(_))));
    }

    #[test]
    #[ignore = "requires a downloaded model; run with --ignored in the model CI job"]
    fn real_embeddings_are_normalised_and_semantic() {
        let path = std::env::var("MEMORYSAFE_MODEL_PATH").expect("set MEMORYSAFE_MODEL_PATH");
        let e = Model2VecEmbedder::from_pretrained(
            std::path::Path::new(&path),
            "potion-base-8M",
        )
        .unwrap();

        let v = e.embed("the cat sat on the mat").unwrap();
        let norm: f32 = v.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
        assert_eq!(v.vector.len(), e.dim() as usize);

        let sim = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let cat = e.embed("a small domestic cat").unwrap();
        let kitten = e.embed("a young kitten").unwrap();
        let finance = e.embed("quarterly revenue exceeded projections").unwrap();
        assert!(sim(&cat.vector, &kitten.vector) > sim(&cat.vector, &finance.vector));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-embed --features model2vec model2vec`
Expected: FAIL — `cannot find type Model2VecEmbedder in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-embed/src/model2vec.rs`:

```rust
use crate::{EmbedError, Embedder};
use memorysafe_core::{EmbedderId, Embedding};
use model2vec_rs::model::StaticModel;
use std::path::Path;

/// Static distilled embeddings. No ONNX runtime, no GPU, microsecond-scale
/// encoding — cheap enough to run inline on every write, which the
/// caller-authored-item design requires.
pub struct Model2VecEmbedder {
    model: StaticModel,
    id: EmbedderId,
    dim: u16,
}

impl Model2VecEmbedder {
    pub fn from_pretrained(path: &Path, id: &str) -> Result<Self, EmbedError> {
        if !path.exists() {
            return Err(EmbedError::Unavailable(format!("no model at {}", path.display())));
        }
        let model = StaticModel::from_pretrained(path, None, None, None)
            .map_err(|e| EmbedError::Unavailable(e.to_string()))?;
        let probe = model.encode_single("dimension probe");
        let dim = u16::try_from(probe.len())
            .map_err(|_| EmbedError::Unavailable("model dimension exceeds u16".into()))?;
        Ok(Self { model, id: EmbedderId::new(id), dim })
    }
}

impl Embedder for Model2VecEmbedder {
    fn id(&self) -> EmbedderId {
        self.id.clone()
    }

    fn dim(&self) -> u16 {
        self.dim
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        if text.trim().is_empty() {
            return Err(EmbedError::EmptyInput);
        }
        let mut v = self.model.encode_single(text);
        if v.len() != self.dim as usize {
            return Err(EmbedError::DimensionMismatch { got: v.len(), expected: self.dim });
        }
        // Quantization assumes unit vectors; normalise here so every Embedder
        // upholds the same contract.
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        } else {
            return Err(EmbedError::Unavailable("model produced a zero vector".into()));
        }
        Ok(Embedding { vector: v, embedder: self.id(), dim: self.dim })
    }
}
```

Add the model job to `.github/workflows/ci.yml`:

```yaml
  model:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.97.1
      - run: cargo test -p memorysafe-embed --features model2vec
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-embed --features model2vec`
Expected: PASS — 13 tests ok, 1 ignored.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-embed/ .github/workflows/ci.yml
git commit -m "feat(embed): optional model2vec embedder behind a feature flag"
```

---
