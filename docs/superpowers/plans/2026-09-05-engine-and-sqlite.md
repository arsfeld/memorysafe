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

## Task 14: Backend — the trait and its query/write types

**Files:**
- Create: `crates/memorysafe-backend/Cargo.toml`
- Create: `crates/memorysafe-backend/src/lib.rs`
- Create: `crates/memorysafe-backend/src/query.rs`
- Create: `crates/memorysafe-backend/src/write.rs`
- Create: `crates/memorysafe-backend/src/portability.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: all core types.
- Produces: `BackendError`, `Backend` trait (exact signatures in the plan header), `CandidateQuery`, `HardFilters`, `Page`, `WriteTransaction`, `ItemWrite`, `MergeWrite`, `AppliedWrite`, `PurgeReport`, `ScopeSelector`, `ExportStream`, `ImportStream`, `ImportReport`, `ExportRecord`.

**Spec constraints encoded here:** hard filters live in `CandidateQuery`, not in a post-filter; `WriteTransaction` bundles item + evictions + audit so a backend cannot apply them separately; `ItemWrite` carries the quantized vector so the backend never computes embeddings.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-backend/src/query.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::SensitivityLevel;

    #[test]
    fn default_filters_admit_nothing_above_internal() {
        // Fail closed: a caller that forgets to set a ceiling gets the
        // conservative one, not Restricted.
        let f = HardFilters::default();
        assert_eq!(f.sensitivity_ceiling, SensitivityLevel::Internal);
        assert!(f.tags_any.is_empty());
        assert!(f.kinds.is_empty());
    }

    #[test]
    fn a_query_must_carry_a_vector_or_text_or_both() {
        let empty = CandidateQuery {
            embedding: None,
            text: None,
            filters: HardFilters::default(),
            limit: 10,
        };
        assert!(!empty.is_valid());

        let text_only = CandidateQuery {
            embedding: None,
            text: Some("cats".into()),
            filters: HardFilters::default(),
            limit: 10,
        };
        assert!(text_only.is_valid());
    }

    #[test]
    fn pages_clamp_to_a_sane_maximum() {
        assert_eq!(Page { offset: 0, limit: 100_000 }.effective_limit(), MAX_PAGE_LIMIT);
        assert_eq!(Page { offset: 0, limit: 25 }.effective_limit(), 25);
        assert_eq!(Page::default().limit, 50);
    }
}
```

Append to `crates/memorysafe-backend/src/write.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::{Actor, AuditEvent, AuditRecord, Scope};

    fn audit() -> AuditRecord {
        AuditRecord::new(
            Scope::new("t", "s", "n").unwrap(),
            AuditEvent::Admitted,
            vec![],
            Actor::system(),
        )
    }

    #[test]
    fn a_write_transaction_always_carries_exactly_one_audit_record() {
        let txn = WriteTransaction::new(Scope::new("t", "s", "n").unwrap(), audit());
        assert!(txn.upsert.is_none());
        assert!(txn.merge.is_none());
        assert!(txn.evictions.is_empty());
        assert_eq!(txn.audit.event, AuditEvent::Admitted);
    }

    #[test]
    fn upsert_and_merge_are_mutually_exclusive() {
        let mut txn = WriteTransaction::new(Scope::new("t", "s", "n").unwrap(), audit());
        txn.merge = Some(MergeWrite {
            target: memorysafe_core::ItemId::new(),
            body: "merged".into(),
            tags: vec![],
            attrs: Default::default(),
            vector: None,
            byte_size: 6,
        });
        assert!(txn.is_valid());
        txn.upsert = Some(ItemWrite {
            item: None,
            vector: None,
        });
        assert!(!txn.is_valid(), "a transaction may not both insert and merge");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend`
Expected: FAIL — no such package `memorysafe-backend`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend/Cargo.toml`:

```toml
[package]
name = "memorysafe-backend"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
memorysafe-core.workspace = true
memorysafe-embed.workspace = true
async-trait = "0.1.92"
thiserror.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true
tokio = { version = "1.53.1", features = ["rt", "macros", "sync"] }

[lints]
workspace = true
```

Add to workspace `[workspace.dependencies]`:

```toml
memorysafe-backend = { path = "crates/memorysafe-backend" }
```

`crates/memorysafe-backend/src/query.rs`:

```rust
use memorysafe_core::SensitivityLevel;
use memorysafe_core::Embedding;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub const MAX_PAGE_LIMIT: usize = 1000;

/// Filters that MUST be applied inside the backend's own query. A policy may
/// narrow a candidate set further but may never widen it, so anything
/// security-relevant belongs here rather than in `compose`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardFilters {
    pub tags_any: Vec<String>,
    pub kinds: Vec<String>,
    #[serde(with = "time::serde::timestamp::option")]
    pub occurred_after: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp::option")]
    pub occurred_before: Option<OffsetDateTime>,
    /// Items strictly above this level are excluded in SQL.
    pub sensitivity_ceiling: SensitivityLevel,
    /// Exclude items whose embedding has not been backfilled yet.
    pub exclude_pending_embedding: bool,
}

impl Default for HardFilters {
    fn default() -> Self {
        Self {
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            // Fail closed.
            sensitivity_ceiling: SensitivityLevel::Internal,
            exclude_pending_embedding: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateQuery {
    pub embedding: Option<Embedding>,
    pub text: Option<String>,
    pub filters: HardFilters,
    /// Over-fetch limit. The engine typically sets 5–10x the recall budget.
    pub limit: usize,
}

impl CandidateQuery {
    pub fn is_valid(&self) -> bool {
        self.embedding.is_some() || self.text.as_ref().is_some_and(|t| !t.trim().is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub offset: usize,
    pub limit: usize,
}

impl Default for Page {
    fn default() -> Self {
        Self { offset: 0, limit: 50 }
    }
}

impl Page {
    pub fn effective_limit(&self) -> usize {
        self.limit.min(MAX_PAGE_LIMIT)
    }
}
```

`crates/memorysafe-backend/src/write.rs`:

```rust
use memorysafe_core::{AuditRecord, ItemId, MemoryItem, Scope};
use memorysafe_embed::QuantizedVector;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An item plus its already-computed vector. Backends never embed.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemWrite {
    pub item: Option<MemoryItem>,
    pub vector: Option<QuantizedVector>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeWrite {
    pub target: ItemId,
    pub body: String,
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, serde_json::Value>,
    pub vector: Option<QuantizedVector>,
    pub byte_size: u64,
}

/// One atomic unit of change. Item write, evictions, and the audit record
/// commit together or not at all — the backend has no API for doing them
/// separately.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteTransaction {
    pub scope: Scope,
    pub upsert: Option<ItemWrite>,
    pub merge: Option<MergeWrite>,
    pub evictions: Vec<ItemId>,
    pub audit: AuditRecord,
    pub idempotency_key: Option<String>,
    pub payload_digest: Option<String>,
}

impl WriteTransaction {
    pub fn new(scope: Scope, audit: AuditRecord) -> Self {
        Self {
            scope,
            upsert: None,
            merge: None,
            evictions: vec![],
            audit,
            idempotency_key: None,
            payload_digest: None,
        }
    }

    pub fn is_valid(&self) -> bool {
        !(self.upsert.is_some() && self.merge.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedWrite {
    pub item_id: Option<ItemId>,
    pub audit_id: memorysafe_core::AuditId,
    pub evicted: Vec<ItemId>,
    /// True when an idempotency key matched and the stored outcome was
    /// returned instead of applying anything.
    pub replayed: bool,
    pub replayed_outcome: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeReport {
    pub items_removed: u64,
    pub vectors_removed: u64,
    pub audit_rows_removed: u64,
    pub audit_rows_preserved: u64,
}
```

`crates/memorysafe-backend/src/portability.rs`:

```rust
use memorysafe_core::{AuditRecord, MemoryItem, Namespace, SubjectId, TenantId};
use memorysafe_embed::QuantizedVector;
use serde::{Deserialize, Serialize};

/// Selects what to export. `None` on a level means "all of them".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSelector {
    pub tenant: TenantId,
    pub subject: Option<SubjectId>,
    pub namespace: Option<Namespace>,
    pub include_audit: bool,
}

/// One line of the export stream. Newline-delimited JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum ExportRecord {
    Header { format_version: u32, exported_at: i64 },
    Item {
        item: Box<MemoryItem>,
        #[serde(skip_serializing_if = "Option::is_none")]
        vector: Option<ExportVector>,
    },
    Audit { record: Box<AuditRecord> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportVector {
    pub embedder: String,
    pub dim: u16,
    pub scale: f32,
    /// Base64 of the int8 bytes.
    pub q_base64: String,
}

impl ExportVector {
    pub fn from_quantized(q: &QuantizedVector) -> Self {
        use base64::Engine as _;
        Self {
            embedder: q.embedder.to_string(),
            dim: q.dim,
            scale: q.scale,
            q_base64: base64::engine::general_purpose::STANDARD.encode(q.to_bytes()),
        }
    }
}

pub type ExportStream = Vec<ExportRecord>;
pub type ImportStream = Vec<ExportRecord>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImportReport {
    pub items_imported: u64,
    pub vectors_imported: u64,
    pub audit_imported: u64,
    pub items_skipped_existing: u64,
}
```

Add `base64 = "0.22"` to `[workspace.dependencies]` and to `memorysafe-backend`'s `[dependencies]` as `base64.workspace = true`.

`crates/memorysafe-backend/src/lib.rs`:

```rust
//! The storage seam. One trait covering persistence and retrieval, because
//! pgvector searches inside the database and a separate index trait would
//! bake the SQLite shape into the interface.

pub mod conformance;
pub mod portability;
pub mod query;
pub mod write;

use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, CapacityState, Embedding, ItemId, MemoryItem, ScopeStats,
    Scope, ScoredCandidate, SubjectId, TenantId,
};
use thiserror::Error;

pub use portability::{
    ExportRecord, ExportStream, ExportVector, ImportReport, ImportStream, ScopeSelector,
};
pub use query::{CandidateQuery, HardFilters, MAX_PAGE_LIMIT, Page};
pub use write::{AppliedWrite, ItemWrite, MergeWrite, PurgeReport, WriteTransaction};

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("storage failure: {message} (retryable: {retryable})")]
    Storage { message: String, retryable: bool },
    #[error("item {0} not found")]
    ItemNotFound(ItemId),
    #[error("merge target {0} does not exist")]
    MergeTargetMissing(ItemId),
    #[error("idempotency key reused with a different payload")]
    IdempotencyConflict,
    #[error("query is invalid: {0}")]
    InvalidQuery(String),
    #[error("transaction is invalid: {0}")]
    InvalidTransaction(String),
    #[error("vector uses embedder {got}, scope uses {expected}")]
    EmbedderMismatch { got: String, expected: String },
    #[error("import stream is malformed: {0}")]
    MalformedImport(String),
}

#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    async fn retrieve_candidates(
        &self,
        scope: &Scope,
        query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

    async fn neighbours(
        &self,
        scope: &Scope,
        embedding: &Embedding,
        k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, BackendError>;

    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, BackendError>;

    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError>;

    async fn record_recall(&self, record: AuditRecord) -> Result<AuditId, BackendError>;

    async fn get(&self, scope: &Scope, id: &ItemId)
        -> Result<Option<MemoryItem>, BackendError>;

    async fn list(&self, scope: &Scope, page: &Page)
        -> Result<Vec<MemoryItem>, BackendError>;

    async fn audit(&self, scope: &Scope, filter: &AuditFilter)
        -> Result<Vec<AuditRecord>, BackendError>;

    async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<PurgeReport, BackendError>;

    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError>;

    async fn import(&self, stream: ImportStream) -> Result<ImportReport, BackendError>;

    /// Set a namespace's budget. Used by the conformance suite and by admin APIs.
    async fn set_budget(
        &self,
        scope: &Scope,
        budget: memorysafe_core::Budget,
    ) -> Result<(), BackendError>;
}
```

Create a stub `crates/memorysafe-backend/src/conformance/mod.rs` so the crate compiles:

```rust
//! The conformance suite. Every backend must pass it unmodified.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend`
Expected: PASS — 5 tests ok.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-backend/
git commit -m "feat(backend): Backend trait with atomic write transactions and hard filters"
```

---

## Task 15: Conformance — harness and isolation

**Files:**
- Modify: `crates/memorysafe-backend/src/conformance/mod.rs`
- Create: `crates/memorysafe-backend/src/conformance/fixtures.rs`
- Create: `crates/memorysafe-backend/src/conformance/isolation.rs`

**Interfaces:**
- Consumes: `Backend`, all core types, `DeterministicEmbedder`.
- Produces: `BackendFactory` trait, `run_conformance_suite<F: BackendFactory>(factory: F)`, and fixture builders `fx::item(scope, body)`, `fx::admit_txn(scope, item, vector)`, `fx::embedder()`.

**Why a factory:** each conformance test needs a pristine backend. The factory hands one out; the SQLite implementation returns a backend rooted in a fresh `TempDir`, and Plan 2's Postgres implementation returns one rooted in a fresh schema.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend/src/conformance/isolation.rs`:

```rust
use super::{BackendFactory, fx};
use memorysafe_core::{Page, Scope};

/// Two tenants writing identical content must never see each other's items.
pub async fn tenants_are_isolated<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let a = Scope::new("tenant-a", "sub", "ns").unwrap();
    let b = Scope::new("tenant-b", "sub", "ns").unwrap();

    let item_a = fx::item(&a, "tenant a private note");
    backend.apply(fx::admit_txn(&a, item_a.clone(), None)).await.unwrap();

    let listed_b = backend.list(&b, &Page::default()).await.unwrap();
    assert!(listed_b.is_empty(), "tenant b saw {} of tenant a's items", listed_b.len());

    assert!(
        backend.get(&b, &item_a.id).await.unwrap().is_none(),
        "tenant b fetched tenant a's item by id"
    );
}

/// Subjects within one tenant are the right-to-delete unit, so they must be
/// separated just as strictly.
pub async fn subjects_are_isolated<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let a = Scope::new("t", "subject-a", "ns").unwrap();
    let b = Scope::new("t", "subject-b", "ns").unwrap();

    let item_a = fx::item(&a, "subject a note");
    backend.apply(fx::admit_txn(&a, item_a.clone(), None)).await.unwrap();

    assert!(backend.list(&b, &Page::default()).await.unwrap().is_empty());
    assert!(backend.get(&b, &item_a.id).await.unwrap().is_none());
}

/// Namespaces are the budget and retrieval-default unit within a subject.
pub async fn namespaces_are_separated<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let a = Scope::new("t", "s", "ns-a").unwrap();
    let b = Scope::new("t", "s", "ns-b").unwrap();

    backend.apply(fx::admit_txn(&a, fx::item(&a, "in ns a"), None)).await.unwrap();
    backend.apply(fx::admit_txn(&b, fx::item(&b, "in ns b"), None)).await.unwrap();

    assert_eq!(backend.list(&a, &Page::default()).await.unwrap().len(), 1);
    assert_eq!(backend.list(&b, &Page::default()).await.unwrap().len(), 1);
}

/// An audit query is scoped too — one subject's decisions are not another's.
pub async fn audit_is_scoped<F: BackendFactory>(factory: &F) {
    use memorysafe_core::AuditFilter;
    let backend = factory.create().await;
    let a = Scope::new("t", "subject-a", "ns").unwrap();
    let b = Scope::new("t", "subject-b", "ns").unwrap();

    backend.apply(fx::admit_txn(&a, fx::item(&a, "note"), None)).await.unwrap();

    assert_eq!(backend.audit(&a, &AuditFilter::default()).await.unwrap().len(), 1);
    assert!(backend.audit(&b, &AuditFilter::default()).await.unwrap().is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend`
Expected: FAIL — `cannot find trait BackendFactory`, `unresolved module fx`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend/src/conformance/fixtures.rs`:

```rust
use crate::write::{ItemWrite, WriteTransaction};
use memorysafe_core::{
    Actor, AuditEvent, AuditRecord, ItemId, ItemRef, MemoryItem, Protection, Scope,
    SensitivityLevel, Source, SourceKind,
};
use memorysafe_embed::{DeterministicEmbedder, Embedder, QuantizedVector};
use time::OffsetDateTime;

/// The one embedder the whole suite uses. Deterministic, no model files.
pub fn embedder() -> DeterministicEmbedder {
    DeterministicEmbedder::new(256)
}

pub fn item(scope: &Scope, body: &str) -> MemoryItem {
    MemoryItem {
        id: ItemId::new(),
        scope: scope.clone(),
        body: body.to_string(),
        kind: "fact".into(),
        source: Source { kind: SourceKind::Agent, id: Some("conformance".into()) },
        occurred_at: None,
        created_at: OffsetDateTime::now_utc(),
        tags: vec![],
        attrs: Default::default(),
        sensitivity: SensitivityLevel::Internal,
        ttl: None,
        protection: Protection::Normal,
        pending_embedding: false,
    }
}

pub fn item_with(
    scope: &Scope,
    body: &str,
    kind: &str,
    tags: &[&str],
    sensitivity: SensitivityLevel,
) -> MemoryItem {
    let mut i = item(scope, body);
    i.kind = kind.to_string();
    i.tags = tags.iter().map(|t| t.to_string()).collect();
    i.sensitivity = sensitivity;
    i
}

pub fn vector_for(body: &str) -> QuantizedVector {
    QuantizedVector::from_embedding(&embedder().embed(body).unwrap())
}

/// A transaction that admits one item, with its audit record already attached.
pub fn admit_txn(
    scope: &Scope,
    item: MemoryItem,
    vector: Option<QuantizedVector>,
) -> WriteTransaction {
    let audit = AuditRecord::new(
        scope.clone(),
        AuditEvent::Admitted,
        vec![ItemRef::from_item(&item)],
        Actor::system(),
    );
    let mut txn = WriteTransaction::new(scope.clone(), audit);
    txn.upsert = Some(ItemWrite { item: Some(item), vector });
    txn
}

/// Same, but embeds the body so the item is vector-searchable.
pub fn admit_txn_embedded(scope: &Scope, item: MemoryItem) -> WriteTransaction {
    let v = vector_for(&item.body);
    admit_txn(scope, item, Some(v))
}

/// A transaction that evicts items without inserting anything.
pub fn evict_txn(scope: &Scope, evictions: Vec<ItemId>) -> WriteTransaction {
    let audit =
        AuditRecord::new(scope.clone(), AuditEvent::Forgotten, vec![], Actor::system());
    let mut txn = WriteTransaction::new(scope.clone(), audit);
    txn.evictions = evictions;
    txn
}
```

`crates/memorysafe-backend/src/conformance/mod.rs`:

```rust
//! The conformance suite. Every backend must pass it unmodified.
//!
//! This is the load-bearing artifact of the two-backend design: without it,
//! SQLite and Postgres drift within a month and the `Backend` trait becomes a
//! lie. Backends call `run_conformance_suite` from their own integration test.

pub mod fixtures;
pub mod isolation;

pub use fixtures as fx;

use crate::Backend;
use std::future::Future;

/// Hands out a pristine backend per test. SQLite returns one rooted in a fresh
/// `TempDir`; Postgres will return one rooted in a fresh schema.
pub trait BackendFactory: Send + Sync {
    type B: Backend;
    fn create(&self) -> impl Future<Output = Self::B> + Send;
}

/// Runs every conformance test in order. Panics on the first failure with the
/// test's own assertion message.
pub async fn run_conformance_suite<F: BackendFactory>(factory: &F) {
    macro_rules! run {
        ($($test:path),* $(,)?) => {
            $(
                eprintln!("conformance: {}", stringify!($test));
                $test(factory).await;
            )*
        };
    }

    run!(
        isolation::tenants_are_isolated,
        isolation::subjects_are_isolated,
        isolation::namespaces_are_separated,
        isolation::audit_is_scoped,
    );
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend && cargo clippy -p memorysafe-backend --all-targets -- -D warnings`
Expected: PASS — the crate compiles and its own unit tests pass. The conformance functions have no backend to run against yet; Task 20 is where they first execute.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend/src/conformance/
git commit -m "feat(backend): conformance harness, fixtures, and isolation tests"
```

---

## Task 16: Conformance — atomicity and idempotency

**Files:**
- Create: `crates/memorysafe-backend/src/conformance/atomicity.rs`
- Modify: `crates/memorysafe-backend/src/conformance/mod.rs`

**Interfaces:**
- Consumes: `BackendFactory`, `fx`.
- Produces: `admit_evict_and_audit_commit_together`, `a_failed_transaction_leaves_no_trace`, `every_mutation_writes_exactly_one_audit_record`, `idempotent_writes_replay_the_original_outcome`, `idempotency_conflict_on_different_payload`.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend/src/conformance/atomicity.rs`:

```rust
use super::{BackendFactory, fx};
use crate::BackendError;
use memorysafe_core::{AuditFilter, ItemId, Page, Scope};

/// The item insert, the evictions, and the audit row must land together.
pub async fn admit_evict_and_audit_commit_together<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let old = fx::item(&scope, "the old memory");
    backend.apply(fx::admit_txn(&scope, old.clone(), None)).await.unwrap();

    let new = fx::item(&scope, "the new memory");
    let mut txn = fx::admit_txn(&scope, new.clone(), None);
    txn.evictions = vec![old.id.clone()];
    let applied = backend.apply(txn).await.unwrap();

    assert_eq!(applied.evicted, vec![old.id.clone()]);
    assert!(backend.get(&scope, &old.id).await.unwrap().is_none(), "eviction did not happen");
    assert!(backend.get(&scope, &new.id).await.unwrap().is_some(), "insert did not happen");

    let audit = backend.audit(&scope, &AuditFilter::default()).await.unwrap();
    assert_eq!(audit.len(), 2, "expected one audit row per apply");
}

/// A transaction naming a nonexistent merge target must change nothing.
pub async fn a_failed_transaction_leaves_no_trace<F: BackendFactory>(factory: &F) {
    use crate::write::MergeWrite;
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let existing = fx::item(&scope, "survivor");
    backend.apply(fx::admit_txn(&scope, existing.clone(), None)).await.unwrap();
    let audit_before = backend.audit(&scope, &AuditFilter::default()).await.unwrap().len();

    let ghost = ItemId::new();
    let mut txn = fx::admit_txn(&scope, fx::item(&scope, "doomed"), None);
    txn.upsert = None;
    txn.merge = Some(MergeWrite {
        target: ghost,
        body: "merged body".into(),
        tags: vec![],
        attrs: Default::default(),
        vector: None,
        byte_size: 11,
    });
    txn.evictions = vec![existing.id.clone()];

    let err = backend.apply(txn).await.unwrap_err();
    assert!(matches!(err, BackendError::MergeTargetMissing(_)), "got {err:?}");

    assert!(
        backend.get(&scope, &existing.id).await.unwrap().is_some(),
        "a failed transaction still evicted an item"
    );
    let audit_after = backend.audit(&scope, &AuditFilter::default()).await.unwrap().len();
    assert_eq!(audit_before, audit_after, "a failed transaction still wrote audit");
}

/// Invariant 4 from the spec, at the backend level.
pub async fn every_mutation_writes_exactly_one_audit_record<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for i in 0..5 {
        backend
            .apply(fx::admit_txn(&scope, fx::item(&scope, &format!("memory {i}")), None))
            .await
            .unwrap();
    }
    let items = backend.list(&scope, &Page::default()).await.unwrap();
    backend.apply(fx::evict_txn(&scope, vec![items[0].id.clone()])).await.unwrap();

    let audit = backend.audit(&scope, &AuditFilter { limit: 1000, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(audit.len(), 6, "5 admits + 1 eviction should be 6 audit rows");
}

/// A retried write returns the original outcome instead of admitting a
/// duplicate and evicting something to make room for it.
pub async fn idempotent_writes_replay_the_original_outcome<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let item = fx::item(&scope, "written once");
    let mut txn = fx::admit_txn(&scope, item.clone(), None);
    txn.idempotency_key = Some("key-1".into());
    txn.payload_digest = Some(item.digest());

    let first = backend.apply(txn.clone()).await.unwrap();
    assert!(!first.replayed);

    // Same key, same payload, but a fresh item id — as a real retry would look.
    let retry_item = {
        let mut i = fx::item(&scope, "written once");
        i.id = ItemId::new();
        i
    };
    let mut retry = fx::admit_txn(&scope, retry_item, None);
    retry.idempotency_key = Some("key-1".into());
    retry.payload_digest = Some(item.digest());

    let second = backend.apply(retry).await.unwrap();
    assert!(second.replayed, "retry was not recognised as a replay");
    assert_eq!(second.item_id, first.item_id, "replay returned a different item");

    assert_eq!(
        backend.list(&scope, &Page::default()).await.unwrap().len(),
        1,
        "the retry created a duplicate"
    );
}

/// Reusing a key with a different payload is a conflict, not a silent replay.
pub async fn idempotency_conflict_on_different_payload<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let first_item = fx::item(&scope, "original payload");
    let mut txn = fx::admit_txn(&scope, first_item.clone(), None);
    txn.idempotency_key = Some("key-2".into());
    txn.payload_digest = Some(first_item.digest());
    backend.apply(txn).await.unwrap();

    let other = fx::item(&scope, "a completely different payload");
    let mut conflicting = fx::admit_txn(&scope, other.clone(), None);
    conflicting.idempotency_key = Some("key-2".into());
    conflicting.payload_digest = Some(other.digest());

    let err = backend.apply(conflicting).await.unwrap_err();
    assert!(matches!(err, BackendError::IdempotencyConflict), "got {err:?}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend`
Expected: FAIL — `unresolved module atomicity` in `mod.rs`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/memorysafe-backend/src/conformance/mod.rs`:

```rust
pub mod atomicity;
```

and extend the `run!` invocation in `run_conformance_suite`:

```rust
    run!(
        isolation::tenants_are_isolated,
        isolation::subjects_are_isolated,
        isolation::namespaces_are_separated,
        isolation::audit_is_scoped,
        atomicity::admit_evict_and_audit_commit_together,
        atomicity::a_failed_transaction_leaves_no_trace,
        atomicity::every_mutation_writes_exactly_one_audit_record,
        atomicity::idempotent_writes_replay_the_original_outcome,
        atomicity::idempotency_conflict_on_different_payload,
    );
```

`WriteTransaction` must derive `Clone` for the retry test — confirm the derive added in Task 14 is present.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend && cargo clippy -p memorysafe-backend --all-targets -- -D warnings`
Expected: PASS — compiles clean.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend/src/conformance/
git commit -m "feat(backend): conformance tests for atomicity and idempotency"
```

---

## Task 17: Conformance — retrieval and capacity

**Files:**
- Create: `crates/memorysafe-backend/src/conformance/retrieval.rs`
- Create: `crates/memorysafe-backend/src/conformance/capacity.rs`
- Modify: `crates/memorysafe-backend/src/conformance/mod.rs`

**Interfaces:**
- Consumes: `BackendFactory`, `fx`.
- Produces: `sensitivity_ceiling_is_enforced_in_the_query`, `tag_and_kind_filters_narrow_results`, `vector_search_ranks_by_similarity`, `keyword_search_finds_exact_terms`, `hybrid_beats_either_alone`, `pagination_is_stable`, `pending_embedding_items_are_excluded_when_asked`, `capacity_accounting_tracks_items_and_bytes`, `concurrent_admits_do_not_double_count`, `eviction_releases_capacity`.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend/src/conformance/retrieval.rs`:

```rust
use super::{BackendFactory, fx};
use crate::query::{CandidateQuery, HardFilters};
use memorysafe_core::{Page, Scope, SensitivityLevel};
use memorysafe_embed::Embedder;

fn query(text: &str, filters: HardFilters) -> CandidateQuery {
    CandidateQuery {
        embedding: Some(fx::embedder().embed(text).unwrap()),
        text: Some(text.to_string()),
        filters,
        limit: 50,
    }
}

/// The security-critical one: a restricted item must never leave the database
/// for a caller cleared only to Personal.
pub async fn sensitivity_ceiling_is_enforced_in_the_query<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for (body, level) in [
        ("public announcement about cats", SensitivityLevel::Public),
        ("internal note about cats", SensitivityLevel::Internal),
        ("personal detail about cats", SensitivityLevel::Personal),
        ("restricted medical record about cats", SensitivityLevel::Restricted),
    ] {
        let item = fx::item_with(&scope, body, "fact", &[], level);
        backend.apply(fx::admit_txn_embedded(&scope, item)).await.unwrap();
    }

    let filters = HardFilters {
        sensitivity_ceiling: SensitivityLevel::Personal,
        ..Default::default()
    };
    let hits = backend.retrieve_candidates(&scope, &query("cats", filters)).await.unwrap();

    assert_eq!(hits.len(), 3, "expected Public, Internal, Personal only");
    for h in &hits {
        assert!(
            h.item.sensitivity <= SensitivityLevel::Personal,
            "leaked {:?}: {}",
            h.item.sensitivity,
            h.item.body
        );
    }
}

pub async fn tag_and_kind_filters_narrow_results<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let cases = [
        ("alpha about cats", "fact", vec!["work"]),
        ("beta about cats", "preference", vec!["work"]),
        ("gamma about cats", "fact", vec!["home"]),
    ];
    for (body, kind, tags) in cases {
        let item = fx::item_with(&scope, body, kind, &tags, SensitivityLevel::Internal);
        backend.apply(fx::admit_txn_embedded(&scope, item)).await.unwrap();
    }

    let by_kind = HardFilters { kinds: vec!["fact".into()], ..Default::default() };
    assert_eq!(
        backend.retrieve_candidates(&scope, &query("cats", by_kind)).await.unwrap().len(),
        2
    );

    let by_tag = HardFilters { tags_any: vec!["home".into()], ..Default::default() };
    assert_eq!(
        backend.retrieve_candidates(&scope, &query("cats", by_tag)).await.unwrap().len(),
        1
    );

    let both = HardFilters {
        kinds: vec!["fact".into()],
        tags_any: vec!["work".into()],
        ..Default::default()
    };
    assert_eq!(
        backend.retrieve_candidates(&scope, &query("cats", both)).await.unwrap().len(),
        1
    );
}

pub async fn vector_search_ranks_by_similarity<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for body in [
        "the cat sat on the mat",
        "the cat sat on a rug",
        "quarterly revenue exceeded projections",
    ] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let probe = fx::embedder().embed("the cat sat on the mat").unwrap();
    let hits = backend.neighbours(&scope, &probe, 3).await.unwrap();

    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].item.body, "the cat sat on the mat", "exact match should rank first");
    assert!(
        hits[0].relevance >= hits[1].relevance && hits[1].relevance >= hits[2].relevance,
        "neighbours must come back sorted descending"
    );
    assert_eq!(hits[2].item.body, "quarterly revenue exceeded projections");
}

pub async fn keyword_search_finds_exact_terms<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // A rare token a hash embedder will not usefully cluster.
    for body in ["the deployment used zstandard compression", "unrelated musings"] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let q = CandidateQuery {
        embedding: None,
        text: Some("zstandard".into()),
        filters: HardFilters::default(),
        limit: 10,
    };
    let hits = backend.retrieve_candidates(&scope, &q).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].item.body.contains("zstandard"));
    assert!(hits[0].keyword_score.is_some());
    assert!(hits[0].vector_score.is_none());
}

/// FTS5 syntax characters in user text must not become query operators.
pub async fn keyword_search_escapes_user_input<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, "a normal memory")))
        .await
        .unwrap();

    for hostile in ["\"", "OR 1=1", "a AND b", "NEAR/", "*", "(unbalanced"] {
        let q = CandidateQuery {
            embedding: None,
            text: Some(hostile.to_string()),
            filters: HardFilters::default(),
            limit: 10,
        };
        // Must not error and must not match everything.
        let hits = backend.retrieve_candidates(&scope, &q).await.unwrap();
        assert!(hits.len() <= 1, "hostile input {hostile:?} matched {} rows", hits.len());
    }
}

pub async fn hybrid_returns_both_signal_sources<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for body in ["the cat sat on the mat", "zstandard compression details"] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let hits = backend
        .retrieve_candidates(&scope, &query("the cat sat on the mat", HardFilters::default()))
        .await
        .unwrap();

    let exact = hits.iter().find(|h| h.item.body == "the cat sat on the mat").unwrap();
    assert!(exact.vector_score.is_some(), "vector score missing");
    assert!(exact.keyword_score.is_some(), "keyword score missing");
    assert!(exact.relevance > 0.0);
}

/// Two pages must not overlap or drop rows.
pub async fn pagination_is_stable<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for i in 0..25 {
        backend
            .apply(fx::admit_txn(&scope, fx::item(&scope, &format!("memory {i:02}")), None))
            .await
            .unwrap();
    }

    let p1 = backend.list(&scope, &Page { offset: 0, limit: 10 }).await.unwrap();
    let p2 = backend.list(&scope, &Page { offset: 10, limit: 10 }).await.unwrap();
    let p3 = backend.list(&scope, &Page { offset: 20, limit: 10 }).await.unwrap();

    assert_eq!((p1.len(), p2.len(), p3.len()), (10, 10, 5));

    let mut ids: Vec<_> = p1.iter().chain(&p2).chain(&p3).map(|i| i.id.clone()).collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "pages overlapped");
    assert_eq!(total, 25, "pages dropped rows");
}

pub async fn pending_embedding_items_are_excluded_when_asked<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let mut pending = fx::item(&scope, "written while the embedder was down, about cats");
    pending.pending_embedding = true;
    backend.apply(fx::admit_txn(&scope, pending.clone(), None)).await.unwrap();
    backend
        .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, "a normal memory about cats")))
        .await
        .unwrap();

    // Visible to review regardless.
    assert_eq!(backend.list(&scope, &Page::default()).await.unwrap().len(), 2);

    let filters = HardFilters { exclude_pending_embedding: true, ..Default::default() };
    let hits = backend.retrieve_candidates(&scope, &query("cats", filters)).await.unwrap();
    assert!(hits.iter().all(|h| !h.item.pending_embedding));
}

/// Vectors from a different embedder must be refused, not silently compared.
pub async fn cross_model_vectors_are_rejected<F: BackendFactory>(factory: &F) {
    use memorysafe_embed::DeterministicEmbedder;
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    backend
        .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, "stored with the 256-dim model")))
        .await
        .unwrap();

    let other = DeterministicEmbedder::new(384).embed("a probe from another model").unwrap();
    let result = backend.neighbours(&scope, &other, 5).await;

    match result {
        Err(crate::BackendError::EmbedderMismatch { .. }) => {}
        Ok(hits) => assert!(
            hits.is_empty(),
            "cross-model probe returned {} hits instead of erroring or returning nothing",
            hits.len()
        ),
        Err(e) => panic!("unexpected error {e:?}"),
    }
}
```

`crates/memorysafe-backend/src/conformance/capacity.rs`:

```rust
use super::{BackendFactory, fx};
use memorysafe_core::{Budget, Page, Scope};

pub async fn capacity_accounting_tracks_items_and_bytes<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(&scope, Budget { max_items: Some(100), max_bytes: Some(100_000) })
        .await
        .unwrap();

    let before = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(before.used_items, 0);
    assert_eq!(before.used_bytes, 0);

    let item = fx::item(&scope, "a memory of some length");
    let size = item.byte_size();
    backend.apply(fx::admit_txn(&scope, item, None)).await.unwrap();

    let after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(after.used_items, 1);
    assert_eq!(after.used_bytes, size);
    assert_eq!(after.budget.max_items, Some(100));
}

pub async fn eviction_releases_capacity<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend.set_budget(&scope, Budget { max_items: Some(10), max_bytes: None }).await.unwrap();

    for i in 0..3 {
        backend
            .apply(fx::admit_txn(&scope, fx::item(&scope, &format!("memory {i}")), None))
            .await
            .unwrap();
    }
    assert_eq!(backend.capacity_state(&scope).await.unwrap().used_items, 3);

    let items = backend.list(&scope, &Page::default()).await.unwrap();
    backend.apply(fx::evict_txn(&scope, vec![items[0].id.clone()])).await.unwrap();

    let after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(after.used_items, 2);
    assert_eq!(after.used_bytes, items[1].byte_size() + items[2].byte_size());
}

/// The correctness detail the spec calls out: without a lock on the accounting
/// row, concurrent admits both conclude there is room and the count drifts.
pub async fn concurrent_admits_do_not_double_count<F: BackendFactory>(factory: &F) {
    use std::sync::Arc;
    let backend = Arc::new(factory.create().await);
    let scope = Scope::new("t", "s", "n").unwrap();
    backend.set_budget(&scope, Budget { max_items: Some(1000), max_bytes: None }).await.unwrap();

    let mut handles = Vec::new();
    for i in 0..20 {
        let b = Arc::clone(&backend);
        let s = scope.clone();
        handles.push(tokio::spawn(async move {
            b.apply(fx::admit_txn(&s, fx::item(&s, &format!("concurrent {i}")), None)).await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }

    let state = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(state.used_items, 20, "capacity accounting drifted under concurrency");

    let listed = backend.list(&scope, &Page { offset: 0, limit: 100 }).await.unwrap();
    assert_eq!(listed.len() as u64, state.used_items, "accounting disagrees with reality");
}

pub async fn scope_stats_reflect_the_corpus<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    assert_eq!(backend.scope_stats(&scope).await.unwrap().item_count, 0);

    for body in ["first memory", "second memory", "third memory"] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let stats = backend.scope_stats(&scope).await.unwrap();
    assert_eq!(stats.item_count, 3);
    assert!(stats.total_bytes > 0);
    assert!(stats.median_item_bytes > 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend`
Expected: FAIL — `unresolved module retrieval` in `mod.rs`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/memorysafe-backend/src/conformance/mod.rs`:

```rust
pub mod capacity;
pub mod retrieval;
```

and extend `run!`:

```rust
        retrieval::sensitivity_ceiling_is_enforced_in_the_query,
        retrieval::tag_and_kind_filters_narrow_results,
        retrieval::vector_search_ranks_by_similarity,
        retrieval::keyword_search_finds_exact_terms,
        retrieval::keyword_search_escapes_user_input,
        retrieval::hybrid_returns_both_signal_sources,
        retrieval::pagination_is_stable,
        retrieval::pending_embedding_items_are_excluded_when_asked,
        retrieval::cross_model_vectors_are_rejected,
        capacity::capacity_accounting_tracks_items_and_bytes,
        capacity::eviction_releases_capacity,
        capacity::concurrent_admits_do_not_double_count,
        capacity::scope_stats_reflect_the_corpus,
```

Add `tokio` with the `rt-multi-thread` feature to `memorysafe-backend` for `tokio::spawn` in the concurrency test:

```toml
tokio = { version = "1.53.1", features = ["rt", "rt-multi-thread", "macros", "sync"] }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend && cargo clippy -p memorysafe-backend --all-targets -- -D warnings`
Expected: PASS — compiles clean.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend/
git commit -m "feat(backend): conformance tests for retrieval, filters, and capacity"
```

---

## Task 18: Conformance — lifecycle

**Files:**
- Create: `crates/memorysafe-backend/src/conformance/lifecycle.rs`
- Modify: `crates/memorysafe-backend/src/conformance/mod.rs`

**Interfaces:**
- Consumes: `BackendFactory`, `fx`.
- Produces: `audit_filter_narrows_by_event_and_time`, `purge_subject_removes_everything_for_that_subject`, `purge_subject_leaves_other_subjects_intact`, `export_import_round_trips_exactly`, `import_is_idempotent`.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend/src/conformance/lifecycle.rs`:

```rust
use super::{BackendFactory, fx};
use crate::portability::ScopeSelector;
use memorysafe_core::{AuditEvent, AuditFilter, Page, Scope, SubjectId, TenantId};

pub async fn audit_filter_narrows_by_event_and_time<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for i in 0..3 {
        backend
            .apply(fx::admit_txn(&scope, fx::item(&scope, &format!("memory {i}")), None))
            .await
            .unwrap();
    }
    let items = backend.list(&scope, &Page::default()).await.unwrap();
    backend.apply(fx::evict_txn(&scope, vec![items[0].id.clone()])).await.unwrap();

    let admits = backend
        .audit(&scope, &AuditFilter { events: vec![AuditEvent::Admitted], ..Default::default() })
        .await
        .unwrap();
    assert_eq!(admits.len(), 3);
    assert!(admits.iter().all(|r| r.event == AuditEvent::Admitted));

    let forgets = backend
        .audit(&scope, &AuditFilter { events: vec![AuditEvent::Forgotten], ..Default::default() })
        .await
        .unwrap();
    assert_eq!(forgets.len(), 1);

    let limited = backend
        .audit(&scope, &AuditFilter { limit: 2, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(limited.len(), 2);
    assert!(limited[0].at >= limited[1].at, "audit must come back newest first");
}

/// Right-to-delete must be total for the subject.
pub async fn purge_subject_removes_everything_for_that_subject<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let tenant = TenantId::new("t").unwrap();
    let subject = SubjectId::new("doomed").unwrap();
    let a = Scope::new("t", "doomed", "ns-a").unwrap();
    let b = Scope::new("t", "doomed", "ns-b").unwrap();

    for scope in [&a, &b] {
        for i in 0..3 {
            backend
                .apply(fx::admit_txn_embedded(scope, fx::item(scope, &format!("memory {i}"))))
                .await
                .unwrap();
        }
    }

    let report = backend.purge_subject(&tenant, &subject).await.unwrap();
    assert_eq!(report.items_removed, 6);
    assert_eq!(report.vectors_removed, 6);

    assert!(backend.list(&a, &Page::default()).await.unwrap().is_empty());
    assert!(backend.list(&b, &Page::default()).await.unwrap().is_empty());
    assert_eq!(backend.capacity_state(&a).await.unwrap().used_items, 0);
    assert_eq!(
        report.audit_rows_removed + report.audit_rows_preserved,
        6,
        "every audit row must be accounted for as removed or preserved"
    );
}

pub async fn purge_subject_leaves_other_subjects_intact<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let doomed = Scope::new("t", "doomed", "ns").unwrap();
    let keeper = Scope::new("t", "keeper", "ns").unwrap();

    backend.apply(fx::admit_txn(&doomed, fx::item(&doomed, "goes away"), None)).await.unwrap();
    backend.apply(fx::admit_txn(&keeper, fx::item(&keeper, "stays"), None)).await.unwrap();

    backend
        .purge_subject(&TenantId::new("t").unwrap(), &SubjectId::new("doomed").unwrap())
        .await
        .unwrap();

    assert!(backend.list(&doomed, &Page::default()).await.unwrap().is_empty());
    assert_eq!(backend.list(&keeper, &Page::default()).await.unwrap().len(), 1);
    assert_eq!(
        backend.audit(&keeper, &AuditFilter::default()).await.unwrap().len(),
        1,
        "purging one subject destroyed another's audit"
    );
}

/// Invariant 5 from the spec: export then import reproduces the corpus exactly.
pub async fn export_import_round_trips_exactly<F: BackendFactory>(factory: &F) {
    let source = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for (body, kind, tags) in [
        ("first memory about cats", "fact", vec!["work"]),
        ("second memory about dogs", "preference", vec!["home", "pets"]),
        ("third memory about zstandard", "procedure", vec![]),
    ] {
        let item = fx::item_with(
            &scope,
            body,
            kind,
            &tags,
            memorysafe_core::SensitivityLevel::Internal,
        );
        source.apply(fx::admit_txn_embedded(&scope, item)).await.unwrap();
    }

    let selector = ScopeSelector {
        tenant: TenantId::new("t").unwrap(),
        subject: None,
        namespace: None,
        include_audit: true,
    };
    let exported = source.export(&selector).await.unwrap();

    let target = factory.create().await;
    let report = target.import(exported.clone()).await.unwrap();
    assert_eq!(report.items_imported, 3);
    assert_eq!(report.vectors_imported, 3);

    let mut before = source.list(&scope, &Page { offset: 0, limit: 100 }).await.unwrap();
    let mut after = target.list(&scope, &Page { offset: 0, limit: 100 }).await.unwrap();
    before.sort_by(|a, b| a.id.cmp(&b.id));
    after.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(before, after, "round trip did not reproduce the items exactly");

    // Vectors survived: the same probe ranks the same way on both sides.
    use memorysafe_embed::Embedder;
    let probe = fx::embedder().embed("cats").unwrap();
    let src_hits = source.neighbours(&scope, &probe, 3).await.unwrap();
    let tgt_hits = target.neighbours(&scope, &probe, 3).await.unwrap();
    let ids = |v: &[memorysafe_core::ScoredCandidate]| {
        v.iter().map(|c| c.item.id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(&src_hits), ids(&tgt_hits), "vector ranking changed across the round trip");
}

/// Importing the same stream twice must not duplicate anything.
pub async fn import_is_idempotent<F: BackendFactory>(factory: &F) {
    let source = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    source
        .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, "only memory")))
        .await
        .unwrap();

    let selector = ScopeSelector {
        tenant: TenantId::new("t").unwrap(),
        subject: None,
        namespace: None,
        include_audit: false,
    };
    let exported = source.export(&selector).await.unwrap();

    let target = factory.create().await;
    let first = target.import(exported.clone()).await.unwrap();
    assert_eq!(first.items_imported, 1);

    let second = target.import(exported).await.unwrap();
    assert_eq!(second.items_imported, 0);
    assert_eq!(second.items_skipped_existing, 1);
    assert_eq!(target.list(&scope, &Page::default()).await.unwrap().len(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend`
Expected: FAIL — `unresolved module lifecycle` in `mod.rs`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/memorysafe-backend/src/conformance/mod.rs`:

```rust
pub mod lifecycle;
```

and extend `run!`:

```rust
        lifecycle::audit_filter_narrows_by_event_and_time,
        lifecycle::purge_subject_removes_everything_for_that_subject,
        lifecycle::purge_subject_leaves_other_subjects_intact,
        lifecycle::export_import_round_trips_exactly,
        lifecycle::import_is_idempotent,
```

The suite now stands at **22 conformance tests**. This set is frozen at the end of Task 24; Plan 2's Postgres backend must pass it unmodified.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend && cargo clippy -p memorysafe-backend --all-targets -- -D warnings`
Expected: PASS — compiles clean.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend/
git commit -m "feat(backend): conformance tests for audit, purge, and portability"
```

---

## Task 19: SQLite — schema and `TenantManager`

**Files:**
- Create: `crates/memorysafe-backend-sqlite/Cargo.toml`
- Create: `crates/memorysafe-backend-sqlite/src/lib.rs`
- Create: `crates/memorysafe-backend-sqlite/src/schema.rs`
- Create: `crates/memorysafe-backend-sqlite/src/tenant.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `TenantId`, `BackendError`.
- Produces: `SqliteBackend::open(root: PathBuf) -> SqliteBackend`, `SqliteBackend::with_max_open(root, n)`, `TenantManager::with_conn<T>(tenant, f)`, `TenantManager::with_write<T>(tenant, f)`, `schema::SCHEMA_VERSION`, `schema::initialise(&Connection)`.

**Design:** one database file per tenant, named `<tenant>.db` under the root. `TenantId` validation from Task 2 already forbids `/`, `..`, and control characters, so the name is safe as a path component. Reads run concurrently under WAL; writes for a given tenant are serialized through a per-tenant `tokio::sync::Mutex`, which is what makes the capacity accounting correct.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-backend-sqlite/src/tenant.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::TenantId;

    #[tokio::test]
    async fn each_tenant_gets_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);

        let a = TenantId::new("tenant-a").unwrap();
        let b = TenantId::new("tenant-b").unwrap();
        mgr.with_conn(&a, |c| { c.execute_batch("CREATE TABLE probe(x)")?; Ok(()) }).await.unwrap();
        mgr.with_conn(&b, |c| { c.execute_batch("CREATE TABLE probe(x)")?; Ok(()) }).await.unwrap();

        assert!(dir.path().join("tenant-a.db").exists());
        assert!(dir.path().join("tenant-b.db").exists());
    }

    #[tokio::test]
    async fn the_schema_is_installed_on_first_open() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
        let t = TenantId::new("t").unwrap();

        let version: i64 = mgr
            .with_conn(&t, |c| {
                Ok(c.query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| {
                    r.get::<_, String>(0)
                })?
                .parse()
                .unwrap())
            })
            .await
            .unwrap();
        assert_eq!(version, crate::schema::SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn reopening_a_tenant_does_not_wipe_it() {
        let dir = tempfile::tempdir().unwrap();
        let t = TenantId::new("t").unwrap();
        {
            let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
            mgr.with_conn(&t, |c| {
                c.execute("INSERT INTO meta(key,value) VALUES('probe','kept')", [])?;
                Ok(())
            })
            .await
            .unwrap();
        }
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
        let value: String = mgr
            .with_conn(&t, |c| {
                Ok(c.query_row("SELECT value FROM meta WHERE key='probe'", [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(value, "kept");
    }

    #[tokio::test]
    async fn the_pool_evicts_but_stays_correct_beyond_its_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 2);

        for i in 0..6 {
            let t = TenantId::new(&format!("tenant-{i}")).unwrap();
            mgr.with_conn(&t, |c| {
                c.execute("INSERT INTO meta(key,value) VALUES('probe','v')", [])?;
                Ok(())
            })
            .await
            .unwrap();
        }
        // The first tenant was evicted from the pool; its data must survive.
        let t0 = TenantId::new("tenant-0").unwrap();
        let value: String = mgr
            .with_conn(&t0, |c| {
                Ok(c.query_row("SELECT value FROM meta WHERE key='probe'", [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(value, "v");
    }

    #[tokio::test]
    async fn writes_to_one_tenant_are_serialized() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(TenantManager::new(dir.path().to_path_buf(), 8));
        let t = TenantId::new("t").unwrap();

        mgr.with_write(&t, |c| {
            c.execute_batch("CREATE TABLE counter(n INTEGER NOT NULL)")?;
            c.execute("INSERT INTO counter(n) VALUES(0)", [])?;
            Ok(())
        })
        .await
        .unwrap();

        let mut handles = Vec::new();
        for _ in 0..25 {
            let m = Arc::clone(&mgr);
            let t = t.clone();
            handles.push(tokio::spawn(async move {
                // Read-modify-write: only correct if writes are serialized.
                m.with_write(&t, |c| {
                    let n: i64 = c.query_row("SELECT n FROM counter", [], |r| r.get(0))?;
                    c.execute("UPDATE counter SET n = ?1", [n + 1])?;
                    Ok(())
                })
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        let n: i64 = mgr
            .with_conn(&t, |c| Ok(c.query_row("SELECT n FROM counter", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 25, "writes were not serialized");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-sqlite`
Expected: FAIL — no such package `memorysafe-backend-sqlite`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-sqlite/Cargo.toml`:

```toml
[package]
name = "memorysafe-backend-sqlite"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
memorysafe-core.workspace = true
memorysafe-embed.workspace = true
memorysafe-backend.workspace = true
rusqlite = { version = "0.40.2", features = ["bundled", "time"] }
tokio = { version = "1.53.1", features = ["rt", "rt-multi-thread", "macros", "sync"] }
async-trait = "0.1.92"
thiserror.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true
base64.workspace = true
lru = "0.16"

[dev-dependencies]
tempfile = "3.27.0"

[lints]
workspace = true
```

Add to workspace `[workspace.dependencies]`:

```toml
memorysafe-backend-sqlite = { path = "crates/memorysafe-backend-sqlite" }
```

`crates/memorysafe-backend-sqlite/src/schema.rs`:

```rust
use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS items (
  id                TEXT PRIMARY KEY,
  subject           TEXT NOT NULL,
  namespace         TEXT NOT NULL,
  body              TEXT NOT NULL,
  kind              TEXT NOT NULL,
  source_kind       TEXT NOT NULL,
  source_id         TEXT,
  occurred_at       INTEGER,
  created_at        INTEGER NOT NULL,
  tags              TEXT NOT NULL,
  attrs             TEXT NOT NULL,
  sensitivity       INTEGER NOT NULL,
  ttl_seconds       INTEGER,
  protection        TEXT NOT NULL,
  protected_until   INTEGER,
  value_score       REAL NOT NULL DEFAULT 0.0,
  fragility_score   REAL NOT NULL DEFAULT 0.0,
  byte_size         INTEGER NOT NULL,
  last_access       INTEGER,
  access_count      INTEGER NOT NULL DEFAULT 0,
  pending_embedding INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_items_scope      ON items(subject, namespace);
CREATE INDEX IF NOT EXISTS idx_items_scope_sens ON items(subject, namespace, sensitivity);

CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
  body, tags, content='items', content_rowid='rowid', tokenize='unicode61'
);
CREATE TRIGGER IF NOT EXISTS items_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, body, tags) VALUES (new.rowid, new.body, new.tags);
END;
CREATE TRIGGER IF NOT EXISTS items_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, body, tags)
    VALUES('delete', old.rowid, old.body, old.tags);
END;
CREATE TRIGGER IF NOT EXISTS items_au AFTER UPDATE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, body, tags)
    VALUES('delete', old.rowid, old.body, old.tags);
  INSERT INTO items_fts(rowid, body, tags) VALUES (new.rowid, new.body, new.tags);
END;

CREATE TABLE IF NOT EXISTS vectors (
  item_id   TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  subject   TEXT NOT NULL,
  namespace TEXT NOT NULL,
  embedder  TEXT NOT NULL,
  dim       INTEGER NOT NULL,
  scale     REAL NOT NULL,
  q         BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_vectors_scope ON vectors(subject, namespace, embedder, dim);

CREATE TABLE IF NOT EXISTS capacity (
  subject    TEXT NOT NULL,
  namespace  TEXT NOT NULL,
  max_items  INTEGER,
  max_bytes  INTEGER,
  used_items INTEGER NOT NULL DEFAULT 0,
  used_bytes INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (subject, namespace)
);

CREATE TABLE IF NOT EXISTS audit (
  id         TEXT PRIMARY KEY,
  at         INTEGER NOT NULL,
  subject    TEXT NOT NULL,
  namespace  TEXT NOT NULL,
  event      TEXT NOT NULL,
  items      TEXT NOT NULL,
  assessment TEXT,
  decision   TEXT,
  actor      TEXT NOT NULL,
  policy     TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_scope_at ON audit(subject, namespace, at DESC);

CREATE TABLE IF NOT EXISTS idempotency (
  key            TEXT PRIMARY KEY,
  subject        TEXT NOT NULL,
  namespace      TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  outcome        TEXT NOT NULL,
  at             INTEGER NOT NULL
);
"#;

/// Applies pragmas and DDL. Idempotent — safe on every open.
pub fn initialise(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}
```

`crates/memorysafe-backend-sqlite/src/tenant.rs`:

```rust
use crate::schema;
use memorysafe_backend::BackendError;
use memorysafe_core::TenantId;
use rusqlite::Connection;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex as AsyncMutex;

pub fn storage_error(e: impl std::fmt::Display, retryable: bool) -> BackendError {
    BackendError::Storage { message: e.to_string(), retryable }
}

fn to_backend(e: rusqlite::Error) -> BackendError {
    let retryable = matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked)
    );
    storage_error(e, retryable)
}

type Handle = Arc<StdMutex<Connection>>;

/// Owns one SQLite file per tenant. Connections are pooled with an LRU so a
/// large tenant count does not mean an equally large open-file count; write
/// locks are held per tenant so capacity accounting cannot drift.
pub struct TenantManager {
    root: PathBuf,
    pool: StdMutex<lru::LruCache<String, Handle>>,
    write_locks: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl TenantManager {
    pub fn new(root: PathBuf, max_open: usize) -> Self {
        std::fs::create_dir_all(&root).expect("tenant root must be creatable");
        let cap = NonZeroUsize::new(max_open.max(1)).expect("max_open >= 1");
        Self {
            root,
            pool: StdMutex::new(lru::LruCache::new(cap)),
            write_locks: StdMutex::new(HashMap::new()),
        }
    }

    fn handle(&self, tenant: &TenantId) -> Result<Handle, BackendError> {
        let key = tenant.as_str().to_string();
        {
            let mut pool = self.pool.lock().expect("pool mutex");
            if let Some(h) = pool.get(&key) {
                return Ok(Arc::clone(h));
            }
        }
        // `TenantId` validation forbids `/`, `..`, and control characters, so
        // this is a safe path component by construction.
        let path = self.root.join(format!("{key}.db"));
        let conn = Connection::open(&path).map_err(to_backend)?;
        schema::initialise(&conn).map_err(to_backend)?;
        let handle: Handle = Arc::new(StdMutex::new(conn));
        let mut pool = self.pool.lock().expect("pool mutex");
        pool.put(key, Arc::clone(&handle));
        Ok(handle)
    }

    fn write_lock(&self, tenant: &TenantId) -> Arc<AsyncMutex<()>> {
        let mut locks = self.write_locks.lock().expect("write-lock map mutex");
        Arc::clone(
            locks
                .entry(tenant.as_str().to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    /// Runs `f` on the tenant's connection off the async runtime. Concurrent
    /// readers are fine under WAL.
    pub async fn with_conn<T, F>(&self, tenant: &TenantId, f: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, BackendError> + Send + 'static,
    {
        let handle = self.handle(tenant)?;
        tokio::task::spawn_blocking(move || {
            let mut conn = handle.lock().expect("connection mutex");
            f(&mut conn)
        })
        .await
        .map_err(|e| storage_error(e, false))?
    }

    /// Same, but holds the tenant's write lock for the whole closure. Every
    /// mutating path must go through this.
    pub async fn with_write<T, F>(&self, tenant: &TenantId, f: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, BackendError> + Send + 'static,
    {
        let lock = self.write_lock(tenant);
        let _guard = lock.lock().await;
        self.with_conn(tenant, f).await
    }
}
```

`crates/memorysafe-backend-sqlite/src/lib.rs`:

```rust
//! SQLite backend: one database file per tenant.
//!
//! Isolation is structural rather than a query-layer invariant — backup is
//! `cp`, tenant deletion is `rm`, and per-tenant encryption is a key per file.

pub mod schema;
pub mod tenant;

use std::path::PathBuf;
use tenant::TenantManager;

pub struct SqliteBackend {
    pub(crate) tenants: TenantManager,
}

impl SqliteBackend {
    pub fn open(root: PathBuf) -> Self {
        Self::with_max_open(root, 64)
    }

    pub fn with_max_open(root: PathBuf, max_open: usize) -> Self {
        Self { tenants: TenantManager::new(root, max_open) }
    }
}
```

Note: `rusqlite::Error` must be convertible into `BackendError` for the `?` operator inside closures. Add to `tenant.rs`:

```rust
impl From<rusqlite::Error> for BackendErrorShim {
    fn from(e: rusqlite::Error) -> Self {
        BackendErrorShim(to_backend(e))
    }
}
```

Simpler and preferred: give the closures the return type `Result<T, BackendError>` and add this blanket conversion in `memorysafe-backend/src/lib.rs` instead — but `memorysafe-backend` must not depend on `rusqlite`. So define the conversion locally in the SQLite crate:

```rust
// crates/memorysafe-backend-sqlite/src/tenant.rs
pub trait SqlResultExt<T> {
    fn sql(self) -> Result<T, BackendError>;
}

impl<T> SqlResultExt<T> for rusqlite::Result<T> {
    fn sql(self) -> Result<T, BackendError> {
        self.map_err(to_backend)
    }
}
```

Every SQL call in Tasks 20–24 ends in `.sql()?` rather than a bare `?`. Update the test closures above accordingly (`c.execute(...).sql()?`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-sqlite tenant`
Expected: PASS — 5 tests ok.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-backend-sqlite/
git commit -m "feat(sqlite): per-tenant database files, schema, and pooled connections"
```

---

## Task 20: SQLite — items and audit

**Files:**
- Create: `crates/memorysafe-backend-sqlite/src/items.rs`
- Create: `crates/memorysafe-backend-sqlite/src/audit.rs`
- Create: `crates/memorysafe-backend-sqlite/tests/conformance.rs`
- Modify: `crates/memorysafe-backend-sqlite/src/lib.rs`

**Interfaces:**
- Consumes: `TenantManager`, `SqlResultExt`, all core types.
- Produces: `items::insert(&Connection, &MemoryItem)`, `items::get`, `items::list`, `items::delete`, `items::row_to_item`, `audit::insert`, `audit::query`, and a partial `Backend` impl covering `get`, `list`, `audit`, `record_recall`, plus a first `apply` that handles insert + eviction + audit.

**Milestone:** the isolation and atomicity conformance tests execute for the first time.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend-sqlite/tests/conformance.rs`:

```rust
use memorysafe_backend::conformance::{BackendFactory, isolation};
use memorysafe_backend_sqlite::SqliteBackend;
use std::future::Future;

/// Each test gets a backend rooted in its own `TempDir`. The directory is
/// leaked deliberately: it must outlive the backend, and the OS reclaims it.
struct SqliteFactory;

impl BackendFactory for SqliteFactory {
    type B = SqliteBackend;
    fn create(&self) -> impl Future<Output = Self::B> + Send {
        async {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.keep();
            SqliteBackend::open(path)
        }
    }
}

#[tokio::test]
async fn tenants_are_isolated() {
    isolation::tenants_are_isolated(&SqliteFactory).await;
}

#[tokio::test]
async fn subjects_are_isolated() {
    isolation::subjects_are_isolated(&SqliteFactory).await;
}

#[tokio::test]
async fn namespaces_are_separated() {
    isolation::namespaces_are_separated(&SqliteFactory).await;
}

#[tokio::test]
async fn audit_is_scoped() {
    isolation::audit_is_scoped(&SqliteFactory).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-sqlite --test conformance`
Expected: FAIL — `SqliteBackend: Backend is not satisfied`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-sqlite/src/items.rs`:

```rust
use crate::tenant::SqlResultExt;
use memorysafe_backend::{BackendError, Page};
use memorysafe_core::{
    ItemId, MemoryItem, Protection, Scope, SensitivityLevel, Source, SourceKind,
};
use rusqlite::{Connection, Row, params};
use time::{Duration, OffsetDateTime};

fn source_kind_str(k: SourceKind) -> &'static str {
    match k {
        SourceKind::Agent => "agent",
        SourceKind::Session => "session",
        SourceKind::Tool => "tool",
        SourceKind::Human => "human",
    }
}

fn source_kind_from(s: &str) -> SourceKind {
    match s {
        "session" => SourceKind::Session,
        "tool" => SourceKind::Tool,
        "human" => SourceKind::Human,
        _ => SourceKind::Agent,
    }
}

fn protection_parts(p: Protection) -> (&'static str, Option<i64>) {
    match p {
        Protection::Normal => ("normal", None),
        Protection::Pinned => ("pinned", None),
        Protection::Protected { until } => ("protected", Some(until.unix_timestamp())),
    }
}

fn protection_from(kind: &str, until: Option<i64>) -> Protection {
    match kind {
        "pinned" => Protection::Pinned,
        "protected" => match until.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) {
            Some(until) => Protection::Protected { until },
            None => Protection::Normal,
        },
        _ => Protection::Normal,
    }
}

pub const ITEM_COLUMNS: &str = "id, subject, namespace, body, kind, source_kind, source_id, \
     occurred_at, created_at, tags, attrs, sensitivity, ttl_seconds, protection, \
     protected_until, byte_size, pending_embedding";

pub fn row_to_item(row: &Row<'_>, tenant: &str) -> rusqlite::Result<MemoryItem> {
    let tags_json: String = row.get("tags")?;
    let attrs_json: String = row.get("attrs")?;
    let subject: String = row.get("subject")?;
    let namespace: String = row.get("namespace")?;
    let sensitivity: i64 = row.get("sensitivity")?;
    let ttl: Option<i64> = row.get("ttl_seconds")?;
    let protection: String = row.get("protection")?;
    let protected_until: Option<i64> = row.get("protected_until")?;
    let occurred: Option<i64> = row.get("occurred_at")?;
    let created: i64 = row.get("created_at")?;
    let pending: i64 = row.get("pending_embedding")?;

    Ok(MemoryItem {
        id: ItemId::parse(&row.get::<_, String>("id")?).expect("stored ids are valid ULIDs"),
        scope: Scope::new(tenant, &subject, &namespace).expect("stored scopes are valid"),
        body: row.get("body")?,
        kind: row.get("kind")?,
        source: Source {
            kind: source_kind_from(&row.get::<_, String>("source_kind")?),
            id: row.get("source_id")?,
        },
        occurred_at: occurred.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()),
        created_at: OffsetDateTime::from_unix_timestamp(created)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH),
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        attrs: serde_json::from_str(&attrs_json).unwrap_or_default(),
        sensitivity: SensitivityLevel::from_ordinal(sensitivity)
            .unwrap_or(SensitivityLevel::Restricted),
        ttl: ttl.map(Duration::seconds),
        protection: protection_from(&protection, protected_until),
        pending_embedding: pending != 0,
    })
}

pub fn insert(conn: &Connection, item: &MemoryItem) -> Result<(), BackendError> {
    let (protection, protected_until) = protection_parts(item.protection);
    conn.execute(
        "INSERT INTO items (id, subject, namespace, body, kind, source_kind, source_id,
             occurred_at, created_at, tags, attrs, sensitivity, ttl_seconds, protection,
             protected_until, byte_size, pending_embedding)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            item.id.as_str(),
            item.scope.subject.as_str(),
            item.scope.namespace.as_str(),
            item.body,
            item.kind,
            source_kind_str(item.source.kind),
            item.source.id,
            item.occurred_at.map(|t| t.unix_timestamp()),
            item.created_at.unix_timestamp(),
            serde_json::to_string(&item.tags).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(&item.attrs).unwrap_or_else(|_| "{}".into()),
            item.sensitivity.ordinal(),
            item.ttl.map(|d| d.whole_seconds()),
            protection,
            protected_until,
            item.byte_size() as i64,
            i64::from(item.pending_embedding),
        ],
    )
    .sql()?;
    Ok(())
}

pub fn get(conn: &Connection, scope: &Scope, id: &ItemId)
    -> Result<Option<MemoryItem>, BackendError>
{
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM items
         WHERE id = ?1 AND subject = ?2 AND namespace = ?3"
    );
    let mut stmt = conn.prepare(&sql).sql()?;
    let tenant = scope.tenant.as_str().to_string();
    let mut rows = stmt
        .query_map(
            params![id.as_str(), scope.subject.as_str(), scope.namespace.as_str()],
            move |r| row_to_item(r, &tenant),
        )
        .sql()?;
    match rows.next() {
        Some(r) => Ok(Some(r.sql()?)),
        None => Ok(None),
    }
}

pub fn list(conn: &Connection, scope: &Scope, page: &Page)
    -> Result<Vec<MemoryItem>, BackendError>
{
    // Ordered by id: ULIDs are lexicographically time-ordered, which makes
    // pagination stable under concurrent inserts.
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM items
         WHERE subject = ?1 AND namespace = ?2
         ORDER BY id ASC LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = conn.prepare(&sql).sql()?;
    let tenant = scope.tenant.as_str().to_string();
    let rows = stmt
        .query_map(
            params![
                scope.subject.as_str(),
                scope.namespace.as_str(),
                page.effective_limit() as i64,
                page.offset as i64,
            ],
            move |r| row_to_item(r, &tenant),
        )
        .sql()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().sql()
}

/// Returns the byte size of what was removed, for capacity accounting.
pub fn delete(conn: &Connection, scope: &Scope, id: &ItemId) -> Result<u64, BackendError> {
    let size: Option<i64> = conn
        .query_row(
            "SELECT byte_size FROM items WHERE id=?1 AND subject=?2 AND namespace=?3",
            params![id.as_str(), scope.subject.as_str(), scope.namespace.as_str()],
            |r| r.get(0),
        )
        .ok();
    let Some(size) = size else { return Ok(0) };
    conn.execute(
        "DELETE FROM items WHERE id=?1 AND subject=?2 AND namespace=?3",
        params![id.as_str(), scope.subject.as_str(), scope.namespace.as_str()],
    )
    .sql()?;
    Ok(size as u64)
}

pub fn exists(conn: &Connection, scope: &Scope, id: &ItemId) -> Result<bool, BackendError> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE id=?1 AND subject=?2 AND namespace=?3",
            params![id.as_str(), scope.subject.as_str(), scope.namespace.as_str()],
            |r| r.get(0),
        )
        .sql()?;
    Ok(n > 0)
}
```

`crates/memorysafe-backend-sqlite/src/audit.rs`:

```rust
use crate::tenant::SqlResultExt;
use memorysafe_backend::BackendError;
use memorysafe_core::{AuditFilter, AuditId, AuditRecord, Scope};
use rusqlite::{Connection, params};

pub fn insert(conn: &Connection, record: &AuditRecord) -> Result<AuditId, BackendError> {
    let policy = record.decision.as_ref().map(|d| d.policy.to_string());
    conn.execute(
        "INSERT INTO audit (id, at, subject, namespace, event, items, assessment, decision,
             actor, policy)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            record.id.as_str(),
            record.at.unix_timestamp(),
            record.scope.subject.as_str(),
            record.scope.namespace.as_str(),
            serde_json::to_string(&record.event).unwrap_or_default().trim_matches('"'),
            serde_json::to_string(&record.items).unwrap_or_else(|_| "[]".into()),
            record.assessment.as_ref().map(|a| serde_json::to_string(a).unwrap_or_default()),
            record.decision.as_ref().map(|d| serde_json::to_string(d).unwrap_or_default()),
            serde_json::to_string(&record.actor).unwrap_or_default(),
            policy,
        ],
    )
    .sql()?;
    Ok(record.id.clone())
}

pub fn query(conn: &Connection, scope: &Scope, filter: &AuditFilter)
    -> Result<Vec<AuditRecord>, BackendError>
{
    let mut sql = String::from(
        "SELECT id, at, subject, namespace, event, items, assessment, decision, actor
         FROM audit WHERE subject = ?1 AND namespace = ?2",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![
        Box::new(scope.subject.as_str().to_string()),
        Box::new(scope.namespace.as_str().to_string()),
    ];

    if !filter.events.is_empty() {
        let names: Vec<String> = filter
            .events
            .iter()
            .map(|e| serde_json::to_string(e).unwrap_or_default().trim_matches('"').to_string())
            .collect();
        let placeholders =
            (0..names.len()).map(|i| format!("?{}", args.len() + i + 1)).collect::<Vec<_>>();
        sql.push_str(&format!(" AND event IN ({})", placeholders.join(",")));
        for n in names {
            args.push(Box::new(n));
        }
    }
    if let Some(since) = filter.since {
        args.push(Box::new(since.unix_timestamp()));
        sql.push_str(&format!(" AND at >= ?{}", args.len()));
    }
    if let Some(until) = filter.until {
        args.push(Box::new(until.unix_timestamp()));
        sql.push_str(&format!(" AND at <= ?{}", args.len()));
    }
    // Newest first; id breaks ties so ordering is total even at one-second
    // resolution.
    args.push(Box::new(filter.limit as i64));
    sql.push_str(&format!(" ORDER BY at DESC, id DESC LIMIT ?{}", args.len()));

    let tenant = scope.tenant.as_str().to_string();
    let mut stmt = conn.prepare(&sql).sql()?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), move |r| {
            let subject: String = r.get("subject")?;
            let namespace: String = r.get("namespace")?;
            let event_str: String = r.get("event")?;
            let items_json: String = r.get("items")?;
            let assessment: Option<String> = r.get("assessment")?;
            let decision: Option<String> = r.get("decision")?;
            let actor_json: String = r.get("actor")?;
            let at: i64 = r.get("at")?;
            Ok(AuditRecord {
                id: AuditId::parse(&r.get::<_, String>("id")?).expect("stored audit ids"),
                at: time::OffsetDateTime::from_unix_timestamp(at)
                    .unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
                scope: Scope::new(&tenant, &subject, &namespace).expect("stored scope"),
                event: serde_json::from_str(&format!("\"{event_str}\"")).expect("stored event"),
                items: serde_json::from_str(&items_json).unwrap_or_default(),
                assessment: assessment.and_then(|s| serde_json::from_str(&s).ok()),
                decision: decision.and_then(|s| serde_json::from_str(&s).ok()),
                actor: serde_json::from_str(&actor_json).expect("stored actor"),
            })
        })
        .sql()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().sql()
}
```

Add a first `Backend` impl in `crates/memorysafe-backend-sqlite/src/lib.rs`. `apply` at this stage handles insert, evictions, and audit inside one transaction; vectors, merge, capacity, and idempotency arrive in Tasks 21–23. Every other method returns `Ok` defaults so the crate compiles and the isolation tests can run:

```rust
pub mod audit;
pub mod items;

use async_trait::async_trait;
use memorysafe_backend::{
    AppliedWrite, Backend, BackendError, CandidateQuery, ExportStream, ImportReport,
    ImportStream, Page, PurgeReport, ScopeSelector, WriteTransaction,
};
use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, Budget, CapacityState, Embedding, ItemId, MemoryItem,
    Scope, ScopeStats, ScoredCandidate, SubjectId, TenantId,
};

#[async_trait]
impl Backend for SqliteBackend {
    async fn get(&self, scope: &Scope, id: &ItemId)
        -> Result<Option<MemoryItem>, BackendError>
    {
        let (scope, id) = (scope.clone(), id.clone());
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| items::get(c, &scope, &id))
            .await
    }

    async fn list(&self, scope: &Scope, page: &Page)
        -> Result<Vec<MemoryItem>, BackendError>
    {
        let (scope, page) = (scope.clone(), *page);
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| items::list(c, &scope, &page))
            .await
    }

    async fn audit(&self, scope: &Scope, filter: &AuditFilter)
        -> Result<Vec<AuditRecord>, BackendError>
    {
        let (scope, filter) = (scope.clone(), filter.clone());
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| audit::query(c, &scope, &filter))
            .await
    }

    async fn record_recall(&self, record: AuditRecord) -> Result<AuditId, BackendError> {
        let tenant = record.scope.tenant.clone();
        self.tenants.with_write(&tenant, move |c| audit::insert(c, &record)).await
    }

    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError> {
        if !txn.is_valid() {
            return Err(BackendError::InvalidTransaction(
                "a transaction may not both insert and merge".into(),
            ));
        }
        let tenant = txn.scope.tenant.clone();
        self.tenants
            .with_write(&tenant, move |conn| {
                let tx = conn.transaction().map_err(|e| tenant::storage_error(e, false))?;

                let mut evicted = Vec::new();
                for id in &txn.evictions {
                    items::delete(&tx, &txn.scope, id)?;
                    evicted.push(id.clone());
                }

                let mut item_id = None;
                if let Some(w) = &txn.upsert
                    && let Some(item) = &w.item
                {
                    items::insert(&tx, item)?;
                    item_id = Some(item.id.clone());
                }

                let audit_id = audit::insert(&tx, &txn.audit)?;
                tx.commit().map_err(|e| tenant::storage_error(e, false))?;

                Ok(AppliedWrite {
                    item_id,
                    audit_id,
                    evicted,
                    replayed: false,
                    replayed_outcome: None,
                })
            })
            .await
    }

    // Implemented in Tasks 21-24.
    async fn retrieve_candidates(&self, _s: &Scope, _q: &CandidateQuery)
        -> Result<Vec<ScoredCandidate>, BackendError> { Ok(vec![]) }
    async fn neighbours(&self, _s: &Scope, _e: &Embedding, _k: usize)
        -> Result<Vec<ScoredCandidate>, BackendError> { Ok(vec![]) }
    async fn capacity_state(&self, _s: &Scope) -> Result<CapacityState, BackendError> {
        Ok(CapacityState { budget: Budget::UNBOUNDED, used_items: 0, used_bytes: 0 })
    }
    async fn scope_stats(&self, _s: &Scope) -> Result<ScopeStats, BackendError> {
        Ok(ScopeStats::default())
    }
    async fn set_budget(&self, _s: &Scope, _b: Budget) -> Result<(), BackendError> { Ok(()) }
    async fn purge_subject(&self, _t: &TenantId, _s: &SubjectId)
        -> Result<PurgeReport, BackendError> {
        Ok(PurgeReport {
            items_removed: 0, vectors_removed: 0,
            audit_rows_removed: 0, audit_rows_preserved: 0,
        })
    }
    async fn export(&self, _s: &ScopeSelector) -> Result<ExportStream, BackendError> {
        Ok(vec![])
    }
    async fn import(&self, _s: ImportStream) -> Result<ImportReport, BackendError> {
        Ok(ImportReport::default())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-sqlite --test conformance`
Expected: PASS — 4 isolation tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-sqlite/
git commit -m "feat(sqlite): item and audit persistence; isolation conformance passes"
```

---

## Task 21: SQLite — vectors and brute-force search

**Files:**
- Create: `crates/memorysafe-backend-sqlite/src/vectors.rs`
- Modify: `crates/memorysafe-backend-sqlite/src/lib.rs`
- Modify: `crates/memorysafe-backend-sqlite/tests/conformance.rs`

**Interfaces:**
- Consumes: `QuantizedVector`, `items::row_to_item`.
- Produces: `vectors::insert(&Connection, &ItemId, &Scope, &QuantizedVector)`, `vectors::delete`, `vectors::scope_embedder(&Connection, &Scope) -> Option<(String, u16)>`, `vectors::search(&Connection, &Scope, &QuantizedVector, k) -> Vec<(MemoryItem, f32)>`, and a real `Backend::neighbours`.

**Design:** no ANN index. Load the scope's vector rows, score them with `QuantizedVector::dot`, keep the top k with a bounded heap. Exact results, nothing to rebuild after every write. `scope_embedder` reports which model a scope's vectors use so a probe from a different model is rejected rather than silently compared.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-backend-sqlite/tests/conformance.rs`:

```rust
use memorysafe_backend::conformance::retrieval;

#[tokio::test]
async fn vector_search_ranks_by_similarity() {
    retrieval::vector_search_ranks_by_similarity(&SqliteFactory).await;
}

#[tokio::test]
async fn cross_model_vectors_are_rejected() {
    retrieval::cross_model_vectors_are_rejected(&SqliteFactory).await;
}
```

Append to `crates/memorysafe-backend-sqlite/src/vectors.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use memorysafe_embed::{DeterministicEmbedder, Embedder};
    use rusqlite::Connection;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        schema::initialise(&c).unwrap();
        c
    }

    #[test]
    fn top_k_is_bounded_and_sorted_descending() {
        let c = conn();
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        let e = DeterministicEmbedder::new(256);

        for body in ["alpha one", "alpha two", "alpha three", "unrelated finance topic"] {
            let item = {
                let mut i = memorysafe_backend::conformance::fx::item(&scope, body);
                i.body = body.to_string();
                i
            };
            crate::items::insert(&c, &item).unwrap();
            let q = memorysafe_embed::QuantizedVector::from_embedding(&e.embed(body).unwrap());
            insert(&c, &item.id, &scope, &q).unwrap();
        }

        let probe =
            memorysafe_embed::QuantizedVector::from_embedding(&e.embed("alpha one").unwrap());
        let hits = search(&c, &scope, &probe, 2).unwrap();

        assert_eq!(hits.len(), 2, "k was not honoured");
        assert!(hits[0].1 >= hits[1].1, "results not sorted descending");
        assert_eq!(hits[0].0.body, "alpha one");
    }

    #[test]
    fn scope_embedder_reports_the_stored_model() {
        let c = conn();
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        assert_eq!(scope_embedder(&c, &scope).unwrap(), None);

        let e = DeterministicEmbedder::new(256);
        let item = memorysafe_backend::conformance::fx::item(&scope, "hello");
        crate::items::insert(&c, &item).unwrap();
        let q = memorysafe_embed::QuantizedVector::from_embedding(&e.embed("hello").unwrap());
        insert(&c, &item.id, &scope, &q).unwrap();

        assert_eq!(
            scope_embedder(&c, &scope).unwrap(),
            Some(("deterministic-256".to_string(), 256))
        );
    }

    #[test]
    fn deleting_an_item_cascades_to_its_vector() {
        let c = conn();
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        let e = DeterministicEmbedder::new(256);
        let item = memorysafe_backend::conformance::fx::item(&scope, "transient");
        crate::items::insert(&c, &item).unwrap();
        let q = memorysafe_embed::QuantizedVector::from_embedding(&e.embed("transient").unwrap());
        insert(&c, &item.id, &scope, &q).unwrap();

        crate::items::delete(&c, &scope, &item.id).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM vectors", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "vector row survived its item");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-sqlite vectors`
Expected: FAIL — `cannot find function search in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-sqlite/src/vectors.rs`:

```rust
use crate::items::{ITEM_COLUMNS, row_to_item};
use crate::tenant::SqlResultExt;
use memorysafe_backend::BackendError;
use memorysafe_core::{ItemId, MemoryItem, Scope};
use memorysafe_embed::QuantizedVector;
use rusqlite::{Connection, params};

pub fn insert(
    conn: &Connection,
    id: &ItemId,
    scope: &Scope,
    q: &QuantizedVector,
) -> Result<(), BackendError> {
    conn.execute(
        "INSERT INTO vectors (item_id, subject, namespace, embedder, dim, scale, q)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(item_id) DO UPDATE SET
             embedder=excluded.embedder, dim=excluded.dim,
             scale=excluded.scale, q=excluded.q",
        params![
            id.as_str(),
            scope.subject.as_str(),
            scope.namespace.as_str(),
            q.embedder.to_string(),
            q.dim as i64,
            q.scale as f64,
            q.to_bytes(),
        ],
    )
    .sql()?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &ItemId) -> Result<(), BackendError> {
    conn.execute("DELETE FROM vectors WHERE item_id = ?1", params![id.as_str()]).sql()?;
    Ok(())
}

/// Which embedder this scope's vectors were produced by. `None` when the scope
/// holds no vectors yet. Comparing across models yields silently meaningless
/// similarities, so every search gates on this.
pub fn scope_embedder(
    conn: &Connection,
    scope: &Scope,
) -> Result<Option<(String, u16)>, BackendError> {
    let mut stmt = conn
        .prepare(
            "SELECT embedder, dim FROM vectors
             WHERE subject=?1 AND namespace=?2 LIMIT 1",
        )
        .sql()?;
    let mut rows = stmt
        .query_map(params![scope.subject.as_str(), scope.namespace.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u16))
        })
        .sql()?;
    match rows.next() {
        Some(r) => Ok(Some(r.sql()?)),
        None => Ok(None),
    }
}

/// Exact brute-force top-k. No ANN index: per-scope corpora are small, results
/// are exact, and there is nothing to rebuild after every write.
pub fn search(
    conn: &Connection,
    scope: &Scope,
    probe: &QuantizedVector,
    k: usize,
) -> Result<Vec<(MemoryItem, f32)>, BackendError> {
    if k == 0 {
        return Ok(vec![]);
    }
    let sql = format!(
        "SELECT i.rowid, {cols}, v.embedder AS v_embedder, v.dim AS v_dim,
                v.scale AS v_scale, v.q AS v_q
         FROM items i JOIN vectors v ON v.item_id = i.id
         WHERE i.subject = ?1 AND i.namespace = ?2
           AND v.embedder = ?3 AND v.dim = ?4",
        cols = ITEM_COLUMNS
            .split(", ")
            .map(|c| format!("i.{c} AS {c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let tenant = scope.tenant.as_str().to_string();
    let mut stmt = conn.prepare(&sql).sql()?;
    let rows = stmt
        .query_map(
            params![
                scope.subject.as_str(),
                scope.namespace.as_str(),
                probe.embedder.to_string(),
                probe.dim as i64,
            ],
            move |r| {
                let item = row_to_item(r, &tenant)?;
                let scale: f64 = r.get("v_scale")?;
                let bytes: Vec<u8> = r.get("v_q")?;
                Ok((item, scale as f32, bytes))
            },
        )
        .sql()?;

    let mut scored: Vec<(MemoryItem, f32)> = Vec::new();
    for row in rows {
        let (item, scale, bytes) = row.sql()?;
        let q = QuantizedVector::from_bytes(
            probe.embedder.clone(),
            probe.dim,
            scale,
            &bytes,
        )
        .map_err(|e| BackendError::Storage { message: e.to_string(), retryable: false })?;
        let score = probe.dot(&q).map_err(|e| BackendError::EmbedderMismatch {
            got: e.to_string(),
            expected: probe.embedder.to_string(),
        })?;
        scored.push((item, score));
    }

    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(k);
    Ok(scored)
}
```

Replace the placeholder `neighbours` in `lib.rs`:

```rust
    async fn neighbours(&self, scope: &Scope, embedding: &Embedding, k: usize)
        -> Result<Vec<ScoredCandidate>, BackendError>
    {
        let (scope, embedding) = (scope.clone(), embedding.clone());
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| {
                // Refuse a probe from a model the scope was not indexed with.
                if let Some((stored, dim)) = vectors::scope_embedder(c, &scope)?
                    && (stored != embedding.embedder.to_string() || dim != embedding.dim)
                {
                    return Err(BackendError::EmbedderMismatch {
                        got: format!("{}:{}", embedding.embedder, embedding.dim),
                        expected: format!("{stored}:{dim}"),
                    });
                }
                let probe = memorysafe_embed::QuantizedVector::from_embedding(&embedding);
                let hits = vectors::search(c, &scope, &probe, k)?;
                Ok(hits
                    .into_iter()
                    .map(|(item, score)| ScoredCandidate {
                        estimated_tokens: estimate_tokens(&item.body),
                        item,
                        relevance: score,
                        vector_score: Some(score),
                        keyword_score: None,
                        value: memorysafe_core::Score::ZERO,
                        fragility: memorysafe_core::Score::ZERO,
                    })
                    .collect())
            })
            .await
    }
```

Add the shared token estimator to `lib.rs`:

```rust
/// Rough token count for budget packing: ~4 bytes per token, the usual
/// English approximation. Deliberately cheap — the budget is a guide, not a
/// contract with a specific tokenizer.
pub(crate) fn estimate_tokens(body: &str) -> u32 {
    ((body.len() as f32 / 4.0).ceil() as u32).max(1)
}
```

Add `pub mod vectors;` and wire vector insert/delete into `apply`:

```rust
                for id in &txn.evictions {
                    items::delete(&tx, &txn.scope, id)?;
                    vectors::delete(&tx, id)?;
                    evicted.push(id.clone());
                }

                let mut item_id = None;
                if let Some(w) = &txn.upsert
                    && let Some(item) = &w.item
                {
                    items::insert(&tx, item)?;
                    if let Some(v) = &w.vector {
                        vectors::insert(&tx, &item.id, &txn.scope, v)?;
                    }
                    item_id = Some(item.id.clone());
                }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-sqlite`
Expected: PASS — 3 unit tests plus 6 conformance tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-sqlite/
git commit -m "feat(sqlite): exact brute-force vector search with cross-model rejection"
```

---

## Task 22: SQLite — FTS5 keyword search and hybrid retrieval

**Files:**
- Create: `crates/memorysafe-backend-sqlite/src/keyword.rs`
- Create: `crates/memorysafe-backend-sqlite/src/retrieve.rs`
- Modify: `crates/memorysafe-backend-sqlite/src/lib.rs`
- Modify: `crates/memorysafe-backend-sqlite/tests/conformance.rs`

**Interfaces:**
- Consumes: `vectors::search`, `items::row_to_item`.
- Produces: `keyword::escape_fts_query(&str) -> Option<String>`, `keyword::search(&Connection, &Scope, &str, limit)`, `retrieve::candidates(&Connection, &Scope, &CandidateQuery)`, and a real `Backend::retrieve_candidates`.

**Two things this task must get right.** FTS5 treats `"`, `*`, `NEAR`, `AND`, `OR`, and parentheses as operators, so raw user text is a query-injection and a crash risk — every term is quoted and internal quotes are doubled. And hard filters (`sensitivity_ceiling`, tags, kinds, time bounds, pending-embedding) are applied in SQL, never after fetching, because a policy must never be able to widen a candidate set.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-backend-sqlite/tests/conformance.rs`:

```rust
#[tokio::test]
async fn sensitivity_ceiling_is_enforced_in_the_query() {
    retrieval::sensitivity_ceiling_is_enforced_in_the_query(&SqliteFactory).await;
}

#[tokio::test]
async fn tag_and_kind_filters_narrow_results() {
    retrieval::tag_and_kind_filters_narrow_results(&SqliteFactory).await;
}

#[tokio::test]
async fn keyword_search_finds_exact_terms() {
    retrieval::keyword_search_finds_exact_terms(&SqliteFactory).await;
}

#[tokio::test]
async fn keyword_search_escapes_user_input() {
    retrieval::keyword_search_escapes_user_input(&SqliteFactory).await;
}

#[tokio::test]
async fn hybrid_returns_both_signal_sources() {
    retrieval::hybrid_returns_both_signal_sources(&SqliteFactory).await;
}

#[tokio::test]
async fn pagination_is_stable() {
    retrieval::pagination_is_stable(&SqliteFactory).await;
}

#[tokio::test]
async fn pending_embedding_items_are_excluded_when_asked() {
    retrieval::pending_embedding_items_are_excluded_when_asked(&SqliteFactory).await;
}
```

Append to `crates/memorysafe-backend-sqlite/src/keyword.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terms_become_quoted_terms() {
        assert_eq!(escape_fts_query("zstandard").as_deref(), Some("\"zstandard\""));
        assert_eq!(
            escape_fts_query("cat mat").as_deref(),
            Some("\"cat\" OR \"mat\"")
        );
    }

    #[test]
    fn operators_are_neutralised_rather_than_interpreted() {
        // "AND"/"OR"/"NEAR" must be searched for, not executed.
        assert_eq!(escape_fts_query("a AND b").as_deref(), Some("\"a\" OR \"AND\" OR \"b\""));
        assert_eq!(escape_fts_query("*").as_deref(), Some("\"*\""));
        assert_eq!(escape_fts_query("(unbalanced").as_deref(), Some("\"(unbalanced\""));
    }

    #[test]
    fn embedded_quotes_are_doubled_so_the_term_stays_one_token() {
        assert_eq!(escape_fts_query("say \"hi\"").as_deref(), Some("\"say\" OR \"\"\"hi\"\"\""));
    }

    #[test]
    fn empty_or_punctuation_only_input_yields_no_query() {
        assert_eq!(escape_fts_query(""), None);
        assert_eq!(escape_fts_query("   "), None);
        assert_eq!(escape_fts_query("\""), Some("\"\"\"\"".to_string()));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-sqlite keyword`
Expected: FAIL — `cannot find function escape_fts_query in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-sqlite/src/keyword.rs`:

```rust
use crate::items::{ITEM_COLUMNS, row_to_item};
use crate::tenant::SqlResultExt;
use memorysafe_core::{MemoryItem, Scope};
use memorysafe_backend::BackendError;
use rusqlite::{Connection, params};

/// Turns arbitrary user text into a safe FTS5 MATCH expression.
///
/// Every whitespace-separated term is wrapped in double quotes, which makes
/// FTS5 treat it as a literal string rather than an operator; internal quotes
/// are doubled per FTS5's own escaping rule. Terms are OR-ed so a multi-word
/// query behaves like "any of these", which is what hybrid retrieval wants —
/// precision comes from the vector side.
pub fn escape_fts_query(raw: &str) -> Option<String> {
    let terms: Vec<String> = raw
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return None;
    }
    Some(terms.join(" OR "))
}

/// Returns `(item, bm25_score)` where a higher score is a better match.
pub fn search(
    conn: &Connection,
    scope: &Scope,
    raw_query: &str,
    limit: usize,
) -> Result<Vec<(MemoryItem, f32)>, BackendError> {
    let Some(expr) = escape_fts_query(raw_query) else {
        return Ok(vec![]);
    };
    let sql = format!(
        "SELECT {cols}, bm25(items_fts) AS bm25
         FROM items_fts
         JOIN items i ON i.rowid = items_fts.rowid
         WHERE items_fts MATCH ?1 AND i.subject = ?2 AND i.namespace = ?3
         ORDER BY bm25 ASC LIMIT ?4",
        cols = ITEM_COLUMNS
            .split(", ")
            .map(|c| format!("i.{c} AS {c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let tenant = scope.tenant.as_str().to_string();
    let mut stmt = conn.prepare(&sql).sql()?;
    let rows = stmt
        .query_map(
            params![expr, scope.subject.as_str(), scope.namespace.as_str(), limit as i64],
            move |r| {
                let item = row_to_item(r, &tenant)?;
                let bm25: f64 = r.get("bm25")?;
                Ok((item, bm25 as f32))
            },
        )
        .sql()?;

    // bm25() returns negative values, more negative meaning a better match.
    // Flip and squash into (0, 1] so it can be fused with cosine.
    let mut out = Vec::new();
    for row in rows {
        let (item, bm25) = row.sql()?;
        let positive = (-bm25).max(0.0);
        out.push((item, positive / (1.0 + positive)));
    }
    Ok(out)
}
```

`crates/memorysafe-backend-sqlite/src/retrieve.rs`:

```rust
use crate::{estimate_tokens, keyword, vectors};
use memorysafe_backend::{BackendError, CandidateQuery, HardFilters};
use memorysafe_core::{MemoryItem, Score, Scope, ScoredCandidate};
use rusqlite::Connection;
use std::collections::HashMap;

/// Weight on the vector signal when both are present. Keyword carries the rest.
const VECTOR_WEIGHT: f32 = 0.7;

/// Applied in Rust only over rows the SQL already narrowed, as a second line of
/// defence. The SQL predicates are the enforcement point.
fn passes(item: &MemoryItem, f: &HardFilters) -> bool {
    if item.sensitivity > f.sensitivity_ceiling {
        return false;
    }
    if f.exclude_pending_embedding && item.pending_embedding {
        return false;
    }
    if !f.kinds.is_empty() && !f.kinds.contains(&item.kind) {
        return false;
    }
    if !f.tags_any.is_empty() && !item.tags.iter().any(|t| f.tags_any.contains(t)) {
        return false;
    }
    if let Some(after) = f.occurred_after
        && item.occurred_at.is_none_or(|t| t < after)
    {
        return false;
    }
    if let Some(before) = f.occurred_before
        && item.occurred_at.is_none_or(|t| t > before)
    {
        return false;
    }
    true
}

pub fn candidates(
    conn: &Connection,
    scope: &Scope,
    query: &CandidateQuery,
) -> Result<Vec<ScoredCandidate>, BackendError> {
    if !query.is_valid() {
        return Err(BackendError::InvalidQuery(
            "a query must carry an embedding, text, or both".into(),
        ));
    }

    // Over-fetch from each source; fusion and the policy narrow afterwards.
    let fetch = query.limit.saturating_mul(4).max(query.limit);

    let mut merged: HashMap<String, (MemoryItem, Option<f32>, Option<f32>)> = HashMap::new();

    if let Some(embedding) = &query.embedding
        && let Some((stored, dim)) = vectors::scope_embedder(conn, scope)?
        && stored == embedding.embedder.to_string()
        && dim == embedding.dim
    {
        let probe = memorysafe_embed::QuantizedVector::from_embedding(embedding);
        for (item, score) in vectors::search(conn, scope, &probe, fetch)? {
            merged
                .entry(item.id.as_str().to_string())
                .or_insert((item, None, None))
                .1 = Some(score);
        }
    }

    if let Some(text) = &query.text {
        for (item, score) in keyword::search(conn, scope, text, fetch)? {
            let e = merged.entry(item.id.as_str().to_string()).or_insert((item, None, None));
            e.2 = Some(score);
        }
    }

    let mut out: Vec<ScoredCandidate> = merged
        .into_values()
        .filter(|(item, _, _)| passes(item, &query.filters))
        .map(|(item, vector_score, keyword_score)| {
            let relevance = match (vector_score, keyword_score) {
                (Some(v), Some(k)) => VECTOR_WEIGHT * v + (1.0 - VECTOR_WEIGHT) * k,
                (Some(v), None) => v,
                (None, Some(k)) => k,
                (None, None) => 0.0,
            };
            ScoredCandidate {
                estimated_tokens: estimate_tokens(&item.body),
                item,
                relevance,
                vector_score,
                keyword_score,
                value: Score::ZERO,
                fragility: Score::ZERO,
            }
        })
        .collect();

    // Ties broken by id so ordering is total and pagination is reproducible.
    out.sort_by(|a, b| {
        b.relevance.total_cmp(&a.relevance).then_with(|| a.item.id.cmp(&b.item.id))
    });
    out.truncate(query.limit);
    Ok(out)
}
```

Wire the SQL-side filters into `vectors::search` and `keyword::search` by adding a `filters: &HardFilters` parameter to both and appending predicates:

```rust
// Shared helper in retrieve.rs, used to build the WHERE fragment for both.
pub fn filter_sql(f: &HardFilters, args: &mut Vec<Box<dyn rusqlite::ToSql>>) -> String {
    let mut sql = String::new();
    args.push(Box::new(f.sensitivity_ceiling.ordinal()));
    sql.push_str(&format!(" AND i.sensitivity <= ?{}", args.len()));
    if f.exclude_pending_embedding {
        sql.push_str(" AND i.pending_embedding = 0");
    }
    if !f.kinds.is_empty() {
        let ph: Vec<String> =
            (0..f.kinds.len()).map(|i| format!("?{}", args.len() + i + 1)).collect();
        sql.push_str(&format!(" AND i.kind IN ({})", ph.join(",")));
        for k in &f.kinds {
            args.push(Box::new(k.clone()));
        }
    }
    if !f.tags_any.is_empty() {
        let ph: Vec<String> =
            (0..f.tags_any.len()).map(|i| format!("?{}", args.len() + i + 1)).collect();
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM json_each(i.tags) WHERE json_each.value IN ({}))",
            ph.join(",")
        ));
        for t in &f.tags_any {
            args.push(Box::new(t.clone()));
        }
    }
    if let Some(after) = f.occurred_after {
        args.push(Box::new(after.unix_timestamp()));
        sql.push_str(&format!(" AND i.occurred_at >= ?{}", args.len()));
    }
    if let Some(before) = f.occurred_before {
        args.push(Box::new(before.unix_timestamp()));
        sql.push_str(&format!(" AND i.occurred_at <= ?{}", args.len()));
    }
    sql
}
```

Replace the placeholder `retrieve_candidates` in `lib.rs`:

```rust
    async fn retrieve_candidates(&self, scope: &Scope, query: &CandidateQuery)
        -> Result<Vec<ScoredCandidate>, BackendError>
    {
        let (scope, query) = (scope.clone(), query.clone());
        self.tenants
            .with_conn(&scope.tenant.clone(), move |c| retrieve::candidates(c, &scope, &query))
            .await
    }
```

`CandidateQuery` needs `Clone`; confirm the derive from Task 14.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-sqlite`
Expected: PASS — 4 keyword unit tests plus 13 conformance tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-sqlite/
git commit -m "feat(sqlite): FTS5 keyword search with escaping and hybrid fusion"
```

---
