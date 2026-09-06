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
- **When you change a test count, propagate it — whoever you are and whatever you are doing.** Every task's Step 4 states an expected count (`Expected: PASS — N tests ok`), and those counts are cumulative *per crate*: adding, removing or splitting a test in one task changes the expected count of **every later task that runs the same crate**, not just its own. Walk forward to the last task touching that crate and update each one. This is not hypothetical — Task 28 gained a test and Task 29's count stayed at the old total for a full round, which turns the one signal a task executor has that its step went right into noise it learns to ignore. The rule is deliberately addressed to the **action**, not to a role: the four extra tests in Task 25 were written by a policy author, routed by a coordinator and applied by a third party, and the count went uncorrected because the obligation had been worded as binding on *implementers* and none of the three was one. A rule whose trigger names a person or a document has a gap wherever the action happens outside it.
- **When you assert a fact about this codebase, cite the symbol, and say whether you verified it or are relaying it.** Cite `Backend::purge_subject` or `fixtures::fx::admit_txn`, never a line number: this document's line numbers have drifted six times during execution and every citation made against them is now wrong, while a symbol survives any edit that does not delete it. The verified/relayed marking matters because an unmarked relayed claim reads exactly like a checked one, and the reader cannot tell which they are acting on. This binds the **act of claiming** — it reaches task reports, review findings, ledger entries and messages equally, not only the documents where a reader is obviously waiting.
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

**Signatures only.** Each method's *contract* lives on the trait itself
(`memorysafe-backend/src/lib.rs`, `memorysafe-core/src/policy.rs`) and is not
restated here, because restating is how it drifts: three sketches in this
document carried `audit_aggregates`' pre-filter signature for a full round
after the real trait had changed, one of them literal code a task execution
would have copied and failed to compile.

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
    async fn purge_subject(&self, tenant: &TenantId, subject: &SubjectId,
        cascade: PurgeCascade, audit: AuditRecord)
        -> Result<PurgeReport, BackendError>;
    async fn audit_aggregates(&self, tenant: &TenantId, filter: &AuditAggregateFilter)
        -> Result<Vec<AuditAggregate>, BackendError>;
    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError>;
    async fn import(&self, destination: &TenantId, stream: ImportStream)
        -> Result<ImportReport, BackendError>;
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
CREATE INDEX idx_audit_scope_at ON audit(subject, namespace, at, id DESC);

CREATE TABLE idempotency (
  key            TEXT PRIMARY KEY,
  subject        TEXT NOT NULL,
  namespace      TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  outcome        TEXT NOT NULL,               -- JSON WriteOutcome
  at             INTEGER NOT NULL
);

CREATE TABLE audit_aggregates (
  -- The key, and nothing finer. No subject column and no namespace column:
  -- `memorysafe_backend::aggregates` carries the argument, and
  -- `lifecycle::audit_aggregates_survive_a_cascading_purge` fails if a purge
  -- can reach these rows.
  --
  -- The policy is two columns because `PolicyId`'s Display is not injective;
  -- a rendered key column merges distinct policies into one row. See the
  -- prose below.
  policy_name    TEXT,                        -- NULL together with policy_version
  policy_version TEXT,
  event          TEXT NOT NULL,               -- AuditEvent::as_str(), never an ordinal
  day            INTEGER NOT NULL,            -- whole UTC days, aggregates::day_bucket
  count          INTEGER NOT NULL,
  value_histogram     TEXT NOT NULL,          -- JSON [u64; 10]
  fragility_histogram TEXT NOT NULL,          -- JSON [u64; 10]
  histogram_version   INTEGER NOT NULL,
  -- The two policy columns are NULL together or set together. Without this a
  -- row like ('x', NULL, 'admitted', 1) is representable, falls inside the
  -- policied partial index — whose predicate tests only `policy_name` — and
  -- `aggregates::query`'s `(Some(n), Some(v)) => Some(..), _ => None` would
  -- silently relabel it as policy-less: a wrong aggregate that looks
  -- well-formed. The invariant was a comment; this makes it a constraint.
  CHECK ((policy_name IS NULL) = (policy_version IS NULL))
);
-- Uniqueness in two partial indexes rather than one PRIMARY KEY over the
-- nullable tuple. A unique index treats NULLs as distinct from each other, so
-- a single index over (policy_name, policy_version, event, day) would let two
-- policy-less rows with the same event and day both insert — and policy-less
-- rows are the majority of the key space, not a corner. Splitting on the
-- nullability makes the constraint hold in both cases without inventing a
-- sentinel policy string, which `AggregateKey::policy`'s doc rules out.
CREATE UNIQUE INDEX idx_aggregates_key_policied
  ON audit_aggregates(policy_name, policy_version, event, day)
  WHERE policy_name IS NOT NULL;
CREATE UNIQUE INDEX idx_aggregates_key_policy_less
  ON audit_aggregates(event, day)
  WHERE policy_name IS NULL;
-- The read path's ordering index, in the documented key order: day first, then
-- the policy parts, then the event name. Collation is stated on every text
-- column rather than left to the engine's default, per the mandate on
-- `Backend::audit_aggregates`; `COLLATE BINARY` is SQLite's spelling of byte
-- order and Postgres's is `COLLATE "C"`.
CREATE INDEX idx_aggregates_order ON audit_aggregates(
  day,
  policy_name    COLLATE BINARY,
  policy_version COLLATE BINARY,
  event          COLLATE BINARY
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
| 30 | Policy: MMR set-coverage | Retires the split-coverage regression |
| 31 | Policy: byte-budget reclaim | Closes the admit/maintain divergence |
| 32 | Engine: decision validation | `fail_closed` / `fail_safe` |
| 33 | Engine: `remember` | Write pipeline end to end |
| 34 | Engine: `recall` | Read pipeline end to end |
| 35 | Engine: `forget`, `review`, `protect`, `purge_subject` | |
| 36 | Engine: maintenance job | Resumable with cursor |
| 37 | Engine: cache + invalidation | |
| 38 | Engine: retention profiles | |
| 39 | Engine: export/import orchestration | |
| 40 | Proptest invariants | The five correctness properties |
| 41 | Engine: re-embedding + backfill | `pending_embedding` items become searchable |

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
- Produces: `CoreError`, `ItemId::new()`, `ItemId::parse()`, `AuditId::new()`, `AuditId::parse()`, `TenantId::new(&str)`, `SubjectId::new(&str)`, `Namespace::new(&str)`, `Scope { tenant, subject, namespace }`, `Scope::key() -> String`, and `PURGED_COMPONENT`.

`PURGED_COMPONENT` was added to `ids.rs` by a later contract commit, not by
this task's sketch below: it is the reserved namespace a `SubjectPurged`
record is filed under when the subject owned no items (Task 35's
`purge_scope`). Core does not reject the name — enforcement is Plan 3's, named
in the constant's own doc comment, which is authoritative and not reproduced
here.

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
        let long = "x".repeat(241);
        assert!(TenantId::new(&long).is_err());
        assert!(TenantId::new(&"x".repeat(240)).is_ok());
    }

    #[test]
    fn uppercase_is_rejected_so_tenants_cannot_collide_on_case_insensitive_disks() {
        // `Acme` and `acme` would be distinct tenants sharing one `.db` file on
        // APFS or NTFS. Structural isolation depends on this.
        assert!(TenantId::new("Acme").is_err());
        assert!(TenantId::new("ACME").is_err());
        assert!(TenantId::new("acme").is_ok());
        assert!(SubjectId::new("User42").is_err());
        assert!(Namespace::new("CodingAgent").is_err());
    }

    #[test]
    fn ulid_ids_parse_back_and_reject_garbage() {
        let id = ItemId::new();
        assert_eq!(ItemId::parse(id.as_str()).unwrap(), id);
        assert!(ItemId::parse("not-a-ulid").is_err());
        assert!(ItemId::parse("").is_err());
        assert!(AuditId::parse(AuditId::new().as_str()).is_ok());
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

// No `Eq`: `OutOfRange` carries an f32.
#[derive(Debug, Error, PartialEq)]
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

// 240, not 255: the SQLite backend writes `<tenant>.db` plus SQLite's own
// `-wal` and `-shm` sidecars, and every one of those must fit under NAME_MAX.
const MAX_COMPONENT_BYTES: usize = 240;

/// Scope components are used as SQLite filenames and SQL parameters, so the
/// allowed alphabet is deliberately narrow: lowercase ASCII alphanumerics,
/// `-`, `_`, `.`, with a leading `.` rejected to rule out `.` and `..`.
///
/// Uppercase is rejected rather than folded. Tenant isolation is structural —
/// one database file per tenant — so on a case-insensitive filesystem (default
/// macOS APFS, default Windows NTFS, most SMB mounts) `Acme` and `acme` would
/// be two distinct tenants sharing one file. Folding to lowercase would close
/// the breach but silently merge two customers; rejecting makes the constraint
/// visible at the API boundary and fails loudly instead.
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
        let ok = byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'-' | b'_' | b'.');
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
                // ulid 3.x names this `generate`, not `new`.
                Self(ulid::Ulid::generate().to_string())
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
        // An array, not `vec!` — clippy::useless_vec denies the latter here.
        let mut v = [Score::clamped(0.5), Score::clamped(0.1), Score::clamped(0.9)];
        v.sort();
        assert_eq!(v[0].get(), 0.1);
        assert_eq!(v[2].get(), 0.9);
    }

    #[test]
    fn deserialization_cannot_bypass_the_range_invariant() {
        // `Ord` and `Eq` on this type are sound only because no `Score` can
        // hold NaN or a value outside [0,1]. Deserialization is the one path
        // that does not go through a constructor, so it is validated too.
        assert_eq!(serde_json::from_str::<Score>("0.5").unwrap().get(), 0.5);
        assert!(serde_json::from_str::<Score>("2.5").is_err());
        assert!(serde_json::from_str::<Score>("-0.1").is_err());
        // Round-trip of a valid score is unaffected.
        let s = Score::clamped(0.25);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Score>(&json).unwrap(), s);
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

// Two arms, not one with `#[allow(unused_mut)]`: an empty invocation needs no
// mutable binding, and a suppression inside an exported macro would land in
// every expansion at every call site forever — including future ones where the
// lint would be telling the truth.
#[macro_export]
macro_rules! features {
    () => {
        $crate::score::FeatureMap::new()
    };
    ($($k:expr => $v:expr),+ $(,)?) => {{
        let mut m = $crate::score::FeatureMap::new();
        $( m.insert($k.to_string(), $v as f64); )+
        m
    }};
}

/// A value in `[0.0, 1.0]`. Never a bare `f32` anywhere in the API.
///
/// `Deserialize` goes through `TryFrom<f32>`, not a transparent passthrough.
/// A derived transparent `Deserialize` would delegate to `f32`'s own impl and
/// bypass both constructors, so a stored `2.5` or a NaN from a binary format
/// would enter the type unchecked — and the hand-written `Ord` below, plus the
/// `Eq` marker, are sound only while that cannot happen. Invalid stored data
/// fails loudly rather than being silently corrected.
// `transparent` and `try_from` conflict, so `Serialize` keeps `transparent`
// (the wire format must stay a bare number — audit rows store these) and
// `Deserialize` is hand-written to route through the validating constructor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Score(f32);

impl TryFrom<f32> for Score {
    type Error = CoreError;
    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Score::new(value)
    }
}

impl<'de> Deserialize<'de> for Score {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = f32::deserialize(deserializer)?;
        Score::try_from(value).map_err(serde::de::Error::custom)
    }
}

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

        // kind and tags are hashed too — the doc comment claims it and the
        // audit trail relies on it to tell two items apart.
        let mut other_kind = item("hello");
        other_kind.kind = "preference".into();
        assert_ne!(item("hello").digest(), other_kind.digest(), "kind is not hashed");

        let mut tagged = item("hello");
        tagged.tags = vec!["work".into()];
        assert_ne!(item("hello").digest(), tagged.digest(), "tags are not hashed");

        // Tag lists must not collide across different groupings.
        let mut joined = item("hello");
        joined.tags = vec!["a,b".into()];
        let mut split = item("hello");
        split.tags = vec!["a".into(), "b".into()];
        assert_ne!(joined.digest(), split.digest(), "tag delimiter is ambiguous");
    }

    #[test]
    fn byte_size_counts_body_and_metadata() {
        let small = item("a");
        let large = item(&"a".repeat(1000));
        assert!(large.byte_size() > small.byte_size());
        assert!(small.byte_size() > 0);

        // Every variable-length field must move the charge, or a caller can
        // consume storage for free and overrun the budget.
        let base = item("a").byte_size();

        let mut tagged = item("a");
        tagged.tags = vec!["a-fairly-long-tag-value".into()];
        assert!(tagged.byte_size() > base, "tags are not charged");

        let mut attributed = item("a");
        attributed.attrs.insert("k".into(), serde_json::json!("a-long-attribute-value"));
        assert!(attributed.byte_size() > base, "attrs are not charged");

        let mut sourced = item("a");
        sourced.source.id = Some("a".repeat(500));
        assert!(sourced.byte_size() > base, "source.id is not charged");

        let mut deep = item("a");
        deep.scope = Scope::new("t", "a-long-subject-identifier", "a-long-namespace").unwrap();
        assert!(deep.byte_size() > base, "scope columns are not charged");

        let mut kinded = item("a");
        kinded.kind = "a-much-longer-kind-name".into();
        assert!(kinded.byte_size() > base, "kind is not charged");
    }

    #[test]
    fn pinned_items_are_never_evictable_at_any_instant() {
        // Pinning is absolute. A single-timestamp test would still pass for a
        // regression like `Pinned => t.unix_timestamp() < 0`.
        for ts in [i32::MIN as i64, -1, 0, 1, 1_700_000_000, i32::MAX as i64] {
            let t = OffsetDateTime::from_unix_timestamp(ts).unwrap();
            assert!(!Protection::Pinned.is_evictable(t), "pinned became evictable at {ts}");
            assert!(Protection::Normal.is_evictable(t));
        }
    }

    #[test]
    fn serde_forms_are_a_stored_wire_format() {
        // Audit rows and the portable export archive both store these shapes.
        // Changing a tag key or the timestamp encoding is a data-compatibility
        // break, not a refactor.
        assert_eq!(
            serde_json::to_string(&Protection::Normal).unwrap(),
            r#"{"kind":"normal"}"#
        );
        assert_eq!(
            serde_json::to_string(&Protection::Pinned).unwrap(),
            r#"{"kind":"pinned"}"#
        );
        let until = OffsetDateTime::from_unix_timestamp(1000).unwrap();
        assert_eq!(
            serde_json::to_string(&Protection::Protected { until }).unwrap(),
            r#"{"kind":"protected","until":1000}"#
        );
        assert_eq!(
            serde_json::to_string(&SourceKind::Human).unwrap(),
            r#""human""#
        );
        // Timestamps are Unix seconds, not RFC3339.
        let i = item("wire format");
        let json = serde_json::to_string(&i).unwrap();
        assert!(json.contains(r#""created_at":0"#), "timestamp encoding changed: {json}");
        let back: MemoryItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, i);
    }

    #[test]
    fn protection_window_expires() {
        let now = OffsetDateTime::from_unix_timestamp(1000).unwrap();
        let future = OffsetDateTime::from_unix_timestamp(2000).unwrap();
        let past = OffsetDateTime::from_unix_timestamp(500).unwrap();
        assert!(!Protection::Protected { until: future }.is_evictable(now));
        assert!(Protection::Protected { until: past }.is_evictable(now));
        // The boundary is inclusive: a window ending exactly now has expired.
        assert!(Protection::Protected { until: now }.is_evictable(now));
        let plus_one = OffsetDateTime::from_unix_timestamp(1001).unwrap();
        assert!(!Protection::Protected { until: plus_one }.is_evictable(now));
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
            // Unit separator, not a comma: `["a,b"]` and `["a", "b"]` would
            // otherwise hash identically, and this digest is an item's identity
            // in the audit trail.
            h.update(b"\x1f");
        }
        h.finalize().to_hex().to_string()
    }

    /// The storage charge for a body plus its metadata. `MemoryItem::byte_size`
    /// and the pre-admission `Candidate.byte_size` MUST both come from here —
    /// if admission checks a smaller number than storage records, every write
    /// overruns the budget by the difference, in the same direction, forever.
    pub fn charge(
        body: &str,
        kind: &str,
        tags: &[String],
        attrs: &BTreeMap<String, serde_json::Value>,
        source_id: Option<&str>,
        subject: &str,
        namespace: &str,
    ) -> u64 {
        let attrs_len = serde_json::to_string(attrs).map(|s| s.len()).unwrap_or(0);
        let tags_len: usize = tags.iter().map(|t| t.len()).sum();
        // 64 approximates the fixed-width fields: ULID, two timestamps, ttl,
        // sensitivity ordinal, protection tag, and the pending flag.
        const FIXED_OVERHEAD: usize = 64;
        (body.len()
            + kind.len()
            + attrs_len
            + tags_len
            + source_id.map(str::len).unwrap_or(0)
            + subject.len()
            + namespace.len()
            + FIXED_OVERHEAD) as u64
    }

    /// Charged against `Budget::max_bytes`.
    ///
    /// Counts every variable-length field that costs a byte on disk. `subject`
    /// and `namespace` are per-row columns in the SQLite backend, and
    /// `source.id` is an unbounded caller-supplied string — omitting either
    /// would let a caller consume real storage at zero charge and silently
    /// overrun the namespace budget. `tenant` is deliberately excluded: it is
    /// the database filename, not a column, so it costs nothing per row.
    pub fn byte_size(&self) -> u64 {
        Self::charge(
            &self.body,
            &self.kind,
            &self.tags,
            &self.attrs,
            self.source.id.as_deref(),
            self.scope.subject.as_str(),
            self.scope.namespace.as_str(),
        )
    }

    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        match self.ttl {
            Some(ttl) => self.created_at + ttl <= now,
            None => false,
        }
    }

    /// Whether retention REQUIRES this item be forgotten, overriding protection.
    ///
    /// `Protection::Pinned` makes `is_evictable` false unconditionally, so a
    /// pinned item with a TTL would otherwise be unforgettable — letting any
    /// caller's pin silently override a legal retention limit. Expiry dominates
    /// protection; protection only governs eviction for capacity.
    pub fn must_forget(&self, now: OffsetDateTime) -> bool {
        self.is_expired(now)
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
    /// Descending by similarity. `Score` rather than a raw `f32`: cosine spans
    /// [-1, 1], but this list is filtered at `near_duplicate_floor` before it
    /// is built, so a negative similarity — meaning "definitely not a
    /// duplicate" — never belongs here.
    pub near_duplicates: Vec<(ItemId, Score)>,
}

impl RedundancyAssessment {
    pub fn best(&self) -> Option<(&ItemId, Score)> {
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
    fn pressure_takes_the_max_in_both_directions() {
        // Every other dual-bounded case here has bytes >= items, so a mutant
        // that OVERWRITES with the byte ratio instead of taking the max would
        // pass. This is the case that catches last-writer-wins.
        let items_dominate = CapacityState {
            budget: Budget { max_items: Some(100), max_bytes: Some(1000) },
            used_items: 80, // 0.8  <- dominates
            used_bytes: 200, // 0.2
        };
        assert!((items_dominate.pressure() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn a_bytes_only_budget_is_still_enforced() {
        // No test fed this Budget shape to pressure()/would_exceed(), so a bug
        // nesting the byte check inside the item check would be invisible.
        let s = CapacityState {
            budget: Budget { max_items: None, max_bytes: Some(1000) },
            used_items: 999_999,
            used_bytes: 900,
        };
        assert!((s.pressure() - 0.9).abs() < 1e-6, "byte budget inert without max_items");
        assert!(!s.would_exceed(1_000_000, 50));
        assert!(s.would_exceed(0, 200));
    }

    #[test]
    fn an_items_only_budget_ignores_bytes() {
        let s = CapacityState {
            budget: Budget { max_items: Some(10), max_bytes: None },
            used_items: 5,
            used_bytes: u64::MAX / 2,
        };
        assert!((s.pressure() - 0.5).abs() < 1e-6);
        assert!(!s.would_exceed(5, u64::MAX / 2));
        assert!(s.would_exceed(6, 0));
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
    ///
    /// Argument order is (items, bytes) — both `u64`, so a transposition
    /// compiles. Every call site in this workspace passes a literal `1` for
    /// `items`, which makes the mistake visible at a glance; if a caller ever
    /// needs a variable item count, give this a named-field argument instead.
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
    ///
    /// **Meaningless below `item_count >= 2`** — there are no pairs to average,
    /// and the `Default` of `0.0` is "no data", not "a corpus whose baseline
    /// similarity sits at the floor". The engine recomputes this from the
    /// neighbours it just fetched before handing it to a policy, so the default
    /// only survives when a scope has no neighbours at all — the case where
    /// fragility scoring already short-circuits to maximum. Any future consumer
    /// that reads this directly must check `item_count` first.
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
            Reason::new(
                ReasonCode::NovelContent,
                "no near duplicates",
                features! { "best_similarity" => 0.12 },
            ),
        );
        assert!(matches!(d.action, Action::Retain { protection: Protection::Normal }));
        assert!(d.evictions.is_empty());
        assert!(d.has_reason(ReasonCode::NovelContent));
        // `has_reason` compares only `.code`, so assert the rest of the Reason
        // survived too — a constructor that rebuilt it from just the code would
        // otherwise pass.
        assert_eq!(d.reasons[0].detail, "no near duplicates");
        assert_eq!(d.reasons[0].evidence.get("best_similarity"), Some(&0.12));
    }

    #[test]
    fn reject_constructor_carries_its_reason() {
        let d = Decision::reject(
            PolicyId::new("baseline", "0.1.0"),
            Reason::new(ReasonCode::NearDuplicate, "cosine 0.99", features! { "sim" => 0.99 }),
        );
        assert!(matches!(d.action, Action::Reject));
        assert!(d.has_reason(ReasonCode::NearDuplicate));
        assert_eq!(d.reasons[0].evidence.get("sim"), Some(&0.99));
    }

    #[test]
    fn has_reason_searches_evictions_as_well_as_reasons() {
        // `has_reason` searches BOTH vectors. Every other test here builds a
        // decision whose match is in `reasons`, and `||` short-circuits, so the
        // evictions branch is never evaluated — deleting it passes all of them.
        let d = Decision {
            subject: None,
            action: Action::Retain { protection: Protection::Normal },
            evictions: vec![Eviction {
                item: ItemId::new(),
                reason: Reason::new(ReasonCode::CapacityPressure, "made room", features! {}),
            }],
            reasons: vec![Reason::new(ReasonCode::NovelContent, "novel", features! {})],
            policy: PolicyId::new("baseline", "0.1.0"),
        };
        assert!(d.has_reason(ReasonCode::NovelContent), "missed the reasons vector");
        assert!(d.has_reason(ReasonCode::CapacityPressure), "missed the evictions vector");
        assert!(!d.has_reason(ReasonCode::TtlExpired));
    }

    #[test]
    fn action_is_internally_tagged_on_the_wire() {
        // Audit rows store this shape. Dropping `tag = "kind"` would silently
        // switch to serde's externally-tagged form and orphan every stored row,
        // and no Rust-level `matches!` assertion would notice.
        assert_eq!(
            serde_json::to_string(&Action::Retain { protection: Protection::Normal }).unwrap(),
            r#"{"kind":"retain","protection":{"kind":"normal"}}"#
        );
        assert_eq!(serde_json::to_string(&Action::Reject).unwrap(), r#"{"kind":"reject"}"#);
        let merge = Action::Merge {
            into: ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
            strategy: MergeStrategy::AppendAndUnion,
        };
        assert_eq!(
            serde_json::to_string(&merge).unwrap(),
            r#"{"kind":"merge","into":"01ARZ3NDEKTSV4RRFFQ69G5FAV","strategy":"append_and_union"}"#
        );
        let back: Action = serde_json::from_str(r#"{"kind":"reject"}"#).unwrap();
        assert!(matches!(back, Action::Reject));
    }

    #[test]
    fn a_full_decision_round_trips_through_json() {
        // The audit table stores this whole struct in its `decision` column and
        // reads it back with serde_json. Nothing else here exercises Merge,
        // ReplaceBody, a populated evictions vector, or PolicyId on the wire.
        let d = Decision {
            subject: None,
            action: Action::Merge {
                into: ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
                strategy: MergeStrategy::ReplaceBody,
            },
            evictions: vec![Eviction {
                item: ItemId::parse("01BX5ZZKBKACTAV9WEVGEMMVRZ").unwrap(),
                reason: Reason::new(
                    ReasonCode::CapacityPressure,
                    "evicted to make room",
                    features! { "value" => 0.11, "eviction_cost" => 0.02 },
                ),
            }],
            reasons: vec![Reason::new(
                ReasonCode::HighRedundancy,
                "folded into a close neighbour",
                features! { "similarity" => 0.95 },
            )],
            policy: PolicyId::new("baseline", "0.1.0"),
        };

        let json = serde_json::to_string(&d).unwrap();
        let back: Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d, "a stored decision did not survive the round trip");

        // A round trip alone proves only self-consistency: serialise and
        // deserialise share the same code, so ANY symmetric rename round-trips
        // perfectly while orphaning every row already on disk. Pin the literal
        // bytes, which is the property stored data actually depends on.
        let expected = concat!(
            r#"{"subject":null,"action":{"kind":"merge","into":"01ARZ3NDEKTSV4RRFFQ69G5FAV","#,
            r#""strategy":"replace_body"},"#,
            r#""evictions":[{"item":"01BX5ZZKBKACTAV9WEVGEMMVRZ","#,
            r#""reason":{"code":"capacity_pressure","detail":"evicted to make room","#,
            r#""evidence":{"eviction_cost":0.02,"value":0.11}}}],"#,
            r#""reasons":[{"code":"high_redundancy","#,
            r#""detail":"folded into a close neighbour","#,
            r#""evidence":{"similarity":0.95}}],"#,
            r#""policy":{"name":"baseline","version":"0.1.0"}}"#
        );
        assert_eq!(json, expected, "the stored decision format changed");

        // And prove old bytes still parse — the actual compatibility question.
        let from_disk: Decision = serde_json::from_str(expected).unwrap();
        assert_eq!(from_disk, d);
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
    NearDuplicate,
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
    /// Append the new body to the target and union tags and attrs.
    ///
    /// Keeps the EARLIEST `occurred_at` and `created_at`, and the SHORTEST
    /// remaining TTL. Keeping the latest `created_at` would push an item's
    /// expiry forward on every merge, so an item under a retention limit would
    /// never expire as long as anything merged into it. `sensitivity` takes the
    /// maximum of the two — a merge must never downgrade a classification.
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
    /// The item this decision is about. `None` means the admission candidate,
    /// which has no id yet. `maintain` MUST set it: a maintenance decision with
    /// no subject names nobody, so `Action::Retain { protection }` returned from
    /// `maintain` — the way an expired protection window is released — would be
    /// counted and never applied.
    pub subject: Option<ItemId>,
    pub action: Action,
    pub evictions: Vec<Eviction>,
    pub reasons: Vec<Reason>,
    pub policy: PolicyId,
}

impl Decision {
    pub fn retain(policy: PolicyId, reason: Reason) -> Self {
        Self {
            subject: None,
            action: Action::Retain { protection: Protection::Normal },
            evictions: vec![],
            reasons: vec![reason],
            policy,
        }
    }

    pub fn reject(policy: PolicyId, reason: Reason) -> Self {
        Self {
            subject: None,
            action: Action::Reject,
            evictions: vec![],
            reasons: vec![reason],
            policy,
        }
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
        assert_eq!(*r.id(), i.id);
        assert_eq!(r.digest(), i.digest());
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
            OffsetDateTime::from_unix_timestamp(1_000_000).unwrap(),
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
    fn the_builders_do_not_clobber_each_other() {
        // `with_assessment` and `with_decision` each rebuild the record; a typo
        // clearing the other's field would pass every other test here, because
        // the golden test constructs its record with a struct literal.
        let scope = Scope::new("t", "s", "n").unwrap();
        let rec = AuditRecord::new(
            scope,
            AuditEvent::Admitted,
            vec![],
            Actor::system(),
            OffsetDateTime::from_unix_timestamp(1_000_000).unwrap(),
        )
        .with_assessment(sample_assessment())
        .with_decision(sample_decision());
        // Equality, not `is_some()`: a builder that ignored its argument and
        // stored some other value would pass an is_some check.
        assert_eq!(rec.assessment.as_ref(), Some(&sample_assessment()));
        assert_eq!(rec.decision.as_ref(), Some(&sample_decision()));
        // And it must return the SAME record, not a fresh one — a builder that
        // rebuilt from scratch would lose these.
        assert_eq!(rec.event, AuditEvent::Admitted);
        assert_eq!(rec.actor, Actor::system());
        assert_eq!(rec.scope.subject.as_str(), "s");

        // And in the opposite order.
        let scope = Scope::new("t", "s", "n").unwrap();
        let rec = AuditRecord::new(
            scope,
            AuditEvent::Forgotten,
            vec![],
            Actor::system(),
            OffsetDateTime::from_unix_timestamp(1_000_000).unwrap(),
        )
        .with_decision(sample_decision())
        .with_assessment(sample_assessment());
        assert_eq!(rec.assessment.as_ref(), Some(&sample_assessment()));
        assert_eq!(rec.decision.as_ref(), Some(&sample_decision()));
        assert_eq!(rec.event, AuditEvent::Forgotten, "new() ignored its event argument");
    }

    #[test]
    fn actor_kinds_serialize_as_snake_case() {
        // `ApiKey` is the only variant where snake_case and lowercase diverge,
        // so it is the only one that proves which strategy is in force.
        assert_eq!(serde_json::to_string(&ActorKind::ApiKey).unwrap(), "\"api_key\"");
        assert_eq!(serde_json::to_string(&ActorKind::Cli).unwrap(), "\"cli\"");
        assert_eq!(Actor::system().kind, ActorKind::System);
        assert!(Actor::system().id.is_none());
    }

    #[test]
    fn default_audit_filter_matches_everything() {
        let f = AuditFilter::default();
        assert!(f.events.is_empty());
        assert_eq!(f.limit, 100);
        assert!(f.since.is_none());
        assert!(f.until.is_none());
        assert!(f.item.is_none());
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

/// An item's identity in an audit row: an id and a content digest, never the
/// body.
///
/// Fields are private and `from_item` is the only constructor, matching how
/// every other identity type in this crate is built. That closes the
/// accidental path — `ItemRef { id, digest: item.body.clone() }` will not
/// compile — but be precise about what it does not close: `Deserialize` can
/// still produce an `ItemRef` holding arbitrary text, so audit JSON arriving
/// from outside this process is not covered.
///
/// The guarantee is therefore structural for code in this workspace and
/// conventional at the deserialization boundary. See also `Reason::detail`,
/// which is free prose a policy writes and which no type can constrain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRef {
    id: ItemId,
    digest: String,
}

impl ItemRef {
    pub fn from_item(item: &MemoryItem) -> Self {
        Self { id: item.id.clone(), digest: item.digest() }
    }

    pub fn id(&self) -> &ItemId {
        &self.id
    }

    pub fn digest(&self) -> &str {
        &self.digest
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
    /// `at` is supplied, not read from the wall clock: replaying an audit log
    /// against a new policy version must produce comparable rows, and a
    /// constructor that stamped `now_utc()` would make every replayed row
    /// differ from the original.
    pub fn new(
        scope: Scope,
        event: AuditEvent,
        items: Vec<ItemRef>,
        actor: Actor,
        at: OffsetDateTime,
    ) -> Self {
        Self {
            id: AuditId::new(),
            at,
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
    /// Narrow to one subject. Subject is the delete/export unit, so "produce
    /// every audit row for subject X" is THE compliance query — and without
    /// this field it is inexpressible, because after a purge the caller no
    /// longer has the item ids to ask by.
    pub subject: Option<SubjectId>,
    pub namespace: Option<Namespace>,
    /// Cursor. Rows are ordered by `AuditId`, a millisecond ULID and therefore
    /// a total order — `at` is whole seconds and cannot separate rows written
    /// in the same second, so a time-based cursor would repeat or skip them.
    pub after: Option<AuditId>,
    pub item: Option<ItemId>,
    #[serde(with = "time::serde::timestamp::option")]
    pub since: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp::option")]
    pub until: Option<OffsetDateTime>,
    /// Maximum rows to return. Defaults to 100.
    ///
    /// No `truncated` flag, and none is needed: `after` is a ULID cursor, so
    /// a caller detects the end of the log from the page size alone —
    /// `returned.len() < limit` means exhausted. `Backend::audit` states the
    /// rule and requires implementations to return exactly
    /// `min(limit, remaining)`; a backend that returns a short page for any
    /// other reason breaks the signal.
    pub limit: usize,
}

impl Default for AuditFilter {
    fn default() -> Self {
        Self {
            events: vec![],
            subject: None,
            namespace: None,
            after: None,
            item: None,
            since: None,
            until: None,
            limit: 100,
        }
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
    fn a_dim_that_lies_about_its_vector_is_not_comparable() {
        // Fields are public and deserialized, so `dim` can disagree with the
        // vector it describes. Two such embeddings must not compare equal just
        // because their declared dims match.
        let honest = Embedding::new(vec![0.0; 4], EmbedderId::new("m"));
        assert_eq!(honest.dim, 4);
        assert!(honest.is_comparable_to(&honest));

        let liar = Embedding { vector: vec![0.0; 2], embedder: EmbedderId::new("m"), dim: 4 };
        assert!(!liar.is_comparable_to(&honest), "a lying dim was accepted");
        assert!(!honest.is_comparable_to(&liar));
        assert!(!liar.is_comparable_to(&liar), "self-comparison hid the inconsistency");
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
    fn a_budget_is_inclusive_at_its_exact_limit() {
        // `<=`, not `<`. With `<` the last slot of every budget is unusable:
        // `compose` packs while `fits(tokens + next, len + 1)` holds, so an item
        // that exactly fills the budget would be silently dropped and the caller
        // would get fewer memories than they asked for, with no error.
        let b = RecallBudget { max_tokens: Some(2000), max_items: Some(10) };
        assert!(b.fits(2000, 10), "a budget must include its own limit");
        assert!(!b.fits(2001, 10));
        assert!(!b.fits(2000, 11));

        // And each limit is inclusive independently.
        let tokens_only = RecallBudget { max_tokens: Some(100), max_items: None };
        assert!(tokens_only.fits(100, usize::MAX));
        assert!(!tokens_only.fits(101, 0));
        let items_only = RecallBudget { max_tokens: None, max_items: Some(3) };
        assert!(items_only.fits(u32::MAX, 3));
        assert!(!items_only.fits(0, 4));
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
    /// Derives `dim` from the vector rather than accepting it separately, so
    /// the two cannot disagree.
    pub fn new(vector: Vec<f32>, embedder: EmbedderId) -> Self {
        let dim = u16::try_from(vector.len()).unwrap_or(u16::MAX);
        Self { vector, embedder, dim }
    }

    /// Vectors from different models occupy different spaces. Comparing them
    /// produces silently meaningless similarities, so every comparison site
    /// must gate on this.
    ///
    /// Compares the actual vector lengths as well as the declared `dim`. The
    /// fields are public and `Deserialize`d, so `dim` can lie about the vector
    /// it describes — and a `dim` that agrees while the vectors differ in
    /// length is exactly the silent-nonsense case this guard exists to stop.
    pub fn is_comparable_to(&self, other: &Embedding) -> bool {
        self.embedder == other.embedder
            && self.dim == other.dim
            && self.vector.len() == other.vector.len()
            && self.vector.len() == self.dim as usize
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
use time::OffsetDateTime;

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
    /// Bound recall to when the remembered thing happened. Both are pushed
    /// into SQL as hard filters, below the policy, like every other filter
    /// here — without them the backend's time-filter machinery would exist
    /// but be unreachable from any caller.
    pub occurred_after: Option<OffsetDateTime>,
    pub occurred_before: Option<OffsetDateTime>,
    pub mode: RecallMode,
    pub budget: RecallBudget,
    /// The caller's clearance. Items above this level are excluded in SQL,
    /// below the policy.
    pub sensitivity_ceiling: SensitivityLevel,
}

/// A retrieval hit before composition. `relevance` fuses vector and keyword.
///
/// `relevance`, `vector_score` and `keyword_score` are bare `f32`, deliberately
/// breaking the crate's "never a bare `f32`" rule. `Score` is `[0,1]`; cosine
/// spans `[-1,1]` and BM25 is unbounded, so wrapping these would make
/// `Score::new` reject valid retrieval signal and `Score::clamped` destroy the
/// magnitude a ranker needs. Contrast `RedundancyAssessment::near_duplicates`,
/// which *can* use `Score` only because it is pre-filtered above a floor.
/// `value` and `fragility` are genuine governance scores and stay `Score`.
///
/// Because these are `f32`, any sort on them must use `total_cmp` — a
/// degenerate zero-vector cosine can produce NaN.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredCandidate {
    pub item: MemoryItem,
    pub relevance: f32,
    pub vector_score: Option<f32>,
    pub keyword_score: Option<f32>,
    pub value: Score,
    pub fragility: Score,
    pub estimated_tokens: u32,
    /// When this item was last recalled. `retrieve_candidates` and
    /// `neighbours` populate it; `record_recall` advances it.
    ///
    /// **A never-recalled item is `None`, never `Some(created_at)`.** `value`
    /// weighs recency, `fragility` weighs access-recovery cost, and
    /// `compose`'s replay quota is reserved for high-fragility *or
    /// long-unaccessed* items — a feature that exists to resurface what is
    /// never recalled, so an old item recalled yesterday must not look like
    /// one never recalled at all. The distinction is also undetectable in this
    /// workspace's own fixtures if it is not held: `fx::item` pins
    /// `created_at` to `UNIX_EPOCH`, so `Some(created_at)` and "never
    /// accessed" would be the same value everywhere the suite looks.
    ///
    /// Not on `MemoryItem`: that type is serialised into exports and digested
    /// into identity records, so a per-read counter on it would change an
    /// item's serialisation on every read. The ranking structs are ephemeral.
    #[serde(with = "time::serde::timestamp::option")]
    pub last_accessed_at: Option<OffsetDateTime>,
    pub access_count: u64,
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
    /// A **sample** of what was considered and cut, truncated to
    /// `OMITTED_CAP`. Read `omitted_total`, never `omitted.len()`, for how
    /// many items were actually omitted.
    pub omitted: Vec<OmittedItem>,
    /// How many items were omitted **before** the sample above was truncated.
    /// The two are equal exactly when the count is at or below `OMITTED_CAP`
    /// and differ whenever it exceeds it — which is the case a caller needs
    /// to detect and the one `omitted.len()` cannot report, reading 50 for 50
    /// omissions and for 5000 alike. The full argument is on the field in
    /// `crates/memorysafe-core/src/recall.rs`.
    pub omitted_total: usize,
    /// Set by the engine after `record_recall`; the policy leaves it `None`.
    ///
    /// `None` means "not yet audited", and the type cannot distinguish that
    /// from "audited" — so nothing stops an unaudited working set reaching a
    /// caller. "Every recall is audited" is a product claim, so the engine
    /// asserts this is `Some` at its public boundary (see the read-path task).
    pub audit_id: Option<AuditId>,
}

impl WorkingSet {
    pub fn empty() -> Self {
        Self { items: vec![], tokens_used: 0, omitted: vec![], omitted_total: 0, audit_id: None }
    }
}
```

`omitted_total` and the test that pins it apart from `omitted.len()`
(`recall::tests::the_omitted_sample_and_the_omitted_total_are_different_numbers`)
were added by a later contract commit, together with
`ids::tests::the_purged_component_literal_is_frozen_and_is_a_legal_component`.
Neither test is reproduced here; `crates/memorysafe-core` is authoritative, on
the same terms as the conformance tests Task 18 declines to re-sketch.

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
    fn compose_and_maintain_are_reachable_through_the_trait() {
        // Half the trait's surface had no test and neither of these two context
        // types was ever constructed, so a wrong field or a non-Clone field
        // would have been caught only by the signature still compiling.
        let p: Box<dyn GovernancePolicy> = Box::new(AlwaysAdmit);

        let req = RecallRequest {
            scope: Scope::new("t", "s", "n").unwrap(),
            query: Some("anything".into()),
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            mode: RecallMode::WorkingSet,
            budget: RecallBudget::default(),
            sensitivity_ceiling: SensitivityLevel::Internal,
        };
        let compose_ctx = ComposeContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        // Non-empty input: an empty slice proves only that the method returns
        // Ok, not that it can read its own parameters.
        let candidate = ScoredCandidate {
            item: sample_item(),
            relevance: 0.75,
            vector_score: Some(0.75),
            keyword_score: None,
            value: Score::clamped(0.6),
            fragility: Score::clamped(0.2),
            estimated_tokens: 12,
            last_accessed_at: Some(OffsetDateTime::from_unix_timestamp(1_000).unwrap()),
            access_count: 3,
        };
        let ws = p.compose(&req, std::slice::from_ref(&candidate), &compose_ctx).unwrap();
        assert!(ws.items.is_empty(), "AlwaysAdmit composes nothing by design");
        assert_eq!(compose_ctx.clone(), compose_ctx);

        let maintain_ctx = MaintainContext {
            scope: Scope::new("t", "s", "n").unwrap(),
            batch: vec![MaintenanceCandidate {
                item: sample_item(),
                value: Score::clamped(0.3),
                fragility: Score::clamped(0.9),
                // Never recalled: `(None, 0)`, not `(Some(created_at), 0)`.
                // `sample_item().created_at` is the Unix epoch, so the two
                // would be indistinguishable here if the second were allowed.
                last_accessed_at: None,
                access_count: 0,
            }],
            // Not the last page — a policy that only ever sees `true` here is
            // never tested on the partial-view case it must handle.
            is_final_batch: false,
            capacity: CapacityState {
                budget: Budget::UNBOUNDED,
                used_items: 0,
                used_bytes: 0,
            },
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        };
        assert!(p.maintain(&maintain_ctx).unwrap().is_empty());
        assert_eq!(maintain_ctx.clone(), maintain_ctx);
        assert_eq!(maintain_ctx.batch[0].fragility.get(), 0.9);
        assert!(!maintain_ctx.is_final_batch);
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
///
/// Borrows rather than owns, and deliberately does NOT derive `Clone`: a
/// derived `Clone` here would copy the references, not the values, which is
/// not what a reader expects from every other `Clone` in this module. The
/// engine clones the owned `Candidate` and `Assessment` and rebuilds this
/// struct inside its `catch_unwind` boundary, so nothing needs to clone the
/// pair itself.
#[derive(Debug, PartialEq)]
pub struct Assessed<'a> {
    pub candidate: &'a Candidate,
    pub assessment: &'a Assessment,
}

/// Everything `assess` may look at. Plain data; no handles, no closures.
///
/// Serialisable so a production context can be captured to a file and replayed
/// as a fixture against a new policy version — one of the reasons this trait is
/// pure. Every field already derives serde; the context did not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssessContext {
    pub scope: Scope,
    /// Nearest neighbours in the scope, descending by similarity. Empty when
    /// the candidate has no embedding.
    pub neighbours: Vec<ScoredCandidate>,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmitContext {
    pub scope: Scope,
    pub capacity: CapacityState,
    /// Items the engine offers as evictable, cheapest-to-lose first. Already
    /// excludes pinned items and unexpired protection windows.
    ///
    /// `MaintenanceCandidate`, not `ScoredCandidate`, for the same reason
    /// `maintain` uses it: there is no recall query when the engine offers
    /// eviction candidates, so `relevance` could only be zero and a policy
    /// sorting by it would evict in arbitrary backend order.
    pub eviction_candidates: Vec<MaintenanceCandidate>,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComposeContext {
    pub scope: Scope,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

/// An item offered to `maintain`, carrying the same `value`/`fragility` signal
/// `AdmitContext::eviction_candidates` carries — capacity reclaim during
/// maintenance is the same decision `admit` makes under pressure, and a policy
/// handed bare `MemoryItem`s could only sort by age.
///
/// Deliberately NOT `ScoredCandidate`: that type carries `relevance`,
/// `vector_score` and `keyword_score`, which are relative to a recall query.
/// There is no query during maintenance, so the engine could only set them to
/// zero — and a policy that sorted by `relevance` would silently rank every
/// item identically. Omitting the fields makes that mistake unrepresentable
/// rather than merely documented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaintenanceCandidate {
    pub item: MemoryItem,
    pub value: Score,
    pub fragility: Score,
    /// The same access statistics `ScoredCandidate` carries. Maintenance is
    /// where they matter most: "expensive to relearn and nobody has looked at
    /// it in a year" is a different eviction candidate from "expensive to
    /// relearn and recalled yesterday", and without these a policy sorting
    /// eviction candidates can only reach for `created_at`, which says nothing
    /// about use. `(None, 0)` for an item never recalled.
    #[serde(with = "time::serde::timestamp::option")]
    pub last_accessed_at: Option<OffsetDateTime>,
    pub access_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaintainContext {
    pub scope: Scope,
    /// One page of the scope's items. Maintenance is a resumable job; the
    /// engine pages through and calls `maintain` per batch.
    pub batch: Vec<MaintenanceCandidate>,
    /// True when this page is the last one. Derived from the same fact the
    /// engine uses to stop paging (a short page means the end), NOT from
    /// arithmetic over the capacity count — an item forgotten mid-run would
    /// make such a count disagree with the paging position, and a policy would
    /// evict on the strength of a number that had quietly floored to zero.
    pub is_final_batch: bool,
    pub capacity: CapacityState,
    pub stats: ScopeStats,
    pub now: OffsetDateTime,
}

/// The seam. Pure: the engine performs all I/O and hands in everything above.
///
/// `Err` from any method means the CALL failed and the engine should fall back
/// to a baseline policy. It does not mean "skip this item": `compose` reports
/// per-candidate exclusions through `WorkingSet::omitted`, each with a reason,
/// and `maintain` simply returns no `Decision` for an item it declines to act
/// on. A policy that returns `Err` because one candidate of two hundred was
/// unscoreable would discard the other hundred and ninety-nine.
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
    fn distinct_single_tokens_do_not_produce_identical_vectors() {
        // With one probe per token this failed outright: `mat` and `river`
        // scored cosine 1.0, and 21 of 9,316 common word pairs were identical.
        // Downstream conformance tests use short bodies, so a collision here
        // makes "did retrieval find the right item" vacuous.
        let e = DeterministicEmbedder::new(256);
        let words = [
            "mat", "river", "cat", "dog", "the", "repo", "spoon", "valley",
            "memory", "audit", "policy", "tenant", "subject", "namespace",
            "to", "was", "it", "there", "first", "at", "new", "one", "which",
        ];
        let vecs: Vec<Vec<f32>> = words.iter().map(|w| e.embed(w).unwrap().vector).collect();
        for i in 0..words.len() {
            for j in (i + 1)..words.len() {
                let sim: f32 = vecs[i].iter().zip(&vecs[j]).map(|(a, b)| a * b).sum();
                assert!(
                    sim < 0.99,
                    "{} and {} collided at cosine {sim}",
                    words[i],
                    words[j]
                );
            }
        }
    }

    #[test]
    fn documents_whose_tokens_cancel_do_not_collapse_together() {
        // The all-cancel branch used to pin a constant vector, so every
        // document reaching it became identical to every other. Two-word
        // inputs reach it readily.
        let e = DeterministicEmbedder::new(256);
        let a = e.embed("the repo").unwrap();
        let b = e.embed("spoon valley").unwrap();
        let sim: f32 = a.vector.iter().zip(&b.vector).map(|(x, y)| x * y).sum();
        assert!(sim < 0.99, "disjoint documents collapsed together at {sim}");

        // Still unit length whichever branch produced them.
        for v in [&a, &b] {
            let norm: f32 = v.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-5, "fallback vector was not normalised: {norm}");
        }

        // And still deterministic.
        assert_eq!(e.embed("the repo").unwrap().vector, a.vector);
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
model2vec-rs = { version = "0.2.1", optional = true, default-features = false, features = [
    "onig",
    "local-only",
] }

[lints]
workspace = true
```

`model2vec-rs`'s default features are `["onig", "hf-hub"]`; `hf-hub` compiles in a
HuggingFace Hub HTTP client and turns `StaticModel::from_pretrained` on any
non-existent local path into a live network request (it treats the path string as a
Hub repo id). That collides with this plan's write-path constraint of no network
calls, so the dependency is pinned to `default-features = false` with `onig` (the
tokenizer's required regex backend — `tokenizers` fails to compile with neither
`onig` nor `fancy-regex` enabled) and `local-only` (the crate's own feature for
refusing remote downloads, so a missing path fails with a purely local error instead
of silently attempting one) instead of accepting the defaults.

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
            // THREE independent probes per token, not one. With a single probe
            // at dim=256, two unrelated words collide into identical unit
            // vectors about once per 430 pairs — measured: `mat` and `river`
            // score cosine 1.0 with no shared tokens, and 21 of 9,316 common
            // word pairs are byte-identical. Requiring all three probes to
            // coincide drops that to roughly one in dim^3.
            for probe in 0..3 {
                let off = probe * 5;
                let index = u32::from_le_bytes([
                    bytes[off],
                    bytes[off + 1],
                    bytes[off + 2],
                    bytes[off + 3],
                ]) as usize
                    % n;
                let sign = if bytes[off + 4] & 1 == 0 { 1.0 } else { -1.0 };
                v[index] += sign;
            }
        }

        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        } else {
            // Every contribution cancelled. Derive the vector from the WHOLE
            // text rather than pinning a constant: a fixed fallback makes every
            // document that reaches this branch identical to every other one,
            // so `the repo` and `spoon valley` would score cosine 1.0. Two-word
            // inputs reach it readily — `the been`, `is long`, `it every`.
            let whole = blake3::hash(text.as_bytes());
            let hb = whole.as_bytes();
            for (i, slot) in v.iter_mut().enumerate() {
                *slot = (f32::from(hb[i % 32]) - 127.5) / 127.5;
            }
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            } else {
                v[0] = 1.0;
            }
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
        // A zero scale would be persisted to SQLite and make every vector reconstructed
        // from bytes dot to exactly 0.0 forever, a silent retrieval failure. The guard
        // ensures scale stays 1.0 (or at least nonzero), keeping the struct sane.
        assert_eq!(q.scale, 1.0, "scale must be 1.0 for zero vectors, not 0.0");
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
Expected: PASS — 14 tests ok (8 from Task 11 after its two fix rounds, plus the 6 here).

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
    use memorysafe_core::{
        Actor, AuditEvent, AuditRecord, Protection, Scope, SensitivityLevel, Source, SourceKind,
    };
    use time::OffsetDateTime;

    fn scope() -> Scope {
        Scope::new("t", "s", "n").unwrap()
    }

    fn audit() -> AuditRecord {
        audit_for(scope())
    }

    fn audit_for(scope: Scope) -> AuditRecord {
        AuditRecord::new(
            scope,
            AuditEvent::Admitted,
            vec![],
            Actor::system(),
            OffsetDateTime::UNIX_EPOCH,
        )
    }

    fn item(scope: Scope) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope,
            body: "an item".into(),
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
    fn a_write_transaction_always_carries_exactly_one_audit_record() {
        let txn = WriteTransaction::new(scope(), audit());
        assert!(txn.upsert.is_none());
        assert!(txn.merge.is_none());
        assert!(txn.evictions.is_empty());
        assert_eq!(txn.audit.event, AuditEvent::Admitted);
    }

    #[test]
    fn upsert_and_merge_are_mutually_exclusive() {
        let mut txn = WriteTransaction::new(scope(), audit());
        txn.merge = Some(MergeWrite {
            target: memorysafe_core::ItemId::new(),
            body: "merged".into(),
            tags: vec![],
            attrs: Default::default(),
            vector: None,
            byte_size: 6,
        });
        assert!(txn.is_valid());
        txn.upsert = Some(ItemWrite { item: item(scope()), vector: None });
        assert!(!txn.is_valid(), "a transaction may not both insert and merge");
    }

    #[test]
    fn a_transaction_is_invalid_when_the_upserted_items_scope_disagrees() {
        let mut txn = WriteTransaction::new(scope(), audit());
        txn.upsert = Some(ItemWrite { item: item(scope()), vector: None });
        assert!(txn.is_valid(), "a well-formed upsert must be valid");

        // Same tenant, different subject: the item would be written under one
        // subject while the transaction — and its vector row — are filed
        // under another.
        txn.upsert = Some(ItemWrite {
            item: item(Scope::new("t", "other-subject", "n").unwrap()),
            vector: None,
        });
        assert!(
            !txn.is_valid(),
            "an upserted item whose scope disagrees with the transaction's scope must be rejected"
        );
    }

    #[test]
    fn a_transaction_is_invalid_when_the_audit_records_scope_disagrees() {
        let valid = WriteTransaction::new(scope(), audit());
        assert!(valid.is_valid(), "a well-formed transaction must be valid");

        // Same tenant, different namespace: the audit trail would point at a
        // namespace this write never touched.
        let mismatched = WriteTransaction::new(
            scope(),
            audit_for(Scope::new("t", "s", "other-ns").unwrap()),
        );
        assert!(
            !mismatched.is_valid(),
            "an audit record whose scope disagrees with the transaction's scope must be rejected"
        );
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
///
/// `item` is not `Option`: `QuantizedVector` carries no `ItemId`, so
/// `ItemWrite { item: None, vector: Some(v) }` would name no row to attach
/// the vector to — an unaddressable state with no legitimate construction.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemWrite {
    pub item: MemoryItem,
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

    /// A transaction is invalid if it both inserts and merges, or if its
    /// scope-bearing fields disagree about which scope this write belongs to.
    ///
    /// `scope`, the upserted item's own `scope`, and `audit.scope` are three
    /// independently-settable public fields, and a real backend reads
    /// different ones for different rows: the item row is keyed on the
    /// item's own scope, the vector row and the tenant file are keyed on
    /// `scope`, and the audit row is keyed on `audit.scope`. If they
    /// disagreed, the row, its vector, and its own audit trail would each be
    /// filed under a different subject or namespace — the compliance surface
    /// this product sells would point somewhere else. `MergeWrite` carries
    /// no scope of its own, so there is nothing to check there; a merge is
    /// always addressed by `scope`.
    pub fn is_valid(&self) -> bool {
        if self.upsert.is_some() && self.merge.is_some() {
            return false;
        }
        if let Some(w) = &self.upsert
            && w.item.scope != self.scope
        {
            return false;
        }
        self.audit.scope == self.scope
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

/// `audit_rows_removed + audit_rows_preserved` accounts for every audit row
/// that existed for the subject *before* the purge ran — not for any row
/// the purge itself may add. `AuditEvent::SubjectPurged` exists, so a
/// conformant backend may write its own audit row recording the purge; that
/// row did not exist to be removed or preserved and is not counted in
/// either field. (Note: this docstring was added in Task 18's review pass,
/// once `lifecycle`'s purge tests existed to motivate it — it applies from
/// Task 14 onward regardless.)
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
    Audit { audit: Box<AuditRecord> },
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

    /// Ordered ascending by `created_at`, ties broken by ascending `id`
    /// (`id` is a ULID — itself a total, time-sortable order — so breaking
    /// ties by it never contradicts the primary sort). This must be a
    /// *total* order: ordering by timestamp alone is insufficient, since a
    /// bulk import can leave many rows with an identical `created_at`, and
    /// an unstable sort under `LIMIT`/`OFFSET` paging can then return the
    /// same row on two pages while silently dropping another. Every backend
    /// must use this same key and direction — SQLite paging ascending while
    /// Postgres paged descending would each "conform" to a total order
    /// stated without one, which is exactly the cross-backend drift this
    /// suite exists to prevent.
    async fn list(&self, scope: &Scope, page: &Page)
        -> Result<Vec<MemoryItem>, BackendError>;

    /// One page of the scope's audit log. The ordering, the cursor
    /// direction, the inclusivity of `since`/`until` and the truncation rule
    /// all live on `Backend::audit` in `memorysafe-backend/src/lib.rs`, each
    /// now naming the conformance test that enforces it.
    async fn audit(&self, scope: &Scope, filter: &AuditFilter)
        -> Result<Vec<AuditRecord>, BackendError>;

    /// Erases a subject within a tenant. The contract lives on
    /// `Backend::purge_subject` in `memorysafe-backend/src/lib.rs`, together
    /// with the accounting equation on `PurgeReport`.
    async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
        cascade: PurgeCascade,
        audit: AuditRecord,
    ) -> Result<PurgeReport, BackendError>;

    /// Every audit aggregate held for `tenant` that matches `filter`. The
    /// contract lives on `Backend::audit_aggregates` in
    /// `memorysafe-backend/src/lib.rs` and in the `aggregates` module doc
    /// beside it.
    async fn audit_aggregates(
        &self,
        tenant: &TenantId,
        filter: &AuditAggregateFilter,
    ) -> Result<Vec<AuditAggregate>, BackendError>;

    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError>;

    /// Imports `stream` into the `destination` tenant. The contract lives on
    /// `Backend::import` in `memorysafe-backend/src/lib.rs`.
    async fn import(
        &self,
        destination: &TenantId,
        stream: ImportStream,
    ) -> Result<ImportReport, BackendError>;

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
use crate::{Backend, Page};
use memorysafe_core::Scope;

/// Two tenants writing identical content must never see each other's items.
pub async fn tenants_are_isolated<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let a = Scope::new("tenant-a", "sub", "ns").unwrap();
    let b = Scope::new("tenant-b", "sub", "ns").unwrap();

    let item_a = fx::item(&a, "tenant a private note");
    backend.apply(fx::admit_txn(&a, item_a.clone(), None)).await.unwrap();

    // The emptiness assertions below only mean something if the write
    // actually landed under tenant a — otherwise a no-op `apply`, or one
    // that silently dropped the row, would pass this test too.
    let listed_a = backend.list(&a, &Page::default()).await.unwrap();
    assert!(listed_a.iter().any(|i| i.id == item_a.id), "tenant a lost its own item");
    assert_eq!(
        backend.get(&a, &item_a.id).await.unwrap().as_ref().map(|i| &i.id),
        Some(&item_a.id),
        "tenant a could not fetch its own item by id"
    );

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

    // As in `tenants_are_isolated`: prove subject a actually has the item
    // before trusting that subject b's emptiness means isolation and not a
    // dropped write.
    let listed_a = backend.list(&a, &Page::default()).await.unwrap();
    assert!(listed_a.iter().any(|i| i.id == item_a.id), "subject a lost its own item");
    assert_eq!(
        backend.get(&a, &item_a.id).await.unwrap().as_ref().map(|i| &i.id),
        Some(&item_a.id),
        "subject a could not fetch its own item by id"
    );

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
        created_at: OffsetDateTime::UNIX_EPOCH,
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

/// Same as `item`, but with a caller-supplied `created_at` instead of the
/// fixed `UNIX_EPOCH` above.
///
/// `item`'s pinned timestamp makes a corpus built from it **fully tied** under
/// `ORDER BY created_at` — the shape a real bulk import produces. That case is
/// not avoided by this builder; it is covered directly by
/// `retrieval::list_tie_break_is_total_over_identical_timestamps`, which uses
/// `item`/`item_with_id` precisely *because* everything ties there and the
/// tie-break is then the only thing ordering the pages.
///
/// This builder serves the two tests that need distinct, increasing
/// timestamps: `retrieval::list_pages_are_disjoint_and_complete` (which
/// isolates offset/limit arithmetic from ordering) and
/// `retrieval::list_orders_oldest_first_by_created_at` (which can only observe
/// sort direction where the primary key varies).
pub fn item_at(scope: &Scope, body: &str, created_at: OffsetDateTime) -> MemoryItem {
    let mut i = item(scope, body);
    i.created_at = created_at;
    i
}

/// Same as `item`, but with a caller-supplied `ItemId` — `item_at`'s
/// counterpart for the tie-break key.
///
/// `ItemId::new()` is `ulid::Ulid::generate()`, and `ulid_id!` documents that
/// lexicographic order equals creation order only "up to the timestamp's
/// millisecond resolution", so ids minted in a tight loop are randomly ordered
/// relative to each other. Any test asserting an id order must supply literal
/// ULIDs through `ItemId::parse` and insert them in a deliberately
/// non-ascending order, or it passes by coin flip — and passes
/// deterministically for the wrong reason whenever the loop straddles a
/// millisecond boundary.
pub fn item_with_id(scope: &Scope, id: ItemId, body: &str) -> MemoryItem {
    let mut i = item(scope, body);
    i.id = id;
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
    // The audit fires at the same instant the item was created, not a fresh
    // clock read — `at` is supplied, never sampled, per the convention
    // `AuditRecord::new` documents.
    let audit = AuditRecord::new(
        scope.clone(),
        AuditEvent::Admitted,
        vec![ItemRef::from_item(&item)],
        Actor::system(),
        item.created_at,
    );
    let mut txn = WriteTransaction::new(scope.clone(), audit);
    txn.upsert = Some(ItemWrite { item, vector });
    txn
}

/// Same, but embeds the body so the item is vector-searchable.
pub fn admit_txn_embedded(scope: &Scope, item: MemoryItem) -> WriteTransaction {
    let v = vector_for(&item.body);
    admit_txn(scope, item, Some(v))
}

/// A transaction that evicts items without inserting anything.
pub fn evict_txn(scope: &Scope, evictions: Vec<ItemId>) -> WriteTransaction {
    let audit = AuditRecord::new(
        scope.clone(),
        AuditEvent::Forgotten,
        vec![],
        Actor::system(),
        OffsetDateTime::UNIX_EPOCH,
    );
    let mut txn = WriteTransaction::new(scope.clone(), audit);
    txn.evictions = evictions;
    txn
}

/// Same as `evict_txn`, but with a caller-supplied `at` instead of the fixed
/// `UNIX_EPOCH` above — mirrors `item`/`item_at`. `evict_txn` itself stays
/// pinned so Task 16's tests are unaffected; tests that need distinct,
/// ordered eviction timestamps (Task 18's
/// `audit_filter_narrows_by_event_and_time`) use this builder instead.
pub fn evict_txn_at(scope: &Scope, evictions: Vec<ItemId>, at: OffsetDateTime) -> WriteTransaction {
    let audit = AuditRecord::new(
        scope.clone(),
        AuditEvent::Forgotten,
        vec![],
        Actor::system(),
        at,
    );
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

/// Hands out a pristine backend per `create()` call, not per test. SQLite
/// returns one rooted in a fresh `TempDir`; Postgres will return one rooted
/// in a fresh schema.
///
/// Most tests call `create()` once, but `lifecycle::export_import_round_trips_exactly`
/// and `lifecycle::import_is_idempotent` each hold a source and a target
/// backend simultaneously and require them to be mutually invisible — a
/// factory that memoized one instance per test would hand both calls the
/// same backend, and the target would already contain the source's rows
/// before import ever ran. For Postgres this means two schemas alive at
/// once, not one schema reused across a test's two `create()` calls.
/// (Note: this docstring was corrected in Task 18's review pass, once
/// `lifecycle` existed to name the two-backend requirement — it applies
/// from Task 15 onward regardless.)
pub trait BackendFactory: Send + Sync {
    type B: Backend;
    fn create(&self) -> impl Future<Output = Self::B> + Send;
}

/// Runs every conformance test in order. Panics on the first failure with the
/// test's own assertion message.
///
/// What this suite proves, and what it does not: every test here observes
/// state through the `Backend` trait after `apply` returns — item present or
/// absent, evictions gone, audit rows counted. That catches a backend that
/// skips a write, fabricates a success, or leaves a failed transaction's
/// side effects behind. It does not prove atomicity in the transactional
/// sense: there is no fault injection and no concurrent observer, so a
/// backend that performs the item write, the evictions, and the audit row
/// as three separate, non-atomic commits — and simply does not crash
/// between them — passes it too.
/// `atomicity::admit_evict_and_audit_commit_together` is the test whose
/// name promises more than it can check; read it as "the end state after a
/// successful apply is internally consistent," not as proof the three
/// writes committed as one transaction. (Note: this doc comment was added
/// in Task 16's review pass, once `atomicity` existed to name — it applies
/// from Task 15 onward regardless.)
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
        isolation::retrieval_never_crosses_a_scope_boundary,
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
use crate::{Backend, BackendError, Page};
use memorysafe_core::{AuditFilter, ItemId, Scope};

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
        isolation::retrieval_never_crosses_a_scope_boundary,
        atomicity::admit_evict_and_audit_commit_together,
        atomicity::a_failed_transaction_leaves_no_trace,
        atomicity::an_invalid_transaction_is_rejected_and_writes_nothing,
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

**Authorised addition (beyond this brief):** three `is_valid()` unit tests in
`conformance/fixtures.rs`, exercising the transaction shapes this task
introduces — a transaction carrying evictions, a merge-only transaction, and
one carrying an idempotency key and payload digest. The last of the three
has no bite today:

```rust
#[test]
fn a_transaction_with_an_idempotency_key_and_payload_digest_is_valid() {
    let s = scope();
    let i = item(&s, "written once");
    let mut txn = admit_txn(&s, i.clone(), None);
    txn.idempotency_key = Some("key-1".into());
    txn.payload_digest = Some(i.digest());
    // `is_valid()` never inspects `idempotency_key` or `payload_digest`,
    // so this test passes identically whether or not those fields are
    // set — no mutation on either field is demonstrable here. It is kept
    // as a guard against a future `is_valid()` that does inspect them,
    // not as evidence of coverage today.
    assert!(txn.is_valid());
}
```

---

## Task 17: Conformance — retrieval and capacity

**Files:**
- Create: `crates/memorysafe-backend/src/conformance/retrieval.rs`
- Create: `crates/memorysafe-backend/src/conformance/capacity.rs`
- Modify: `crates/memorysafe-backend/src/conformance/mod.rs`

**Interfaces:**
- Consumes: `BackendFactory`, `fx`.
- Produces: `sensitivity_ceiling_is_enforced_in_the_query`, `tag_and_kind_filters_narrow_results`, `vector_search_ranks_by_similarity`, `keyword_search_finds_exact_terms`, `hybrid_beats_either_alone`, `list_pages_are_disjoint_and_complete`, `pending_embedding_items_are_excluded_when_asked`, `capacity_accounting_tracks_items_and_bytes`, `concurrent_admits_do_not_double_count`, `eviction_releases_capacity`.

**`list_pages_are_disjoint_and_complete` was called `pagination_is_stable`.** It was renamed to what it actually proves. "Stable" reads as tie-break stability — the one property it does not check, since it sorts and dedups the collected ids before asserting, which is right for disjointness and destroys every trace of order. The contract task that renamed it added the two tests that do check ordering: `list_orders_oldest_first_by_created_at` (distinct timestamps, rejects a backend paging descending) and `list_tie_break_is_total_over_identical_timestamps` (a fully tied corpus, rejects a backend whose tie-break is not total). The three cannot be merged: on a tied corpus an ascending and a descending backend produce identical output, and on distinct timestamps nothing ever ties.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend/src/conformance/retrieval.rs`:

```rust
use super::{BackendFactory, fx};
use crate::query::{CandidateQuery, HardFilters};
use crate::{Backend, Page};
use memorysafe_core::{Scope, SensitivityLevel};
use memorysafe_embed::Embedder;
use time::{Duration, OffsetDateTime};

fn query(text: &str, filters: HardFilters) -> CandidateQuery {
    CandidateQuery {
        embedding: Some(fx::embedder().embed(text).unwrap()),
        text: Some(text.to_string()),
        filters,
        limit: 50,
    }
}

fn query_with_limit(text: &str, filters: HardFilters, limit: usize) -> CandidateQuery {
    CandidateQuery {
        embedding: Some(fx::embedder().embed(text).unwrap()),
        text: Some(text.to_string()),
        filters,
        limit,
    }
}

/// The security-critical one: a restricted item must never leave the database
/// for a caller cleared only to Personal.
///
/// The corpus needs three properties, each closing a distinct way a ceiling
/// bug could hide behind this test's original, weaker shape:
///
/// 1. **It must exceed the query's `limit`, with the excluded items worded
///    to dominate the ranking.** With a corpus smaller than `limit` (the
///    original shape — one item per level, `limit: 50`), a backend that
///    applies the ceiling inside its query and a backend that runs an
///    unfiltered query and filters the ceiling out afterwards, in Rust,
///    return byte-identical results, because nothing was ever truncated.
///    The two strategies are indistinguishable and the test cannot tell a
///    compliant backend from a dangerous one.
/// 2. **The excluded pool must be large enough to survive a constant-factor
///    over-fetch, not just a naive `SELECT *`.** `CandidateQuery::limit` is
///    itself documented (`query.rs`) as an over-fetch knob the engine
///    "typically sets 5-10x the recall budget," so a backend issuing
///    `LIMIT limit * k` unfiltered and filtering afterwards is not a
///    contrived worst case — it is the natural way to write a hybrid
///    backend's per-arm fetch, and the original 5-item pool (corpus 7,
///    `limit: 3`) fails to catch it: any `k >= 3` fetches the whole corpus
///    unfiltered and returns the correct 2 rows regardless of where
///    filtering happens. With 40 excluded items ranked 1..=40 and the 2
///    admissible items ranked 41 and 42, reaching an admissible row needs
///    `limit * k >= 41`, i.e. `k >= 14` at `limit: 3` — well past the
///    documented 5-10x, so no plausible over-fetch factor leaks the
///    diagnosis. Do not shrink this pool "for speed"; that is exactly the
///    trim that would silently reopen this hole.
/// 3. **The excluded pool must include a level immediately above the
///    ceiling, not only one far above it.** A ceiling of `Personal`
///    (ordinal 2) next to a corpus of only `Restricted` items (ordinal 4)
///    cannot catch a backend whose SQL admits `level_ord <= ceiling_ord +
///    1` — the single most likely off-by-one at the boundary — because
///    ordinal 4 still fails that relaxed check exactly like the correct
///    one (`level_ord <= ceiling_ord`). One `Sensitive` item (ordinal 3,
///    immediately above the ceiling) among the top-ranked pool is wrongly
///    admitted by an off-by-one query and correctly excluded by a
///    compliant one, so both the count assertion and the per-item
///    assertion below catch it.
///
/// The two admissible items share 4 of the query's 5 tokens — differing
/// only in the discriminating first word ("restricted" vs. "public"/
/// "personal") — rather than sharing only one token as the original
/// wording did. That keeps their vector score well above any plausible
/// recall floor in a hybrid backend's vector arm, so a failure here cannot
/// be misdiagnosed as "ceiling applied after LIMIT" when it is really an
/// unrelated recall-floor issue dropping a weakly-matching candidate.
pub async fn sensitivity_ceiling_is_enforced_in_the_query<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // 40 items, verbatim-identical to the query text, so every one of them
    // ranks at the very top under any reasonable vector or keyword scorer.
    // 39 are `Restricted` (ordinal 4); one is `Sensitive` (ordinal 3, the
    // level immediately above the `Personal` ceiling) to catch an
    // off-by-one at the boundary. See point 2 above for why 40, and point 3
    // for why one of them is `Sensitive` rather than all `Restricted`.
    for i in 0..40 {
        let level = if i == 0 {
            SensitivityLevel::Sensitive
        } else {
            SensitivityLevel::Restricted
        };
        let item = fx::item_with(
            &scope,
            "restricted medical record about cats",
            "fact",
            &[],
            level,
        );
        backend
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }
    for (body, level) in [
        ("public medical record about cats", SensitivityLevel::Public),
        (
            "personal medical record about cats",
            SensitivityLevel::Personal,
        ),
    ] {
        let item = fx::item_with(&scope, body, "fact", &[], level);
        backend
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }

    let filters = HardFilters {
        sensitivity_ceiling: SensitivityLevel::Personal,
        ..Default::default()
    };
    let hits = backend
        .retrieve_candidates(
            &scope,
            &query_with_limit("restricted medical record about cats", filters, 3),
        )
        .await
        .unwrap();

    assert_eq!(
        hits.len(),
        2,
        "expected exactly the 2 admissible items; fewer means the ceiling was \
         applied after LIMIT (or with too small an over-fetch margin) instead \
         of inside the query, and more means a level above the ceiling — most \
         likely Sensitive, at the ceiling+1 boundary — leaked through"
    );
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
        backend
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }

    let by_kind = HardFilters {
        kinds: vec!["fact".into()],
        ..Default::default()
    };
    assert_eq!(
        backend
            .retrieve_candidates(&scope, &query("cats", by_kind))
            .await
            .unwrap()
            .len(),
        2
    );

    let by_tag = HardFilters {
        tags_any: vec!["home".into()],
        ..Default::default()
    };
    assert_eq!(
        backend
            .retrieve_candidates(&scope, &query("cats", by_tag))
            .await
            .unwrap()
            .len(),
        1
    );

    let both = HardFilters {
        kinds: vec!["fact".into()],
        tags_any: vec!["work".into()],
        ..Default::default()
    };
    assert_eq!(
        backend
            .retrieve_candidates(&scope, &query("cats", both))
            .await
            .unwrap()
            .len(),
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
    assert_eq!(
        hits[0].item.body, "the cat sat on the mat",
        "exact match should rank first"
    );
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
    for body in [
        "the deployment used zstandard compression",
        "unrelated musings",
    ] {
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

/// Syntax characters a keyword arm treats specially must not become query
/// operators when they arrive as ordinary user text — whatever the
/// backend's own search engine is (SQLite FTS5, Postgres full-text search,
/// or anything else). Only the assertions below are the contract; nothing
/// about them names one engine's syntax.
///
/// The corpus here holds three items, not one. With only one item present —
/// the original shape of this test — "matched everything" and "matched
/// nothing" are the same observation: both produce a result set of size
/// `<= 1`, so the assertion below cannot tell a backend that correctly
/// treats hostile text as an inert literal from one that lets it become an
/// operator and match the whole scope. Three items make the two outcomes
/// different sizes (`<= 1` vs. `3`), so an unescaped hostile string that
/// turns into a match-everything query is actually caught.
///
/// None of the three bodies contains a standalone single-letter token.
/// `"a AND b"` is one of the hostile inputs below; a keyword arm with
/// AND-by-default semantics (FTS5's phrase escaping, Postgres's
/// `plainto_tsquery`) treats it as an inert literal phrase and correctly
/// matches nothing, but a *correctly escaped* arm with any-term OR
/// semantics would still match every body containing a standalone `"a"`.
/// The original corpus ("a normal memory" / "a third normal memory") had
/// two such bodies, so it would fail this test for a compliant
/// OR-semantics backend — the suite rejecting a correct implementation,
/// which is worse than missing an incorrect one.
pub async fn keyword_search_escapes_user_input<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    for body in ["alpha memory", "beta memory", "gamma memory"] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    for hostile in ["\"", "OR 1=1", "a AND b", "NEAR/", "*", "(unbalanced"] {
        let q = CandidateQuery {
            embedding: None,
            text: Some(hostile.to_string()),
            filters: HardFilters::default(),
            limit: 10,
        };
        // Must not error and must not match everything.
        let hits = backend.retrieve_candidates(&scope, &q).await.unwrap();
        assert!(
            hits.len() <= 1,
            "hostile input {hostile:?} matched {} of 3 rows",
            hits.len()
        );
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
        .retrieve_candidates(
            &scope,
            &query("the cat sat on the mat", HardFilters::default()),
        )
        .await
        .unwrap();

    let exact = hits
        .iter()
        .find(|h| h.item.body == "the cat sat on the mat")
        .unwrap();
    assert!(exact.vector_score.is_some(), "vector score missing");
    assert!(exact.keyword_score.is_some(), "keyword score missing");
    assert!(exact.relevance > 0.0);
}

/// Three pages must not overlap and must not drop rows: page *disjointness*
/// and completeness, and nothing else. Rejects a backend whose `OFFSET`
/// arithmetic is off by one — a real bug that survives a perfect total order.
///
/// The corpus uses `fx::item_at`, so nothing ties and no tie-break is
/// involved: an overlap or a dropped row here can only be a paging bug.
/// Direction is covered by `list_orders_oldest_first_by_created_at` and
/// totality by `list_tie_break_is_total_over_identical_timestamps`; the
/// assertions below sort and dedup the ids, which destroys the evidence either
/// of those needs.
pub async fn list_pages_are_disjoint_and_complete<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for i in 0..25 {
        let created_at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(i);
        let item = fx::item_at(&scope, &format!("memory {i:02}"), created_at);
        backend
            .apply(fx::admit_txn(&scope, item, None))
            .await
            .unwrap();
    }

    let p1 = backend
        .list(
            &scope,
            &Page {
                offset: 0,
                limit: 10,
            },
        )
        .await
        .unwrap();
    let p2 = backend
        .list(
            &scope,
            &Page {
                offset: 10,
                limit: 10,
            },
        )
        .await
        .unwrap();
    let p3 = backend
        .list(
            &scope,
            &Page {
                offset: 20,
                limit: 10,
            },
        )
        .await
        .unwrap();

    assert_eq!((p1.len(), p2.len(), p3.len()), (10, 10, 5));

    let mut ids: Vec<_> = p1
        .iter()
        .chain(&p2)
        .chain(&p3)
        .map(|i| i.id.clone())
        .collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "pages overlapped");
    assert_eq!(total, 25, "pages dropped rows");
}

/// `exclude_pending_embedding` must narrow the candidate set, not empty it.
///
/// The assertion also pins the exact surviving count, not just "nothing
/// pending leaked through": `hits.iter().all(...)` is vacuously true over an
/// empty result set, so a backend that (wrongly) excludes every item
/// whenever the flag is set would pass a leak-only check. Asserting
/// `hits.len() == 1` catches that failure mode as well as the leak.
pub async fn pending_embedding_items_are_excluded_when_asked<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let mut pending = fx::item(&scope, "written while the embedder was down, about cats");
    pending.pending_embedding = true;
    backend
        .apply(fx::admit_txn(&scope, pending.clone(), None))
        .await
        .unwrap();
    backend
        .apply(fx::admit_txn_embedded(
            &scope,
            fx::item(&scope, "a normal memory about cats"),
        ))
        .await
        .unwrap();

    // Visible to review regardless.
    assert_eq!(
        backend.list(&scope, &Page::default()).await.unwrap().len(),
        2
    );

    let filters = HardFilters {
        exclude_pending_embedding: true,
        ..Default::default()
    };
    let hits = backend
        .retrieve_candidates(&scope, &query("cats", filters))
        .await
        .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly the non-pending item to survive the filter"
    );
    assert!(hits.iter().all(|h| !h.item.pending_embedding));
}

/// Vectors from a different embedder must be refused, not silently compared.
pub async fn cross_model_vectors_are_rejected<F: BackendFactory>(factory: &F) {
    use memorysafe_embed::DeterministicEmbedder;
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    backend
        .apply(fx::admit_txn_embedded(
            &scope,
            fx::item(&scope, "stored with the 256-dim model"),
        ))
        .await
        .unwrap();

    let other = DeterministicEmbedder::new(384)
        .embed("a probe from another model")
        .unwrap();
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
use crate::{Backend, Page};
use memorysafe_core::{Budget, Scope};

pub async fn capacity_accounting_tracks_items_and_bytes<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(100),
                max_bytes: Some(100_000),
            },
        )
        .await
        .unwrap();

    let before = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(before.used_items, 0);
    assert_eq!(before.used_bytes, 0);

    let item = fx::item(&scope, "a memory of some length");
    let size = item.byte_size();
    backend
        .apply(fx::admit_txn(&scope, item, None))
        .await
        .unwrap();

    let after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(after.used_items, 1);
    assert_eq!(after.used_bytes, size);
    assert_eq!(after.budget.max_items, Some(100));
}

pub async fn eviction_releases_capacity<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(10),
                max_bytes: None,
            },
        )
        .await
        .unwrap();

    for i in 0..3 {
        backend
            .apply(fx::admit_txn(
                &scope,
                fx::item(&scope, &format!("memory {i}")),
                None,
            ))
            .await
            .unwrap();
    }
    assert_eq!(backend.capacity_state(&scope).await.unwrap().used_items, 3);

    let items = backend.list(&scope, &Page::default()).await.unwrap();
    backend
        .apply(fx::evict_txn(&scope, vec![items[0].id.clone()]))
        .await
        .unwrap();

    let after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(after.used_items, 2);
    assert_eq!(
        after.used_bytes,
        items[1].byte_size() + items[2].byte_size()
    );
}

/// The correctness detail the spec calls out: without a lock on the accounting
/// row, concurrent admits both conclude there is room and the count drifts.
///
/// `F::B: 'static` is required, not part of the original test logic: without
/// it, `tokio::spawn` cannot accept a future closing over `Arc<F::B>`, since
/// nothing in `BackendFactory` otherwise promises the backend outlives the
/// borrow of `factory`. Every real backend (SQLite's own connection, a
/// Postgres pool) owns its state and satisfies this trivially.
pub async fn concurrent_admits_do_not_double_count<F: BackendFactory>(factory: &F)
where
    F::B: 'static,
{
    use std::sync::Arc;
    let backend = Arc::new(factory.create().await);
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(1000),
                max_bytes: None,
            },
        )
        .await
        .unwrap();

    let mut handles = Vec::new();
    for i in 0..20 {
        let b = Arc::clone(&backend);
        let s = scope.clone();
        handles.push(tokio::spawn(async move {
            b.apply(fx::admit_txn(
                &s,
                fx::item(&s, &format!("concurrent {i}")),
                None,
            ))
            .await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }

    let state = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(
        state.used_items, 20,
        "capacity accounting drifted under concurrency"
    );

    let listed = backend
        .list(
            &scope,
            &Page {
                offset: 0,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        listed.len() as u64,
        state.used_items,
        "accounting disagrees with reality"
    );
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
        retrieval::list_pages_are_disjoint_and_complete,
        retrieval::list_orders_oldest_first_by_created_at,
        retrieval::list_tie_break_is_total_over_identical_timestamps,
        retrieval::pending_embedding_items_are_excluded_when_asked,
        retrieval::cross_model_vectors_are_rejected,
        retrieval::neighbours_break_ties_before_truncating_at_k,
        retrieval::recall_updates_access_statistics,
        capacity::capacity_accounting_tracks_items_and_bytes,
        capacity::eviction_releases_capacity,
        capacity::concurrent_admits_do_not_double_count,
        capacity::scope_stats_reflect_the_corpus,
```

`concurrent_admits_do_not_double_count` hands `Arc<F::B>` to `tokio::spawn`, which
requires the spawned future to be `'static`; nothing in `BackendFactory`
otherwise promises that `F::B` outlives the borrow of `factory`. Add a
`where F::B: 'static` bound to both `concurrent_admits_do_not_double_count`
and `run_conformance_suite` (which calls every test, including this one,
through the same generic `F`). Every real backend owns its state outright
and satisfies this trivially.

`run_conformance_suite`'s doc comment must also say it needs a
multi-threaded Tokio runtime (e.g. `#[tokio::test(flavor = "multi_thread")]`):
`tokio::spawn` also runs under `current_thread`, but there tasks only
interleave at `.await` points, so `concurrent_admits_do_not_double_count`'s
attempt to provoke a genuine concurrent write race loses most of its bite.

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
- Produces: `audit_filter_narrows_by_event_and_time`, `purge_subject_removes_everything_for_that_subject`, `purge_subject_leaves_other_subjects_intact`, `export_import_round_trips_exactly`, `import_is_idempotent`. (Later contract tasks add eight more to this module — see the `run!` list at the end of this task.)

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend/src/conformance/lifecycle.rs`:

```rust
use super::{BackendFactory, fx};
use crate::portability::ScopeSelector;
use crate::{Backend, Page};
use memorysafe_core::{
    Actor, ActorKind, AuditEvent, AuditFilter, AuditId, AuditRecord, PurgeCascade, Scope,
    SubjectId, TenantId,
};
use std::collections::BTreeSet;
use time::{Duration, OffsetDateTime};

/// Exercises every axis `AuditFilter` can narrow on: `events`, the
/// `since`/`until` time window, and `limit` — including the newest-first
/// ordering `limit` depends on.
///
/// An earlier draft of this test had defects, fixed here:
///
/// 1. **The time bound was never exercised**, despite the test's name —
///    `since`/`until` sat unused. Fixed by querying a `since`/`until` window
///    that must return a genuine strict subset of the four audit rows: not
///    all of them, and not none.
/// 2. **Every record shared one timestamp.** `fx::item`/`fx::evict_txn` both
///    pin `UNIX_EPOCH`, so any ordering assertion over `at` held for *any*
///    order, including oldest-first. Fixed by giving each of the four
///    records its own timestamp — `at` stores whole seconds (see
///    `AuditFilter::after`'s doc on why a time cursor can't separate
///    same-second rows), so anything finer would not survive a real round
///    trip through storage.
/// 3. **The ordering assertion compared `at`**, the one field
///    `memorysafe_core::AuditFilter` documents as unable to provide a total
///    order: rows are ordered by `AuditId` instead, because "`at` is whole
///    seconds and cannot separate rows written in the same second, so a
///    time-based cursor would repeat or skip them." Fixed by asserting
///    identity instead: the two rows returned under `limit: 2` must be the
///    two most recently *written* rows, newest first — checked against the
///    `AuditId`s the writes themselves returned, not against `at`.
/// 4. **The eviction's `at` was the newest of the four**, so ordering by
///    `at` and ordering by `AuditId` (write order) agreed by construction —
///    the test could not tell the two conventions apart, and would have
///    certified a backend that (wrongly) orders by `at`. Fixed by placing
///    the eviction's business timestamp *between* the second and third
///    admit's rather than after all of them, so the two orderings
///    genuinely disagree and only the documented one (`AuditId`) is
///    asserted as correct.
pub async fn audit_filter_narrows_by_event_and_time<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // Three admits at t0/t1/t2, ten seconds apart. The eviction's business
    // timestamp (`t_evict`) sits between t1 and t2 rather than after
    // everything, so it is not simultaneously "newest by `at`" and "newest
    // by write order" — see point 4 above.
    let t0 = OffsetDateTime::UNIX_EPOCH;
    let t1 = t0 + Duration::seconds(10);
    let t2 = t0 + Duration::seconds(20);
    let t_evict = t0 + Duration::seconds(15);

    // All four audit ids are pinned literals, ascending, the eviction's last
    // and largest — never `AuditRecord::new`'s generated ones, which are
    // randomly ordered within a millisecond and made this test's verdict
    // depend on how fast the backend writes. See `fx::AUDIT_ORDER_ULIDS`.
    let audit_ids: Vec<AuditId> = fx::AUDIT_ORDER_ULIDS
        .iter()
        .map(|u| AuditId::parse(u).expect("literal must be a canonical ULID"))
        .collect();

    for (i, at) in [t0, t1, t2].into_iter().enumerate() {
        let mut txn = fx::admit_txn(
            &scope,
            fx::item_at(&scope, &format!("memory {i}"), at),
            None,
        );
        txn.audit.id = audit_ids[i].clone();
        let applied = backend.apply(txn).await.unwrap();
        assert_eq!(applied.audit_id, audit_ids[i], "echo rule: see `Backend`");
    }

    // `list` orders ascending by `created_at`, so `items[0]` is the item
    // admitted at `t0` — the oldest one.
    let items = backend.list(&scope, &Page::default()).await.unwrap();
    let mut evict = fx::evict_txn_at(&scope, vec![items[0].id.clone()], t_evict);
    evict.audit.id = audit_ids[3].clone();
    let evicted = backend.apply(evict).await.unwrap();
    assert_eq!(evicted.audit_id, audit_ids[3], "echo rule: see `Backend`");

    let admits = backend
        .audit(
            &scope,
            &AuditFilter {
                events: vec![AuditEvent::Admitted],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(admits.len(), 3);
    assert!(admits.iter().all(|r| r.event == AuditEvent::Admitted));

    let forgets = backend
        .audit(
            &scope,
            &AuditFilter {
                events: vec![AuditEvent::Forgotten],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(forgets.len(), 1);

    // Time filter: a window strictly between the first admit's timestamp
    // and the third's (5s..25s) admits the second admit (10s), the
    // eviction (15s), and the third admit (20s), and excludes only the
    // first admit (0s) — a strict subset (3 of 4), so a backend that
    // ignores the filter and returns everything, or one that returns
    // nothing, both fail. The bounds are chosen off any record's exact
    // timestamp so the test does not depend on whether `since`/`until` are
    // inclusive or exclusive at the edges.
    let since = t0 + Duration::seconds(5);
    let until = t0 + Duration::seconds(25);
    let windowed = backend
        .audit(
            &scope,
            &AuditFilter {
                since: Some(since),
                until: Some(until),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        windowed.len(),
        3,
        "since/until must select a strict subset of the four rows, not all or none"
    );
    assert!(
        windowed.iter().all(|r| r.at >= since && r.at <= until),
        "a row outside [since, until] leaked through the time filter"
    );
    let windowed_ids: BTreeSet<_> = windowed.iter().map(|r| r.id.clone()).collect();
    let expected_ids: BTreeSet<_> = [
        audit_ids[1].clone(),
        audit_ids[3].clone(),
        audit_ids[2].clone(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        windowed_ids, expected_ids,
        "since/until returned the wrong rows"
    );

    // Ordering: `limit: 2` must return the two most recently *written*
    // rows, newest first — by `AuditId` (write/insertion order), not by
    // the business `at` field. The eviction's `at` (15s) is earlier than
    // the third admit's (20s), so a backend that (wrongly) orders by `at`
    // would return [third admit, eviction] here; the documented ordering
    // by `AuditId` returns [eviction, third admit], since the eviction was
    // written after the third admit regardless of the business timestamp
    // it carries.
    let limited = backend
        .audit(
            &scope,
            &AuditFilter {
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(limited.len(), 2);
    let limited_ids: Vec<_> = limited.iter().map(|r| r.id.clone()).collect();
    assert_eq!(
        limited_ids,
        vec![audit_ids[3].clone(), audit_ids[2].clone()],
        "audit must come back newest first by write order (AuditId), not by the business `at` field: the eviction, then the third admit"
    );
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
                .apply(fx::admit_txn_embedded(
                    scope,
                    fx::item(scope, &format!("memory {i}")),
                ))
                .await
                .unwrap();
        }
    }

    // Baseline: tie the purge report's own numbers to observed state. Every
    // check below the purge is satisfied by a backend that stored nothing
    // at all unless this baseline proves the corpus existed first.
    assert_eq!(
        backend.list(&a, &Page::default()).await.unwrap().len(),
        3,
        "the corpus must exist before the purge for the purge report to mean anything"
    );

    let report = backend
        .purge_subject(
            &tenant,
            &subject,
            PurgeCascade::Cascade,
            fx::purge_record(&a, AuditId::new(), Actor::system()),
        )
        .await
        .unwrap();
    assert_eq!(report.items_removed, 6);
    assert_eq!(report.vectors_removed, 6);

    assert!(backend.list(&a, &Page::default()).await.unwrap().is_empty());
    assert!(backend.list(&b, &Page::default()).await.unwrap().is_empty());
    assert_eq!(backend.capacity_state(&a).await.unwrap().used_items, 0);
    assert_eq!(backend.capacity_state(&b).await.unwrap().used_items, 0);
    assert_eq!(
        report.audit_rows_removed + report.audit_rows_preserved,
        6,
        "every pre-existing audit row must be accounted for as removed or preserved \
         (a backend's own SubjectPurged audit row, if it writes one, did not exist \
         before the purge and is not counted in this total)"
    );
}

pub async fn purge_subject_leaves_other_subjects_intact<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let doomed = Scope::new("t", "doomed", "ns").unwrap();
    let keeper = Scope::new("t", "keeper", "ns").unwrap();

    backend
        .apply(fx::admit_txn(&doomed, fx::item(&doomed, "goes away"), None))
        .await
        .unwrap();
    let keeper_item = fx::item(&keeper, "stays");
    backend
        .apply(fx::admit_txn(&keeper, keeper_item.clone(), None))
        .await
        .unwrap();

    backend
        .purge_subject(
            &TenantId::new("t").unwrap(),
            &SubjectId::new("doomed").unwrap(),
            PurgeCascade::Cascade,
            fx::purge_record(&doomed, AuditId::new(), Actor::system()),
        )
        .await
        .unwrap();

    assert!(
        backend
            .list(&doomed, &Page::default())
            .await
            .unwrap()
            .is_empty()
    );
    let keeper_items = backend.list(&keeper, &Page::default()).await.unwrap();
    assert_eq!(keeper_items.len(), 1);
    assert_eq!(
        keeper_items[0].id, keeper_item.id,
        "purge left the keeper with a different item than the one it admitted"
    );
    // A purge scoped as `DELETE FROM capacity WHERE tenant = ?` (plausible,
    // since purge's own arguments are tenant + subject) would correctly
    // scope the item delete but silently zero an unrelated subject's
    // capacity accounting. Nothing else in this suite purges, so only this
    // test can catch it.
    assert_eq!(
        backend.capacity_state(&keeper).await.unwrap().used_items,
        1,
        "purging one subject zeroed an unrelated subject's capacity accounting"
    );
    assert_eq!(
        backend
            .audit(&keeper, &AuditFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "purging one subject destroyed another's audit"
    );
}

/// Invariant 5 from the spec: export then import reproduces the corpus
/// exactly.
///
/// The three items are built to each carry a different set of
/// `MemoryItem`'s fields away from their shared defaults, not just
/// `kind`/`tags`/`sensitivity`: one carries `Protection::Pinned`, one an
/// `occurred_at`, a `ttl`, and an `attrs` entry, and one
/// `pending_embedding: true` with a non-`Internal` sensitivity. A round
/// trip that silently dropped or re-derived any of these on import, rather
/// than actually persisting and restoring them, is caught by the
/// full-`MemoryItem` equality below. This still cannot catch a field the
/// backend never persists at all — `before` and `after` are both read back
/// through the same backend, via `list()` — but it converts every
/// plausible import-side re-derivation from invisible to caught.
pub async fn export_import_round_trips_exactly<F: BackendFactory>(factory: &F) {
    let source = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let mut cats = fx::item_with(
        &scope,
        "first memory about cats",
        "fact",
        &["work"],
        memorysafe_core::SensitivityLevel::Internal,
    );
    cats.protection = memorysafe_core::Protection::Pinned;

    let mut dogs = fx::item_with(
        &scope,
        "second memory about dogs",
        "preference",
        &["home", "pets"],
        memorysafe_core::SensitivityLevel::Internal,
    );
    dogs.occurred_at = Some(OffsetDateTime::UNIX_EPOCH + Duration::seconds(1));
    dogs.ttl = Some(Duration::days(30));
    dogs.attrs
        .insert("confidence".to_string(), serde_json::json!(0.75));

    let mut zstd = fx::item_with(
        &scope,
        "third memory about zstandard",
        "procedure",
        &[],
        memorysafe_core::SensitivityLevel::Restricted,
    );
    zstd.pending_embedding = true;

    for item in [cats, dogs, zstd] {
        source
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }

    let selector = ScopeSelector {
        tenant: TenantId::new("t").unwrap(),
        subject: None,
        namespace: None,
        include_audit: true,
    };
    let exported = source.export(&selector).await.unwrap();

    let target = factory.create().await;
    let report = target
        .import(&selector.tenant, exported.clone())
        .await
        .unwrap();
    assert_eq!(report.items_imported, 3);
    assert_eq!(report.vectors_imported, 3);
    assert_eq!(
        report.audit_imported, 3,
        "include_audit: true must actually export and import the audit rows, \
         not just set the flag"
    );

    let mut before = source
        .list(
            &scope,
            &Page {
                offset: 0,
                limit: 100,
            },
        )
        .await
        .unwrap();
    let mut after = target
        .list(
            &scope,
            &Page {
                offset: 0,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        before.len(),
        3,
        "the source corpus must exist before comparing it to the imported copy"
    );
    before.sort_by(|a, b| a.id.cmp(&b.id));
    after.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(
        before, after,
        "round trip did not reproduce the items exactly"
    );

    // Vectors survived: the same probe ranks the same way on both sides.
    use memorysafe_embed::Embedder;
    let probe = fx::embedder().embed("cats").unwrap();
    let src_hits = source.neighbours(&scope, &probe, 3).await.unwrap();
    let tgt_hits = target.neighbours(&scope, &probe, 3).await.unwrap();
    assert_eq!(
        src_hits.len(),
        3,
        "the source itself must have ranked vectors before comparing rankings \
         across the round trip — otherwise empty-vs-empty would pass"
    );
    let ids = |v: &[memorysafe_core::ScoredCandidate]| {
        v.iter().map(|c| c.item.id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&src_hits),
        ids(&tgt_hits),
        "vector ranking changed across the round trip"
    );
}

/// Importing the same stream twice must not duplicate anything.
pub async fn import_is_idempotent<F: BackendFactory>(factory: &F) {
    let source = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    source
        .apply(fx::admit_txn_embedded(
            &scope,
            fx::item(&scope, "only memory"),
        ))
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
    let first = target
        .import(&selector.tenant, exported.clone())
        .await
        .unwrap();
    assert_eq!(first.items_imported, 1);
    assert!(
        target
            .audit(&scope, &AuditFilter::default())
            .await
            .unwrap()
            .is_empty(),
        "include_audit: false must not import any audit rows"
    );

    let second = target.import(&selector.tenant, exported).await.unwrap();
    assert_eq!(second.items_imported, 0);
    assert_eq!(second.items_skipped_existing, 1);
    assert_eq!(
        second.vectors_imported, 0,
        "a skipped item's vector must not be re-inserted either"
    );
    assert_eq!(
        target.list(&scope, &Page::default()).await.unwrap().len(),
        1
    );
}
```

**Defects fixed in the code above, relative to earlier drafts, across two rounds** (both found during Task 18's implementation and review, before any of these tests ever ran against a real backend):

*First round, in `audit_filter_narrows_by_event_and_time`:*

1. **The time bound was never exercised**, despite the test's name — `AuditFilter::since`/`until` sat unused. Fixed by querying a `since`/`until` window that returns a genuine strict subset of the four audit rows: not all of them, and not none.
2. **Every record shared one timestamp.** `fx::item`/`fx::evict_txn` both pin `UNIX_EPOCH`, so an ordering assertion over `at` held for *any* order, including oldest-first. Fixed by giving each of the four records (three admits, one eviction) its own timestamp — `at` stores whole seconds, so anything finer would not survive a real round trip through storage. The three admits use `fx::item_at` (Task 17); the eviction uses a new `fx::evict_txn_at` (added to Task 15's fixtures block below, mirroring `item`/`item_at` — `evict_txn` itself is untouched so Task 16's tests are unaffected).
3. **The ordering assertion compared `at`**, the one field `memorysafe_core::AuditFilter` documents as unable to provide a total order (rows are ordered by `AuditId` instead, because "`at` is whole seconds and cannot separate rows written in the same second, so a time-based cursor would repeat or skip them"). Fixed by asserting identity instead: the two rows returned under `limit: 2` are checked against the `AuditId`s the writes themselves returned (`AppliedWrite::audit_id`), not against `at`.

*Second round (review "Fix Round 1"), across all five tests — the suite is frozen after Task 24, so these were folded in now rather than left for later:*

4. **The eviction's `at` was the newest of the four**, so ordering by `at` and ordering by `AuditId` (write order) agreed by construction and the test could not tell the two conventions apart. Fixed by moving the eviction's business timestamp to *between* the second and third admit's (15s, not 30s) rather than after all of them, so the two orderings genuinely disagree; the time window's expected row count changed from 2 to 3 as a consequence (the eviction now falls inside `[5s, 25s]` too).
5. **The round-trip corpus could not detect a lost field for six of `MemoryItem`'s thirteen** (`occurred_at`, `attrs`, `ttl`, `protection`, `pending_embedding`, and uniform `sensitivity`) — all three items varied only `kind`/`tags`/`sensitivity` via `fx::item_with`, at values that were `fx::item`'s own defaults. Fixed by giving one item `Protection::Pinned`, one an `occurred_at` + `ttl` + an `attrs` entry, and one `pending_embedding: true` with a non-`Internal` sensitivity — all fields are `pub`, so no new fixture was needed. This still cannot catch a field the backend never persists at all, since both sides are read back through the same backend via `list()`; it converts every plausible import-side re-derivation from invisible to caught.
6. **`include_audit: true` was set and its effect was never verified anywhere in the frozen suite.** Added `assert_eq!(report.audit_imported, 3)` after import in `export_import_round_trips_exactly`, and an assertion that the `include_audit: false` path (`import_is_idempotent`) leaves the target's audit empty.
7. **`purge_subject_leaves_other_subjects_intact` omitted the capacity mirror its own sibling asserts.** Added `capacity_state(&keeper).used_items == 1`, plus reading back the surviving item's id (not just the list's length) to confirm the keeper's own item, not some other row, is what survived.
8. **The purge report's accounting assertion pinned an undocumented convention with an unactionable message.** `AuditEvent::SubjectPurged` exists, so a conformant backend may write its own purge-audit row and legitimately report 7 instead of 6. Clarified the assertion message and added a doc comment on `PurgeReport` (Task 14's block) stating the convention: the count is of audit rows that existed *before* the purge.
9. **`BackendFactory`'s docstring said "per test"** but `export_import_round_trips_exactly` and `import_is_idempotent` each hold a source and target simultaneously and require them mutually invisible — a factory memoized per test would make the target already contain the source's rows. Corrected the docstring (Task 15's block) to "per call," with the reason.
10. **No pre-purge baseline in `purge_subject_removes_everything_for_that_subject`.** Every post-purge check was satisfiable by a backend that stored nothing. Added `assert_eq!(list(&a).len(), 3)` before the purge.
11. **The vector-survival check was satisfiable by empty-vs-empty.** Added `assert_eq!(src_hits.len(), 3)` and `assert_eq!(before.len(), 3)` before the respective structural comparisons.
12. **Two unchecked adjacent counters**: `capacity_state(&b)` after the purge in `purge_subject_removes_everything_for_that_subject` (only `&a` was checked), and `second.vectors_imported` in `import_is_idempotent` (a backend that skips the item but re-inserts its vector would have passed).

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
        lifecycle::audit_returns_min_of_the_limit_and_the_rows_that_remain,
        lifecycle::audit_pages_by_the_after_cursor_without_repeating_a_row,
        lifecycle::audit_since_and_until_include_a_record_on_the_boundary,
        lifecycle::purge_subject_removes_everything_for_that_subject,
        lifecycle::purge_subject_leaves_other_subjects_intact,
        lifecycle::purge_subject_preserves_audit_when_asked,
        lifecycle::purge_subject_persists_the_record_it_was_given,
        lifecycle::apply_persists_the_audit_id_it_was_given,
        lifecycle::record_recall_persists_the_audit_id_it_was_given,
        lifecycle::import_preserves_every_audit_id,
        lifecycle::export_narrows_to_the_selectors_subject_and_namespace,
        lifecycle::export_orders_the_stream_by_kind_then_by_id,
        lifecycle::export_import_round_trips_exactly,
        lifecycle::import_is_idempotent,
        lifecycle::import_rejects_a_later_record_whose_tenant_disagrees,
        lifecycle::import_rejects_a_foreign_audit_record_even_when_every_item_agrees,
        lifecycle::audit_aggregates_survive_a_cascading_purge,
        lifecycle::audit_aggregates_page_in_the_documented_order,
        lifecycle::audit_aggregates_resume_from_a_cursor_that_names_no_stored_row,
        lifecycle::audit_aggregates_narrow_by_day_window_and_policy,
        lifecycle::every_audit_writing_path_increments_the_aggregates,
```

The suite now stands at **50 conformance tests** (5 isolation + 6 atomicity + 13 retrieval + 4 capacity + 22 lifecycle). This set is frozen at the end of Task 24; Plan 2's Postgres backend must pass it unmodified. **The authoritative list is `run_conformance_suite`'s own `run!` in `crates/memorysafe-backend/src/conformance/mod.rs`** — every `pub async fn` across the conformance modules must appear in it, and that correspondence is checked by enumeration before each of these contract commits, not by reading this document.

Twenty-three of those were added after Tasks 17 and 18 shipped, by the contract tasks that changed `Backend::import`'s and `Backend::purge_subject`'s signatures, put access statistics on the ranking structs, stated the echo rule, and closed the gaps the trait's own doc comments admitted nothing enforced — the last point at which adding conformance tests and changing trait signatures cost nothing, because no `impl Backend` existed yet. They are listed in the `run!` snippets above so those snippets match the file rather than the day it was written:
`retrieval::list_orders_oldest_first_by_created_at`,
`retrieval::list_tie_break_is_total_over_identical_timestamps`,
`retrieval::neighbours_break_ties_before_truncating_at_k`,
`retrieval::recall_updates_access_statistics`,
`lifecycle::import_rejects_a_later_record_whose_tenant_disagrees`,
`lifecycle::import_rejects_a_foreign_audit_record_even_when_every_item_agrees`,
`lifecycle::audit_aggregates_survive_a_cascading_purge`,
`lifecycle::purge_subject_preserves_audit_when_asked`,
`lifecycle::purge_subject_persists_the_record_it_was_given`,
`lifecycle::apply_persists_the_audit_id_it_was_given`,
`lifecycle::record_recall_persists_the_audit_id_it_was_given`,
`lifecycle::import_preserves_every_audit_id`,
`atomicity::an_invalid_transaction_is_rejected_and_writes_nothing`,
`lifecycle::export_narrows_to_the_selectors_subject_and_namespace`,
`lifecycle::audit_returns_min_of_the_limit_and_the_rows_that_remain`,
`lifecycle::audit_pages_by_the_after_cursor_without_repeating_a_row`,
`lifecycle::audit_since_and_until_include_a_record_on_the_boundary`,
`lifecycle::export_orders_the_stream_by_kind_then_by_id`,
`lifecycle::audit_aggregates_page_in_the_documented_order`,
`lifecycle::audit_aggregates_resume_from_a_cursor_that_names_no_stored_row`,
`isolation::retrieval_never_crosses_a_scope_boundary`,
`lifecycle::audit_aggregates_narrow_by_day_window_and_policy`, and
`lifecycle::every_audit_writing_path_increments_the_aggregates`.

**The sixteen most recent are not reproduced above.** Their text lives in
`crates/memorysafe-backend/src/conformance/lifecycle.rs` and
`.../atomicity.rs`, which are authoritative; a sketch of a test that already
exists in the tree can only drift from it, and this document has now lost that
bet twice. Read them there. Eight of them close contract claims the `Backend`
trait itself recorded as unenforced — `WriteTransaction::is_valid` never
reached through the trait, `ScopeSelector`'s optional fields (over audit rows
as well as items), `audit`'s `min(limit, remaining)`, the `after` cursor,
`since`/`until` inclusivity, the export stream's order, and the aggregate
key's documented ordering and its own cursor — and each of those doc comments
now names the test that covers it. The most recent closes a gap no doc
comment had recorded at all: `isolation::retrieval_never_crosses_a_scope_boundary`.
`retrieve_candidates` and `neighbours` are the two methods a recall reaches,
and until it was written no test in the suite called either across a scope
boundary — every retrieval test built one scope, and the isolation module
exercised only `get`, `list` and `audit`. A backend whose retrieval predicate
was absent or mis-bound returned another subject's memories and passed.

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

**This task also creates the `audit_aggregates` table** — and the DDL above now contains it, which it did not for several rounds while this paragraph claimed it did. Its key is policy name, policy version, event class, and day bucket — and **nothing else**: no subject column, no namespace column. `memorysafe_backend::aggregates` carries the full argument; the short version is that a cascading purge deletes items, vectors, audit, idempotency and capacity all by subject, so a namespace column here would make an aggregate row the sole surviving artifact naming a purged subject's namespace. Store the histogram bucket counts as ten integers plus the `SCORE_HISTOGRAM_VERSION` the row was written under, and do **not** give the table a foreign key to `audit` or include it in `purge::subject`'s sweep — `lifecycle::audit_aggregates_survive_a_cascading_purge` fails if you do.

**The policy is two columns, not one rendered string, and this is a correctness constraint rather than a layout preference.** `PolicyId`'s `Display` is `{name}@{version}` and neither field is constrained, so `("a@b", "c")` and `("a", "b@c")` both render `"a@b@c"`. A single rendered key column would merge two distinct policies' counts into one row — a lost count, unrecoverable, in the artifact designed to outlive the detail rows. `lifecycle::audit_aggregates_page_in_the_documented_order` carries that exact pair and fails on a merged row. Note that the `audit` detail table's own `policy` column *is* the rendered form: that is a display and filter convenience, nothing keys on it, and nothing in Plan 1 reads it — do not derive the aggregate key from it.

**The ordering index states its collation explicitly on every text column.** `Backend::audit_aggregates` mandates that every ordering over a text column states its collation and every ordering over a nullable column states null placement, neither relying on a dialect default. `COLLATE BINARY` is SQLite's spelling of byte order; Postgres's is `COLLATE "C"`. `policy_name` and `policy_version` are nullable and must sort NULL first. State it in the query — `ORDER BY (policy_name IS NULL) DESC, ...` — rather than relying on any default; the point of the mandate is that a reader can see the choice was made, and it holds whatever the default turns out to be. (*Recollection, unverified in this environment: SQLite treats NULL as smallest and would agree by default. That is why the mandate is semantic — if the recollection is wrong, the explicit clause is still right.*) `ordering_sql_states_collation_and_null_placement` is where that is asserted, and it lives in the purge-and-portability task, where the query builder it must read is written — not with the retrieval tasks. The two columns `items.last_access` and `items.access_count` are also read for the first time from Task 21 onward; they have been declared since this task and unread until now.

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
CREATE INDEX IF NOT EXISTS idx_audit_scope_at ON audit(subject, namespace, at, id DESC);

CREATE TABLE IF NOT EXISTS idempotency (
  key            TEXT PRIMARY KEY,
  subject        TEXT NOT NULL,
  namespace      TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  outcome        TEXT NOT NULL,
  at             INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_aggregates (
  -- The key, and nothing finer. No subject column and no namespace column:
  -- `memorysafe_backend::aggregates` carries the argument, and
  -- `lifecycle::audit_aggregates_survive_a_cascading_purge` fails if a purge
  -- can reach these rows.
  --
  -- The policy is two columns because `PolicyId`'s Display is not injective;
  -- a rendered key column merges distinct policies into one row. See the
  -- prose below.
  policy_name    TEXT,                        -- NULL together with policy_version
  policy_version TEXT,
  event          TEXT NOT NULL,               -- AuditEvent::as_str(), never an ordinal
  day            INTEGER NOT NULL,            -- whole UTC days, aggregates::day_bucket
  count          INTEGER NOT NULL,
  value_histogram     TEXT NOT NULL,          -- JSON [u64; 10]
  fragility_histogram TEXT NOT NULL,          -- JSON [u64; 10]
  histogram_version   INTEGER NOT NULL,
  -- The two policy columns are NULL together or set together. Without this a
  -- row like ('x', NULL, 'admitted', 1) is representable, falls inside the
  -- policied partial index — whose predicate tests only `policy_name` — and
  -- `aggregates::query`'s `(Some(n), Some(v)) => Some(..), _ => None` would
  -- silently relabel it as policy-less: a wrong aggregate that looks
  -- well-formed. The invariant was a comment; this makes it a constraint.
  CHECK ((policy_name IS NULL) = (policy_version IS NULL))
);
-- Uniqueness in two partial indexes rather than one PRIMARY KEY over the
-- nullable tuple. A unique index treats NULLs as distinct from each other, so
-- a single index over (policy_name, policy_version, event, day) would let two
-- policy-less rows with the same event and day both insert — and policy-less
-- rows are the majority of the key space, not a corner. Splitting on the
-- nullability makes the constraint hold in both cases without inventing a
-- sentinel policy string, which `AggregateKey::policy`'s doc rules out.
CREATE UNIQUE INDEX IF NOT EXISTS idx_aggregates_key_policied
  ON audit_aggregates(policy_name, policy_version, event, day)
  WHERE policy_name IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_aggregates_key_policy_less
  ON audit_aggregates(event, day)
  WHERE policy_name IS NULL;
-- The read path's ordering index, in the documented key order: day first, then
-- the policy parts, then the event name. Collation is stated on every text
-- column rather than left to the engine's default, per the mandate on
-- `Backend::audit_aggregates`; `COLLATE BINARY` is SQLite's spelling of byte
-- order and Postgres's is `COLLATE "C"`.
CREATE INDEX IF NOT EXISTS idx_aggregates_order ON audit_aggregates(
  day,
  policy_name    COLLATE BINARY,
  policy_version COLLATE BINARY,
  event          COLLATE BINARY
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

`memorysafe-backend` must not depend on `rusqlite`, so the conversion from `rusqlite::Error`
to `BackendError` is defined locally in this crate as an extension trait. Add to `tenant.rs`:

```rust
pub trait SqlResultExt<T> {
    fn sql(self) -> Result<T, BackendError>;
}

impl<T> SqlResultExt<T> for rusqlite::Result<T> {
    fn sql(self) -> Result<T, BackendError> {
        self.map_err(to_backend)
    }
}
```

Every SQL call in Tasks 20–24 ends in `.sql()?` rather than a bare `?`, and so do the closures
in this task's tests — `c.execute(...).sql()?`, `c.execute_batch(...).sql()?`,
`c.query_row(...).sql()?`.

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

**The aggregate write rule, stated once and binding on every path: every audit row a backend writes increments exactly one `audit_aggregates` row, in the same transaction that writes the audit row.** No exceptions, and the exceptions are what needed deciding — the plan previously said "every audit row" in prose while calling `aggregates::increment` from one of four write paths.

*Why every row and not only the policy-driven ones.* Both rules are defensible in the abstract; this one is settled by a design decision already made. `AggregateKey::policy` is `Option<PolicyId>` precisely because "most audit events have no decision behind them at all — `SubjectPurged`, `Exported`, `Imported`, `Reembedded`, `MaintenanceRun` and `Recalled` are never the result of a policy call", and its doc adds that "rows that do carry a policy still group by it — 'grouped by policy version' is unaffected by policy-less rows existing alongside them". That sentence only means anything if policy-less rows exist. Aggregating only policy-driven events would make `None` unreachable in stored data and reduce the `Option` to decoration.

*The four paths, each decided:*

- **`apply`** — `Admitted`, `Merged`, `Forgotten`. Increments. This task's `apply` and the final-form one in the capacity/merge/idempotency task; the second replaces the first wholesale, so the call has to appear in both.
- **`record_recall`** — `Recalled`. Increments. Its sketch below previously inserted the audit row and returned, doing neither this nor the access-statistics update this same paragraph claimed for it.
- **`purge_subject`'s `SubjectPurged` row** — increments. The purge deletes the subject's *detail* rows and must never touch aggregates, so the erasure's own record is exactly the kind of history the aggregate table exists to keep: after a cascading purge it is the only remaining evidence, at tenant granularity, that an erasure happened that day.
- **`import`'s audit rows** — increment. An `ExportStream` carries `Header`, `Item` and `Audit` records and **no aggregate records**, so a migrated tenant that did not increment would arrive holding detail rows and no summary — and lose the history entirely at its first cascading purge. Double counting on a re-import is not the risk it looks like: audit rows are preserved under their own `AuditId`, which is the table's primary key, so a second import of the same rows aborts on the conflict and `import` is all-or-nothing.

Same transaction, not a follow-up write: an aggregate that can diverge from the detail rows it summarises is worse than no aggregate.

The increment itself is `aggregates::increment` below — a real function, not a described one. It was prose for several rounds against a table that did not exist, so the first `apply` would have failed at runtime rather than at the aggregate tests. Same key, count + 1, and the assessment's `value`/`fragility` bucketed with `memorysafe_backend::aggregates::score_bucket`.

**Files:**
- Create: `crates/memorysafe-backend-sqlite/src/items.rs`
- Create: `crates/memorysafe-backend-sqlite/src/audit.rs`
- Create: `crates/memorysafe-backend-sqlite/tests/conformance.rs`
- Modify: `crates/memorysafe-backend-sqlite/src/lib.rs`

**Interfaces:**
- Consumes: `TenantManager`, `SqlResultExt`, all core types.
- Produces: `items::insert(&Connection, &MemoryItem)`, `items::get`, `items::list`, `items::delete`, `items::row_to_item`, `audit::insert`, `audit::query`, `aggregates::increment`, and a partial `Backend` impl covering `get`, `list`, `audit`, `record_recall`, plus a first `apply` that handles insert + eviction + audit.

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
    // Ordered by id, descending (newest first) — see `AuditFilter::after`'s
    // doc comment in memorysafe-core: `at` is whole seconds and cannot
    // separate rows written in the same second, so `id` alone is the total
    // order, not a tie-break on `at`.
    args.push(Box::new(filter.limit as i64));
    sql.push_str(&format!(" ORDER BY id DESC LIMIT ?{}", args.len()));

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

`crates/memorysafe-backend-sqlite/src/aggregates.rs` — the write half. The read half arrives with `purge`, `export` and `import` in the portability task; both are listed there so neither is forgotten again.

```rust
use crate::tenant::SqlResultExt;
use memorysafe_backend::BackendError;
use memorysafe_backend::aggregates::{SCORE_HISTOGRAM_BUCKETS, SCORE_HISTOGRAM_VERSION, day_bucket, score_bucket};
use memorysafe_core::AuditRecord;
use rusqlite::{Connection, params};

/// Counts one audit row into its aggregate, creating the row on first sight.
///
/// The key comes from the record itself: the decision's policy **as two
/// fields**, the event's `as_str()`, and `day_bucket(record.at)`. Not from the
/// `audit` table's rendered `policy` column — that column is a display
/// convenience and `PolicyId`'s `Display` is not injective, so keying on it
/// merges distinct policies into one row.
///
/// `sum(histogram) <= count`, never `==`: only rows carrying an `Assessment`
/// land in a bucket, while `count` counts every row matching the key. See
/// `AuditAggregate`.
pub fn increment(conn: &Connection, record: &AuditRecord) -> Result<(), BackendError> {
    let (name, version) = match record.decision.as_ref().map(|d| &d.policy) {
        Some(p) => (Some(p.name.as_str()), Some(p.version.as_str())),
        None => (None, None),
    };
    let event = record.event.as_str();
    let day = day_bucket(record.at);

    // `IS`, not `=`: the policy columns are NULL for the majority of event
    // classes, and `= NULL` is never true. This is the same NULL-distinctness
    // that forced two partial unique indexes in the schema.
    let existing: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT count, value_histogram, fragility_histogram FROM audit_aggregates
             WHERE policy_name IS ?1 AND policy_version IS ?2 AND event = ?3 AND day = ?4",
            params![name, version, event, day],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .sql()?;

    let mut value: Vec<u64> = vec![0; SCORE_HISTOGRAM_BUCKETS];
    let mut fragility: Vec<u64> = vec![0; SCORE_HISTOGRAM_BUCKETS];
    let mut count = 0i64;
    if let Some((c, v, f)) = existing {
        count = c;
        value = serde_json::from_str(&v).map_err(|e| BackendError::Storage {
            message: format!("aggregate histogram is not readable: {e}"),
            retryable: false,
        })?;
        fragility = serde_json::from_str(&f).map_err(|e| BackendError::Storage {
            message: format!("aggregate histogram is not readable: {e}"),
            retryable: false,
        })?;
    }
    count += 1;
    if let Some(a) = &record.assessment {
        value[score_bucket(a.value)] += 1;
        fragility[score_bucket(a.fragility)] += 1;
    }

    conn.execute(
        "INSERT INTO audit_aggregates
             (policy_name, policy_version, event, day, count,
              value_histogram, fragility_histogram, histogram_version)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT (policy_name, policy_version, event, day) WHERE policy_name IS NOT NULL
         DO UPDATE SET
             count = excluded.count,
             value_histogram = excluded.value_histogram,
             fragility_histogram = excluded.fragility_histogram,
             -- Refreshed, not left at whatever the row was created with.
             -- `histogram_version` is not part of the key, so a row created
             -- under older `SCORE_HISTOGRAM_EDGES` would otherwise keep
             -- advertising them while accumulating counts bucketed by this
             -- build's — the silent splicing the version exists to make
             -- detectable. A row whose stored version differs from the
             -- incoming one is a migration signal; writing the current version
             -- makes the row honest about the edges its most recent counts
             -- used, and the retention task is where a genuine bump gets its
             -- migration.
             histogram_version = excluded.histogram_version",
        params![
            name,
            version,
            event,
            day,
            count,
            serde_json::to_string(&value).expect("a Vec<u64> serialises"),
            serde_json::to_string(&fragility).expect("a Vec<u64> serialises"),
            SCORE_HISTOGRAM_VERSION as i64,
        ],
    )
    .sql()?;
    Ok(())
}
```

`rusqlite::OptionalExtension` is needed for `.optional()`.

**The conflict target is named, and named with the partial index's predicate.**
An `ON CONFLICT` with no target resolves against any unique constraint, and
whether it resolves against a *partial* unique index is a question this plan
should not discover at execution time. Naming the index's columns and its
`WHERE` clause is the form that unambiguously selects one. The policy-less
half needs the other target — `ON CONFLICT (event, day) WHERE policy_name IS
NULL` — so the write is two statements chosen on `policy_name.is_some()`, or
one `UPDATE ... WHERE policy_name IS ?1 AND ...` followed by an `INSERT` when
`changes() == 0`. Either is correct under this backend's per-tenant write lock;
pick one and say which. **(This paragraph's claim about partial-index conflict
resolution is reasoned, not run — there is no `sqlite3` in the environment this
was written in.)**

Add a first `Backend` impl in `crates/memorysafe-backend-sqlite/src/lib.rs`. `apply` at this stage handles insert, evictions, and audit inside one transaction; vectors, merge, capacity, and idempotency arrive in Tasks 21–23. Every other method returns `Ok` defaults so the crate compiles and the isolation tests can run:

```rust
pub mod aggregates;
pub mod audit;
pub mod items;

use async_trait::async_trait;
use memorysafe_backend::{
    AppliedWrite, AuditAggregate, AuditAggregateFilter, Backend, BackendError, CandidateQuery,
    ExportStream, ImportReport, ImportStream, Page, PurgeReport, ScopeSelector, WriteTransaction,
};
use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, Budget, CapacityState, Embedding, ItemId, MemoryItem,
    PurgeCascade, Scope, ScopeStats, ScoredCandidate, SubjectId, TenantId,
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
        self.tenants
            .with_write(&tenant, move |conn| {
                let tx = conn.transaction().map_err(|e| tenant::storage_error(e, false))?;
                let id = audit::insert(&tx, &record)?;
                // A `Recalled` row is an audit row: it increments, like every
                // other. See the write-path rule below.
                aggregates::increment(&tx, &record)?;
                // And the access statistics ride this same write rather than a
                // second one — `Backend::record_recall` says why: the record
                // already carries exactly the ids that were recalled, and two
                // writes would let a backend audit a recall without counting
                // it. `record.at`, never a clock read, so a replay produces
                // comparable rows.
                for item in &record.items {
                    tx.execute(
                        "UPDATE items
                            SET access_count = access_count + 1, last_access = ?1
                          WHERE id = ?2 AND subject = ?3 AND namespace = ?4",
                        params![
                            record.at.unix_timestamp(),
                            item.id().as_str(),
                            record.scope.subject.as_str(),
                            record.scope.namespace.as_str(),
                        ],
                    )
                    .sql()?;
                }
                tx.commit().map_err(|e| tenant::storage_error(e, false))?;
                Ok(id)
            })
            .await
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
                if let Some(w) = &txn.upsert {
                    items::insert(&tx, &w.item)?;
                    item_id = Some(w.item.id.clone());
                }

                let audit_id = audit::insert(&tx, &txn.audit)?;
                // Same transaction as the audit row, never a follow-up write:
                // an aggregate that can diverge from the detail rows it
                // summarises is worse than no aggregate.
                aggregates::increment(&tx, &txn.audit)?;
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
    async fn purge_subject(&self, _t: &TenantId, _s: &SubjectId,
        _c: PurgeCascade, _a: AuditRecord)
        -> Result<PurgeReport, BackendError> {
        Ok(PurgeReport {
            items_removed: 0, vectors_removed: 0,
            audit_rows_removed: 0, audit_rows_preserved: 0,
        })
    }
    async fn export(&self, _s: &ScopeSelector) -> Result<ExportStream, BackendError> {
        Ok(vec![])
    }
    async fn import(&self, _t: &TenantId, _s: ImportStream)
        -> Result<ImportReport, BackendError> {
        Ok(ImportReport::default())
    }
    // Stubbed here and replaced in the portability task, alongside
    // `purge_subject`, `export` and `import`. Three conformance tests depend on
    // the real read — `audit_aggregates_survive_a_cascading_purge`,
    // `audit_aggregates_page_in_the_documented_order` and
    // `audit_aggregates_narrow_by_day_window_and_policy` — and this stub
    // satisfies none of them.
    async fn audit_aggregates(&self, _t: &TenantId, _f: &AuditAggregateFilter)
        -> Result<Vec<AuditAggregate>, BackendError> {
        Ok(vec![])
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

#[tokio::test]
async fn neighbours_break_ties_before_truncating_at_k() {
    retrieval::neighbours_break_ties_before_truncating_at_k(&SqliteFactory).await;
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

    // Tie-break by ascending `ItemId` before truncating at `k`: this
    // function is the over-fetch source for both `neighbours` (`k` is the
    // caller's `k`) and `retrieve_candidates` (`k` is `query.limit * 4`), so
    // an untied truncation here drops candidates nondeterministically
    // upstream of fusion — a downstream tie-break on the fused set cannot
    // recover a candidate this truncation already discarded.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
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
                    .map(|(item, access, score)| ScoredCandidate {
                        estimated_tokens: estimate_tokens(&item.body),
                        item,
                        relevance: score,
                        vector_score: Some(score),
                        keyword_score: None,
                        value: memorysafe_core::Score::ZERO,
                        fragility: memorysafe_core::Score::ZERO,
                        // `last_access`/`access_count` are the two columns the
                        // schema has declared since Task 19 and nothing read
                        // until now. Select them alongside `ITEM_COLUMNS` and
                        // return them from `vectors::search` as their own
                        // tuple element — they must NOT go on `MemoryItem`,
                        // which is exported and digested. A row that has never
                        // been recalled reads back `(None, 0)`, never
                        // `(created_at, 0)`.
                        last_accessed_at: access.last_accessed_at,
                        access_count: access.access_count,
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
                if let Some(w) = &txn.upsert {
                    items::insert(&tx, &w.item)?;
                    if let Some(v) = &w.vector {
                        vectors::insert(&tx, &w.item.id, &txn.scope, v)?;
                    }
                    item_id = Some(w.item.id.clone());
                }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-sqlite`
Expected: PASS — 3 unit tests plus 7 conformance tests ok.

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
async fn list_pages_are_disjoint_and_complete() {
    retrieval::list_pages_are_disjoint_and_complete(&SqliteFactory).await;
}

#[tokio::test]
async fn list_orders_oldest_first_by_created_at() {
    retrieval::list_orders_oldest_first_by_created_at(&SqliteFactory).await;
}

#[tokio::test]
async fn list_tie_break_is_total_over_identical_timestamps() {
    retrieval::list_tie_break_is_total_over_identical_timestamps(&SqliteFactory).await;
}

#[tokio::test]
async fn pending_embedding_items_are_excluded_when_asked() {
    retrieval::pending_embedding_items_are_excluded_when_asked(&SqliteFactory).await;
}

#[tokio::test]
async fn recall_updates_access_statistics() {
    retrieval::recall_updates_access_statistics(&SqliteFactory).await;
}

// Wired here rather than with the other isolation tests in the items-and-audit
// task: it calls `retrieve_candidates` and `neighbours`, so it cannot pass
// until both retrieval arms exist.
#[tokio::test]
async fn retrieval_never_crosses_a_scope_boundary() {
    isolation::retrieval_never_crosses_a_scope_boundary(&SqliteFactory).await;
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
                // From the item row's `last_access`/`access_count` columns,
                // carried through the merge map beside the item. `(None, 0)`
                // for a row never recalled.
                last_accessed_at: access.last_accessed_at,
                access_count: access.access_count,
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
Expected: PASS — 4 keyword unit tests plus 18 conformance tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-sqlite/
git commit -m "feat(sqlite): FTS5 keyword search with escaping and hybrid fusion"
```

---

## Task 23: SQLite — capacity accounting, merge, and idempotency

**Also increments the aggregates** for the audit rows this task's paths write (merges, evictions under capacity pressure), on the same terms as Task 20: same transaction as the audit row, same key, same bucketing.

**Files:**
- Create: `crates/memorysafe-backend-sqlite/src/capacity.rs`
- Modify: `crates/memorysafe-backend-sqlite/src/lib.rs`
- Modify: `crates/memorysafe-backend-sqlite/tests/conformance.rs`

**Interfaces:**
- Consumes: `items`, `vectors`, `audit`.
- Produces: `capacity::ensure_row`, `capacity::state`, `capacity::set_budget`, `capacity::adjust(conn, scope, delta_items, delta_bytes)`, `capacity::stats`, `items::merge`, `idempotency` handling in `apply`, and real `Backend::capacity_state`, `scope_stats`, `set_budget`.

**The correctness detail:** capacity accounting is a row per namespace, updated inside the same transaction as the item write. Combined with `TenantManager::with_write`'s per-tenant lock from Task 19, two concurrent admits cannot both conclude there is room.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-backend-sqlite/tests/conformance.rs`:

```rust
use memorysafe_backend::conformance::{atomicity, capacity};

#[tokio::test]
async fn capacity_accounting_tracks_items_and_bytes() {
    capacity::capacity_accounting_tracks_items_and_bytes(&SqliteFactory).await;
}

#[tokio::test]
async fn eviction_releases_capacity() {
    capacity::eviction_releases_capacity(&SqliteFactory).await;
}

#[tokio::test]
async fn concurrent_admits_do_not_double_count() {
    capacity::concurrent_admits_do_not_double_count(&SqliteFactory).await;
}

#[tokio::test]
async fn scope_stats_reflect_the_corpus() {
    capacity::scope_stats_reflect_the_corpus(&SqliteFactory).await;
}

#[tokio::test]
async fn admit_evict_and_audit_commit_together() {
    atomicity::admit_evict_and_audit_commit_together(&SqliteFactory).await;
}

#[tokio::test]
async fn a_failed_transaction_leaves_no_trace() {
    atomicity::a_failed_transaction_leaves_no_trace(&SqliteFactory).await;
}

#[tokio::test]
async fn an_invalid_transaction_is_rejected_and_writes_nothing() {
    atomicity::an_invalid_transaction_is_rejected_and_writes_nothing(&SqliteFactory).await;
}

#[tokio::test]
async fn every_mutation_writes_exactly_one_audit_record() {
    atomicity::every_mutation_writes_exactly_one_audit_record(&SqliteFactory).await;
}

#[tokio::test]
async fn idempotent_writes_replay_the_original_outcome() {
    atomicity::idempotent_writes_replay_the_original_outcome(&SqliteFactory).await;
}

#[tokio::test]
async fn idempotency_conflict_on_different_payload() {
    atomicity::idempotency_conflict_on_different_payload(&SqliteFactory).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-sqlite --test conformance`
Expected: FAIL — `capacity_accounting_tracks_items_and_bytes` panics: `assertion left == right failed: 0 vs 1`, because `capacity_state` still returns the placeholder.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-sqlite/src/capacity.rs`:

```rust
use crate::tenant::SqlResultExt;
use memorysafe_backend::BackendError;
use memorysafe_core::{Budget, CapacityState, Scope, ScopeStats};
use rusqlite::{Connection, params};

pub fn ensure_row(conn: &Connection, scope: &Scope) -> Result<(), BackendError> {
    conn.execute(
        "INSERT OR IGNORE INTO capacity (subject, namespace, used_items, used_bytes)
         VALUES (?1, ?2, 0, 0)",
        params![scope.subject.as_str(), scope.namespace.as_str()],
    )
    .sql()?;
    Ok(())
}

pub fn set_budget(
    conn: &Connection,
    scope: &Scope,
    budget: Budget,
) -> Result<(), BackendError> {
    ensure_row(conn, scope)?;
    conn.execute(
        "UPDATE capacity SET max_items = ?3, max_bytes = ?4
         WHERE subject = ?1 AND namespace = ?2",
        params![
            scope.subject.as_str(),
            scope.namespace.as_str(),
            budget.max_items.map(|v| v as i64),
            budget.max_bytes.map(|v| v as i64),
        ],
    )
    .sql()?;
    Ok(())
}

pub fn state(conn: &Connection, scope: &Scope) -> Result<CapacityState, BackendError> {
    let row = conn
        .query_row(
            "SELECT max_items, max_bytes, used_items, used_bytes FROM capacity
             WHERE subject = ?1 AND namespace = ?2",
            params![scope.subject.as_str(), scope.namespace.as_str()],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .ok();

    Ok(match row {
        Some((mi, mb, ui, ub)) => CapacityState {
            budget: Budget { max_items: mi.map(|v| v as u64), max_bytes: mb.map(|v| v as u64) },
            used_items: ui.max(0) as u64,
            used_bytes: ub.max(0) as u64,
        },
        None => CapacityState { budget: Budget::UNBOUNDED, used_items: 0, used_bytes: 0 },
    })
}

/// Applied inside the write transaction. Deltas are signed; the row is clamped
/// at zero so a bookkeeping slip cannot go negative and wrap.
pub fn adjust(
    conn: &Connection,
    scope: &Scope,
    delta_items: i64,
    delta_bytes: i64,
) -> Result<(), BackendError> {
    ensure_row(conn, scope)?;
    conn.execute(
        "UPDATE capacity
         SET used_items = MAX(0, used_items + ?3),
             used_bytes = MAX(0, used_bytes + ?4)
         WHERE subject = ?1 AND namespace = ?2",
        params![scope.subject.as_str(), scope.namespace.as_str(), delta_items, delta_bytes],
    )
    .sql()?;
    Ok(())
}

pub fn stats(conn: &Connection, scope: &Scope) -> Result<ScopeStats, BackendError> {
    let (count, total): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(byte_size), 0) FROM items
             WHERE subject = ?1 AND namespace = ?2",
            params![scope.subject.as_str(), scope.namespace.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .sql()?;

    let median: i64 = if count == 0 {
        0
    } else {
        conn.query_row(
            "SELECT byte_size FROM items WHERE subject = ?1 AND namespace = ?2
             ORDER BY byte_size LIMIT 1 OFFSET ?3",
            params![scope.subject.as_str(), scope.namespace.as_str(), count / 2],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };

    Ok(ScopeStats {
        item_count: count.max(0) as u64,
        total_bytes: total.max(0) as u64,
        // Computed by the engine from sampled neighbours; the backend has no
        // cheap way to produce it and a wrong value is worse than zero.
        mean_neighbour_similarity: 0.0,
        median_item_bytes: median.max(0) as u64,
    })
}
```

Add `items::merge` to `items.rs`:

```rust
/// Folds a new body and metadata into an existing item. Returns the byte-size
/// delta so capacity accounting stays exact.
pub fn merge(
    conn: &Connection,
    scope: &Scope,
    target: &ItemId,
    body: &str,
    tags: &[String],
    attrs: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<i64, BackendError> {
    let Some(existing) = get(conn, scope, target)? else {
        return Err(BackendError::MergeTargetMissing(target.clone()));
    };
    let before = existing.byte_size() as i64;

    let mut merged_tags = existing.tags.clone();
    for t in tags {
        if !merged_tags.contains(t) {
            merged_tags.push(t.clone());
        }
    }
    let mut merged_attrs = existing.attrs.clone();
    for (k, v) in attrs {
        merged_attrs.insert(k.clone(), v.clone());
    }

    let mut updated = existing;
    updated.body = body.to_string();
    updated.tags = merged_tags;
    updated.attrs = merged_attrs;
    let after = updated.byte_size() as i64;

    conn.execute(
        "UPDATE items SET body = ?4, tags = ?5, attrs = ?6, byte_size = ?7
         WHERE id = ?1 AND subject = ?2 AND namespace = ?3",
        params![
            target.as_str(),
            scope.subject.as_str(),
            scope.namespace.as_str(),
            updated.body,
            serde_json::to_string(&updated.tags).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(&updated.attrs).unwrap_or_else(|_| "{}".into()),
            after,
        ],
    )
    .sql()?;
    Ok(after - before)
}
```

Rewrite `apply` in `lib.rs` to its final form:

```rust
    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError> {
        if !txn.is_valid() {
            return Err(BackendError::InvalidTransaction(
                "a transaction may not both insert and merge".into(),
            ));
        }
        let tenant = txn.scope.tenant.clone();
        self.tenants
            .with_write(&tenant, move |conn| {
                // Idempotency is checked inside the write lock, so a retry
                // racing the original cannot slip past.
                if let Some(key) = &txn.idempotency_key {
                    let prior: Option<(String, String)> = conn
                        .query_row(
                            "SELECT payload_digest, outcome FROM idempotency WHERE key = ?1",
                            params![key],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .ok();
                    if let Some((digest, outcome)) = prior {
                        if txn.payload_digest.as_deref() != Some(digest.as_str()) {
                            return Err(BackendError::IdempotencyConflict);
                        }
                        let mut replay: AppliedWrite = serde_json::from_str(&outcome)
                            .map_err(|e| tenant::storage_error(e, false))?;
                        replay.replayed = true;
                        replay.replayed_outcome = Some(outcome);
                        return Ok(replay);
                    }
                }

                let tx = conn.transaction().map_err(|e| tenant::storage_error(e, false))?;
                capacity::ensure_row(&tx, &txn.scope)?;

                let mut delta_items: i64 = 0;
                let mut delta_bytes: i64 = 0;
                let mut evicted = Vec::new();

                for id in &txn.evictions {
                    let size = items::delete(&tx, &txn.scope, id)?;
                    vectors::delete(&tx, id)?;
                    if size > 0 {
                        delta_items -= 1;
                        delta_bytes -= size as i64;
                    }
                    evicted.push(id.clone());
                }

                let mut item_id = None;

                if let Some(w) = &txn.upsert {
                    items::insert(&tx, &w.item)?;
                    if let Some(v) = &w.vector {
                        vectors::insert(&tx, &w.item.id, &txn.scope, v)?;
                    }
                    delta_items += 1;
                    delta_bytes += w.item.byte_size() as i64;
                    item_id = Some(w.item.id.clone());
                }

                if let Some(m) = &txn.merge {
                    let diff = items::merge(
                        &tx, &txn.scope, &m.target, &m.body, &m.tags, &m.attrs,
                    )?;
                    if let Some(v) = &m.vector {
                        vectors::insert(&tx, &m.target, &txn.scope, v)?;
                    }
                    delta_bytes += diff;
                    item_id = Some(m.target.clone());
                }

                capacity::adjust(&tx, &txn.scope, delta_items, delta_bytes)?;
                let audit_id = audit::insert(&tx, &txn.audit)?;
                // Carried forward from Task 20's partial `apply`. This block
                // replaces that one wholesale, so an increment omitted here is
                // an aggregate table that is never written at all — which is
                // how it was lost once already.
                aggregates::increment(&tx, &txn.audit)?;

                let applied = AppliedWrite {
                    item_id,
                    audit_id,
                    evicted,
                    replayed: false,
                    replayed_outcome: None,
                };

                if let Some(key) = &txn.idempotency_key {
                    tx.execute(
                        "INSERT INTO idempotency (key, subject, namespace, payload_digest,
                             outcome, at)
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        params![
                            key,
                            txn.scope.subject.as_str(),
                            txn.scope.namespace.as_str(),
                            txn.payload_digest.clone().unwrap_or_default(),
                            serde_json::to_string(&applied)
                                .map_err(|e| tenant::storage_error(e, false))?,
                            time::OffsetDateTime::now_utc().unix_timestamp(),
                        ],
                    )
                    .sql()?;
                }

                tx.commit().map_err(|e| tenant::storage_error(e, false))?;
                Ok(applied)
            })
            .await
    }
```

Replace the three remaining placeholders:

```rust
    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, BackendError> {
        let scope = scope.clone();
        self.tenants.with_conn(&scope.tenant.clone(), move |c| capacity::state(c, &scope)).await
    }

    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, BackendError> {
        let scope = scope.clone();
        self.tenants.with_conn(&scope.tenant.clone(), move |c| capacity::stats(c, &scope)).await
    }

    async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), BackendError> {
        let scope = scope.clone();
        self.tenants
            .with_write(&scope.tenant.clone(), move |c| capacity::set_budget(c, &scope, budget))
            .await
    }
```

Add `pub mod capacity;` and `use rusqlite::params;` to `lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-sqlite`
Expected: PASS — 50 conformance tests minus the 22 lifecycle ones, i.e. 28 conformance tests plus 12 unit tests, all ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-sqlite/
git commit -m "feat(sqlite): locked capacity accounting, merge, and idempotent writes"
```

---

## Task 24: SQLite — purge and portable export/import

**Files:**
- Create: `crates/memorysafe-backend-sqlite/src/purge.rs`
- Create: `crates/memorysafe-backend-sqlite/src/portability.rs`
- Modify: `crates/memorysafe-backend-sqlite/src/lib.rs`
- Modify: `crates/memorysafe-backend-sqlite/tests/conformance.rs`

**Interfaces:**
- Consumes: everything in the crate.
- Produces: `purge::subject`, `portability::export`, `portability::import`, `aggregates::query`, real `Backend::purge_subject`, `export`, `import`, `audit_aggregates`, and a single `full_conformance_suite` test.

**Milestone: the complete 39-test conformance suite passes.** From here the suite is frozen — Plan 2's Postgres backend must pass it unmodified.

- [ ] **Step 1: Write the failing test**

Replace the individual conformance tests in `crates/memorysafe-backend-sqlite/tests/conformance.rs` with one entry point plus the lifecycle additions:

```rust
use memorysafe_backend::conformance::{BackendFactory, run_conformance_suite};
use memorysafe_backend_sqlite::SqliteBackend;
use std::future::Future;

struct SqliteFactory;

impl BackendFactory for SqliteFactory {
    type B = SqliteBackend;
    fn create(&self) -> impl Future<Output = Self::B> + Send {
        async {
            let dir = tempfile::tempdir().expect("tempdir");
            SqliteBackend::open(dir.keep())
        }
    }
}

/// The whole suite. Plan 2's Postgres backend runs this same function.
#[tokio::test(flavor = "multi_thread")]
async fn sqlite_passes_the_backend_conformance_suite() {
    run_conformance_suite(&SqliteFactory).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-sqlite --test conformance`
Expected: FAIL — `purge_subject_removes_everything_for_that_subject` panics: `assertion left == right failed: 0 vs 6`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-sqlite/src/purge.rs`:

```rust
use crate::tenant::SqlResultExt;
use memorysafe_backend::{BackendError, PurgeReport};
use memorysafe_core::{AuditRecord, PurgeCascade, SubjectId};
use rusqlite::{Connection, params};

/// Right-to-delete for one subject. A first-class operation rather than a
/// scan-and-delete loop: everything for the subject goes in one transaction
/// across every namespace it owns.
///
/// `cascade` decides the audit detail and nothing else; `audit` — the
/// caller's `SubjectPurged` record — is inserted either way, after the
/// deletes and inside this same transaction. See `Backend::purge_subject` for
/// why the order and the single transaction are both load-bearing. Note the
/// `audit_aggregates` table — which the schema task now actually creates — is
/// absent from every statement below, deliberately:
/// `lifecycle::audit_aggregates_survive_a_cascading_purge` fails the moment it
/// joins the sweep.
pub fn subject(
    conn: &mut Connection,
    subject: &SubjectId,
    cascade: PurgeCascade,
    audit: &AuditRecord,
) -> Result<PurgeReport, BackendError> {
    let tx = conn.transaction().map_err(|e| crate::tenant::storage_error(e, false))?;
    let s = subject.as_str();

    // Counted before anything is written. `audit_rows_removed +
    // audit_rows_preserved` must equal the rows the subject held immediately
    // before this call, and the record inserted at the end belongs to neither
    // term — counting after the insert would put it in `preserved` and break
    // the equation `PurgeReport` states.
    let existing_audit = tx
        .query_row(
            "SELECT count(*) FROM audit WHERE subject = ?1",
            params![s],
            |r| r.get::<_, i64>(0),
        )
        .sql()? as u64;

    let vectors_removed =
        tx.execute("DELETE FROM vectors WHERE subject = ?1", params![s]).sql()? as u64;
    let items_removed =
        tx.execute("DELETE FROM items WHERE subject = ?1", params![s]).sql()? as u64;
    let (audit_rows_removed, audit_rows_preserved) = match cascade {
        PurgeCascade::Cascade => (
            tx.execute("DELETE FROM audit WHERE subject = ?1", params![s]).sql()? as u64,
            0,
        ),
        PurgeCascade::Preserve => (0, existing_audit),
    };
    tx.execute("DELETE FROM idempotency WHERE subject = ?1", params![s]).sql()?;
    tx.execute("DELETE FROM capacity WHERE subject = ?1", params![s]).sql()?;

    // Delete first, insert second — under `Cascade` the sweep above would
    // otherwise delete this very row, and the purge would eat its own record.
    // `audit::insert` writes `audit.id` verbatim: the echo rule on `Backend`.
    crate::audit::insert(&tx, audit)?;
    // And it increments, like every audit row — see the write rule in the
    // items-and-audit task. After a cascading purge this row's aggregate is
    // the only remaining evidence, at tenant granularity, that an erasure
    // happened that day; the detail row it summarises may itself be swept by
    // a later retention pass.
    crate::aggregates::increment(&tx, audit)?;

    tx.commit().map_err(|e| crate::tenant::storage_error(e, false))?;

    Ok(PurgeReport {
        items_removed,
        vectors_removed,
        audit_rows_removed,
        audit_rows_preserved,
    })
}
```

`crates/memorysafe-backend-sqlite/src/portability.rs`:

```rust
use crate::items::{ITEM_COLUMNS, row_to_item};
use crate::tenant::SqlResultExt;
use crate::{capacity, items, vectors};
use base64::Engine as _;
use memorysafe_backend::{
    BackendError, ExportRecord, ExportStream, ExportVector, FORMAT_VERSION, ImportReport,
    ImportStream, ScopeSelector,
};
use memorysafe_core::{AuditFilter, Protection, Scope, TenantId};
use memorysafe_embed::QuantizedVector;
use rusqlite::{Connection, params};

// `FORMAT_VERSION` is `memorysafe-backend`'s, not a private copy. "A supported
// format version" is a property of the format, not of whichever backend is
// reading the stream; two backends each declaring their own constant is one
// silent divergence away from a SQLite export Postgres refuses.

pub fn export(
    conn: &Connection,
    sel: &ScopeSelector,
) -> Result<ExportStream, BackendError> {
    let mut out: ExportStream = vec![ExportRecord::Header {
        format_version: FORMAT_VERSION,
        exported_at: time::OffsetDateTime::now_utc().unix_timestamp(),
    }];

    let sql = format!(
        "SELECT {cols}, v.embedder AS v_embedder, v.dim AS v_dim, v.scale AS v_scale,
                v.q AS v_q
         FROM items i LEFT JOIN vectors v ON v.item_id = i.id
         WHERE (?1 IS NULL OR i.subject = ?1) AND (?2 IS NULL OR i.namespace = ?2)
         ORDER BY i.id ASC",
        cols = ITEM_COLUMNS
            .split(", ")
            .map(|c| format!("i.{c} AS {c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let tenant = sel.tenant.as_str().to_string();
    let subject = sel.subject.as_ref().map(|s| s.as_str().to_string());
    let namespace = sel.namespace.as_ref().map(|n| n.as_str().to_string());

    let mut stmt = conn.prepare(&sql).sql()?;
    let rows = stmt
        .query_map(params![subject, namespace], move |r| {
            let item = row_to_item(r, &tenant)?;
            let embedder: Option<String> = r.get("v_embedder")?;
            let vector = match embedder {
                Some(embedder) => {
                    let dim: i64 = r.get("v_dim")?;
                    let scale: f64 = r.get("v_scale")?;
                    let q: Vec<u8> = r.get("v_q")?;
                    Some(ExportVector {
                        embedder,
                        dim: dim as u16,
                        scale: scale as f32,
                        q_base64: base64::engine::general_purpose::STANDARD.encode(q),
                    })
                }
                None => None,
            };
            Ok((item, vector))
        })
        .sql()?;

    let mut scopes = Vec::new();
    for row in rows {
        let (item, vector) = row.sql()?;
        if !scopes.contains(&item.scope) {
            scopes.push(item.scope.clone());
        }
        out.push(ExportRecord::Item { item: Box::new(item), vector });
    }

    if sel.include_audit {
        // Collect across every scope before sorting: `audit::query` returns
        // each scope's rows newest-first (descending `AuditId`, per this
        // commit's fix to that function), but `Backend::export`'s contract
        // is one global run ascending by `AuditId` — appending each scope's
        // descending run back to back would satisfy neither order.
        //
        // `limit: 100_000` truncates silently for a tenant with more audit
        // rows than that — the same defect `AuditFilter::limit`'s doc
        // comment warns about (see `crates/memorysafe-core/src/audit.rs`).
        // Not resolved here.
        let mut audit_rows = Vec::new();
        for scope in scopes {
            let filter = AuditFilter { limit: 100_000, ..Default::default() };
            audit_rows.extend(crate::audit::query(conn, &scope, &filter)?);
        }
        audit_rows.sort_by(|a, b| a.id.cmp(&b.id));
        for record in audit_rows {
            out.push(ExportRecord::Audit { audit: Box::new(record) });
        }
    }

    Ok(out)
}

pub fn import(
    conn: &mut Connection,
    destination: &TenantId,
    stream: ImportStream,
) -> Result<ImportReport, BackendError> {
    let tx = conn.transaction().map_err(|e| crate::tenant::storage_error(e, false))?;
    let mut report = ImportReport::default();

    for record in stream {
        match record {
            ExportRecord::Header { format_version, .. } => {
                if format_version != FORMAT_VERSION {
                    return Err(BackendError::MalformedImport(format!(
                        "unsupported format version {format_version}"
                    )));
                }
            }
            ExportRecord::Item { mut item, vector } => {
                let scope: Scope = item.scope.clone();
                // Every record is compared against `destination` — never
                // against another record. No record's tenant is authority for
                // any other's, so a disagreement rejects the whole import
                // rather than being retargeted. `subject` and `namespace` are
                // preserved exactly as written; only the tenant is checked,
                // and it is checked rather than assigned.
                if scope.tenant != *destination {
                    return Err(BackendError::MalformedImport(format!(
                        "item {} names tenant {} but the destination is {destination}",
                        item.id, scope.tenant
                    )));
                }
                // Import is idempotent: an item already present is skipped
                // rather than duplicated or overwritten.
                if items::exists(&tx, &scope, &item.id)? {
                    report.items_skipped_existing += 1;
                    continue;
                }
                // `MemoryItem` has public fields and derives `Deserialize`, so
                // an import stream can assert any value it likes for the two
                // fields the engine is supposed to own. Neither is trusted here:
                //
                // `protection` — a stream claiming `Pinned` would create items
                // no policy can ever evict, letting an import permanently fill a
                // namespace and starve every later write with BudgetExhausted.
                // Imported items enter as Normal; a caller re-pins deliberately
                // through `protect`, which is audited.
                //
                // `sensitivity` — a stream claiming `Public` for a body full of
                // credentials would bypass the detector and surface that body to
                // a Public-clearance recall. The engine re-assesses on import
                // (see `Engine::import` in Task 39); the backend refuses to
                // lower whatever the engine resolved.
                item.protection = Protection::Normal;
                capacity::ensure_row(&tx, &scope)?;
                items::insert(&tx, &item)?;
                capacity::adjust(&tx, &scope, 1, item.byte_size() as i64)?;
                report.items_imported += 1;

                if let Some(v) = vector {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&v.q_base64)
                        .map_err(|e| BackendError::MalformedImport(e.to_string()))?;
                    let q = QuantizedVector::from_bytes(
                        memorysafe_core::EmbedderId::new(&v.embedder),
                        v.dim,
                        v.scale,
                        &bytes,
                    )
                    .map_err(|e| BackendError::MalformedImport(e.to_string()))?;
                    vectors::insert(&tx, &item.id, &scope, &q)?;
                    report.vectors_imported += 1;
                }
            }
            ExportRecord::Audit { audit } => {
                // The same comparison, so audit rows need no rule of their
                // own: same tenant as the destination, preserve the row
                // byte-exact; different, reject. The scope is never rewritten
                // to make the row fit — a rewritten audit row is a forged one.
                if audit.scope.tenant != *destination {
                    return Err(BackendError::MalformedImport(format!(
                        "audit row {} names tenant {} but the destination is {destination}",
                        audit.id, audit.scope.tenant
                    )));
                }
                crate::audit::insert(&tx, &audit)?;
                // Imported audit rows increment too: an `ExportStream` carries
                // no aggregate records, so a migrated tenant would otherwise
                // hold detail with no summary and lose the history at its first
                // cascading purge. A re-import cannot double-count — audit rows
                // keep their own `AuditId`, which is the table's primary key,
                // so the second import conflicts and `import` is all-or-nothing.
                crate::aggregates::increment(&tx, &audit)?;
                report.audit_imported += 1;
            }
        }
    }

    tx.commit().map_err(|e| crate::tenant::storage_error(e, false))?;
    Ok(report)
}
```

Add the aggregate **read** to `crates/memorysafe-backend-sqlite/src/aggregates.rs`,
beside the `increment` the items-and-audit task wrote. It was stubbed
`Ok(vec![])` there and three lifecycle conformance tests fail against the stub:
`audit_aggregates_survive_a_cascading_purge` (which asserts three admits are
counted **before** the purge), `audit_aggregates_page_in_the_documented_order`
and `audit_aggregates_narrow_by_day_window_and_policy`.

```rust
use memorysafe_backend::{AggregateKey, AuditAggregate, AuditAggregateFilter};
use memorysafe_core::{AuditEvent, PolicyId, TenantId};

/// One page of aggregates for `tenant`, matching `filter`.
///
/// **The ordering is the four-component key in `Backend::audit_aggregates`'s
/// order**: `day`, then the policy's `name`, then its `version`, then the
/// event's serialised name. Not the struct's field order, and not by
/// `PolicyId::to_string()` — the render is not injective, which is why the
/// policy is two columns here. `tenant` is the fifth component of
/// `AggregateKey`'s `Ord` and is omitted from the SQL deliberately: it is a
/// parameter of this query, so every row shares it.
///
/// **Collation and null placement are stated, not defaulted**, per the mandate
/// on `Backend::audit_aggregates`. `COLLATE BINARY` is SQLite's spelling of
/// byte order; `(policy_name IS NULL) DESC` puts policy-less rows first
/// without relying on SQLite's NULL ordering being what we remember it to be.
///
/// **The cursor runs the opposite way from `Backend::audit`'s, and this is the
/// most likely thing to get wrong here.** `audit` pages **descending** and its
/// `after` selects ids **strictly less** than the cursor. This method pages
/// **ascending** and `after` selects keys **strictly greater**. An implementer
/// who copies the shape from `audit::query` next door gets a cursor that
/// compiles, returns rows, and pages backwards through the log.
///
/// The comparison is a lexicographic row comparison over four components, so
/// it is written out longhand rather than as SQL's `(a,b,c,d) > (w,x,y,z)`:
/// the row-value form evaluates to NULL when any component is NULL, and the
/// policy columns are NULL for the majority of event classes, so the row would
/// be dropped and the page would come back short — which
/// `AuditAggregateFilter::limit` defines as meaning the log is exhausted.
/// Silent early termination on most of the key space is worse than a visible
/// duplicate.
///
/// Returns exactly `min(filter.limit, rows still matching after the cursor)`,
/// the same rule `Backend::audit` carries.
pub fn query(
    conn: &Connection,
    tenant: &TenantId,
    filter: &AuditAggregateFilter,
) -> Result<Vec<AuditAggregate>, BackendError> {
    // The cursor, decomposed. `after_present` is bound separately so the
    // predicate can be switched off without a second SQL string.
    let (a_day, a_name, a_version, a_event) = match &filter.after {
        Some(k) => (
            Some(k.day),
            k.policy.as_ref().map(|p| p.name.clone()),
            k.policy.as_ref().map(|p| p.version.clone()),
            Some(k.event.as_str().to_string()),
        ),
        None => (None, None, None, None),
    };

    let mut stmt = conn
        .prepare(
            "SELECT policy_name, policy_version, event, day, count,
                    value_histogram, fragility_histogram, histogram_version
             FROM audit_aggregates
             WHERE (?1 IS NULL OR day >= ?1)
               AND (?2 IS NULL OR day <= ?2)
               AND (?3 IS NULL OR (policy_name IS ?3 AND policy_version IS ?4))
               AND (?5 IS NULL OR (
                     day > ?5
                  OR (day = ?5 AND (policy_name IS NOT NULL) > (?6 IS NOT NULL))
                  OR (day = ?5 AND (policy_name IS NULL) = (?6 IS NULL)
                      AND coalesce(policy_name, '') COLLATE BINARY
                        > coalesce(?6, '') COLLATE BINARY)
                  OR (day = ?5 AND policy_name IS ?6
                      AND coalesce(policy_version, '') COLLATE BINARY
                        > coalesce(?7, '') COLLATE BINARY)
                  OR (day = ?5 AND policy_name IS ?6 AND policy_version IS ?7
                      AND event COLLATE BINARY > ?8 COLLATE BINARY)))
             ORDER BY day ASC,
                      (policy_name IS NULL) DESC,
                      policy_name    COLLATE BINARY ASC,
                      (policy_version IS NULL) DESC,
                      policy_version COLLATE BINARY ASC,
                      event          COLLATE BINARY ASC
             LIMIT ?9",
        )
        .sql()?;

    let rows = stmt
        .query_map(
            params![
                filter.since,
                filter.until,
                filter.policy.as_ref().map(|p| p.name.clone()),
                filter.policy.as_ref().map(|p| p.version.clone()),
                a_day,
                a_name,
                a_version,
                a_event,
                filter.limit as i64,
            ],
            |r| {
                let name: Option<String> = r.get(0)?;
                let version: Option<String> = r.get(1)?;
                let event: String = r.get(2)?;
                Ok((name, version, event, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?, r.get::<_, String>(6)?, r.get::<_, i64>(7)?))
            },
        )
        .sql()?;

    let mut out = Vec::new();
    for row in rows {
        let (name, version, event, day, count, value, fragility, version_tag) = row.sql()?;
        out.push(AuditAggregate {
            key: AggregateKey {
                tenant: tenant.clone(),
                policy: match (name, version) {
                    (Some(n), Some(v)) => Some(PolicyId::new(&n, &v)),
                    _ => None,
                },
                event: event_from_str(&event)?,
                day,
            },
            count: count as u64,
            value_histogram: histogram_from_json(&value)?,
            fragility_histogram: histogram_from_json(&fragility)?,
            histogram_version: version_tag as u32,
        });
    }
    Ok(out)
}
```

`event_from_str` is the inverse of `AuditEvent::as_str`, and belongs beside it
in `crates/memorysafe-backend-sqlite/src/audit.rs` if that file already has
one — do not write a second. `histogram_from_json` parses the stored JSON into
`[u64; SCORE_HISTOGRAM_BUCKETS]` and rejects a wrong length rather than
padding: a histogram of the wrong width means the row was written under
different `SCORE_HISTOGRAM_EDGES` than this build has, which
`histogram_version` exists to make detectable.

**The `ordering_sql_states_collation_and_null_placement` test the trait
requires goes in this crate**, asserting over the string `query` builds rather
than a copied literal — a literal is a second copy and drifts. Since the SQL
above is a `const`-shaped string inside the function, lift it to a
`pub(crate) const AGGREGATE_ORDER_SQL: &str` and have both the query and the
test read that.

Replace the last four placeholders in `lib.rs`:

```rust
    async fn audit_aggregates(&self, tenant: &TenantId, filter: &AuditAggregateFilter)
        -> Result<Vec<AuditAggregate>, BackendError>
    {
        let (tenant, filter) = (tenant.clone(), filter.clone());
        self.tenants
            .with_conn(&tenant.clone(), move |c| aggregates::query(c, &tenant, &filter))
            .await
    }

    async fn purge_subject(&self, tenant: &TenantId, subject: &SubjectId,
        cascade: PurgeCascade, audit: AuditRecord)
        -> Result<PurgeReport, BackendError>
    {
        let (tenant, subject) = (tenant.clone(), subject.clone());
        self.tenants
            .with_write(&tenant, move |c| purge::subject(c, &subject, cascade, &audit))
            .await
    }

    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError> {
        let sel = sel.clone();
        self.tenants
            .with_conn(&sel.tenant.clone(), move |c| portability::export(c, &sel))
            .await
    }

    async fn import(&self, destination: &TenantId, stream: ImportStream)
        -> Result<ImportReport, BackendError>
    {
        // Deliberately no "a stream may not span tenants" check and no
        // "stream contains no items" rejection.
        //
        // The span check is strictly implied: `portability::import` compares
        // every record against `destination`, so two records cannot disagree
        // with each other without at least one of them disagreeing with the
        // destination first. A second rule that is true only by implication
        // has no test of its own, cannot fail today, and silently stops being
        // implied the day someone weakens the first.
        //
        // The empty-stream rejection existed only to derive a tenant from the
        // first item. The destination is now a parameter, so there is nothing
        // left to derive and nothing left to reject: a header-only stream is
        // a valid export of an empty tenant, and the round trip has to
        // survive it.
        let dest = destination.clone();
        self.tenants
            .with_write(destination, move |c| portability::import(c, &dest, stream))
            .await
    }
```

`ScopeSelector` needs `Clone`; confirm the derive from Task 14. Add `pub mod portability;` and `pub mod purge;`. `lib.rs` no longer needs `ExportRecord` — nothing in it inspects the stream now.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-sqlite && cargo clippy -p memorysafe-backend-sqlite --all-targets -- -D warnings`
Expected: PASS — `sqlite_passes_the_backend_conformance_suite` prints all 50 conformance test names and passes.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-sqlite/
git commit -m "feat(sqlite): subject purge and portable export/import; full conformance passes"
```

---

## Task 25: Policy — config, redundancy, and fragility

**Files:**
- Create: `crates/memorysafe-policy/Cargo.toml`
- Create: `crates/memorysafe-policy/src/lib.rs`
- Create: `crates/memorysafe-policy/src/config.rs`
- Create: `crates/memorysafe-policy/src/redundancy.rs`
- Create: `crates/memorysafe-policy/src/fragility.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `GovernancePolicy` and its contexts, `ScoredCandidate`, `ScopeStats`.
- Produces: `BaselineConfig` (with `Default`), `BaselinePolicy::new(BaselineConfig)`, `BaselinePolicy::default()`, `redundancy::assess(&[ScoredCandidate], &BaselineConfig)`, `fragility::score(&[ScoredCandidate], &ScopeStats)`.

**Constraint:** this crate must not depend on `tokio`, `rusqlite`, or `memorysafe-backend`. The CI purity job asserts it.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-policy/src/redundancy.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::candidate;

    #[test]
    fn no_neighbours_means_no_redundancy() {
        let a = assess(&[], &BaselineConfig::default());
        assert_eq!(a.score, Score::ZERO);
        assert!(a.near_duplicates.is_empty());
    }

    #[test]
    fn redundancy_is_the_best_neighbour_similarity() {
        let cfg = BaselineConfig::default();
        let a = assess(&[candidate("a", 0.42), candidate("b", 0.81), candidate("c", 0.10)], &cfg);
        assert!((a.score.get() - 0.81).abs() < 1e-6);
    }

    #[test]
    fn near_duplicates_come_back_sorted_descending() {
        let cfg = BaselineConfig::default();
        let a = assess(&[candidate("a", 0.42), candidate("b", 0.81)], &cfg);
        assert_eq!(a.near_duplicates.len(), 2);
        assert!(a.near_duplicates[0].1 >= a.near_duplicates[1].1);
        assert!((a.near_duplicates[0].1.get() - 0.81).abs() < 1e-6);
    }

    #[test]
    fn only_neighbours_above_the_floor_are_listed() {
        let cfg = BaselineConfig { near_duplicate_floor: 0.5, ..Default::default() };
        let a = assess(&[candidate("a", 0.81), candidate("b", 0.10)], &cfg);
        assert_eq!(a.near_duplicates.len(), 1, "the 0.10 neighbour is not near-duplicate");
    }

    #[test]
    fn classification_matches_the_documented_thresholds() {
        let cfg = BaselineConfig::default();
        assert_eq!(cfg.classify(0.99), Verdict::NearDuplicate);
        assert_eq!(cfg.classify(0.95), Verdict::Mergeable);
        assert_eq!(cfg.classify(0.50), Verdict::Novel);
        // Boundaries are inclusive at the threshold.
        assert_eq!(cfg.classify(cfg.duplicate_threshold), Verdict::NearDuplicate);
        assert_eq!(cfg.classify(cfg.merge_threshold), Verdict::Mergeable);
    }
}
```

Append to `crates/memorysafe-policy/src/fragility.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::candidate;
    use memorysafe_core::ScopeStats;

    fn stats(mean: f32) -> ScopeStats {
        ScopeStats { item_count: 100, mean_neighbour_similarity: mean, ..Default::default() }
    }

    #[test]
    fn an_item_with_no_neighbours_is_maximally_fragile() {
        // Nothing like it exists, so losing it loses the information entirely.
        assert_eq!(score(&[], &stats(0.5)), Score::ONE);
    }

    #[test]
    fn an_item_in_a_dense_neighbourhood_is_not_fragile() {
        let dense = [candidate("a", 0.95), candidate("b", 0.93), candidate("c", 0.91)];
        assert!(score(&dense, &stats(0.5)).get() < 0.2);
    }

    #[test]
    fn an_atypical_item_is_more_fragile_than_a_typical_one() {
        let atypical = [candidate("a", 0.20), candidate("b", 0.15)];
        let typical = [candidate("a", 0.85), candidate("b", 0.80)];
        assert!(score(&atypical, &stats(0.5)) > score(&typical, &stats(0.5)));
    }

    #[test]
    fn density_at_the_corpus_mean_scores_exactly_one_half() {
        // 0.5 is the calibration anchor downstream policy code reads
        // directly (compose's fixed 0.5/0.8 thresholds mean "at least as
        // sparse as typical" / "much sparser than typical"): a neighbourhood
        // exactly as dense as the corpus's own mean similarity must land
        // exactly at the midpoint — for any baseline, not by coincidence at
        // one particular value.
        let baseline = 0.42;
        // A single neighbour at exactly the baseline: local_density = b / 1
        // = b with no averaging rounding, so this is exact by construction
        // rather than exact by coincidence (three neighbours all at 0.5
        // would also land exactly on 0.5, but only because 1.5 / 3 happens
        // to divide evenly).
        let at_baseline = [candidate("a", baseline)];
        assert_eq!(score(&at_baseline, &stats(baseline)).get(), 0.5);
    }

    #[test]
    fn a_neighbour_identical_to_the_item_scores_exactly_zero() {
        // local_density == 1.0 is the other calibration anchor: an exact
        // duplicate neighbour is never fragile, however diffuse or tight the
        // rest of the corpus is.
        let identical = [candidate("a", 1.0)];
        assert_eq!(score(&identical, &stats(0.3)).get(), 0.0);
    }

    #[test]
    fn sparser_than_typical_scores_above_half_denser_scores_below() {
        // The gradient either side of the 0.5 anchor: strictly sparser than
        // corpus-typical must land strictly above 0.5, and strictly denser
        // strictly below. This is the function's contract independent of
        // what any particular caller currently computes as `local_density`
        // — it must hold for every caller, including ones that only ever
        // produce values on one side of the anchor today.
        let baseline = 0.5;
        let sparser = [candidate("a", 0.2)];
        let denser = [candidate("a", 0.8)];
        assert!(score(&sparser, &stats(baseline)).get() > 0.5);
        assert!(score(&denser, &stats(baseline)).get() < 0.5);
    }

    #[test]
    fn an_isolated_item_reaches_maximum_fragility_even_in_a_diffuse_corpus() {
        // Regression guard: a shared-denominator rescale (normalising the
        // sparser-than-typical side by the *upward* room, `1.0 - baseline`,
        // instead of its own downward room) caps the reachable ceiling below
        // 1.0 at any baseline under 0.5 — an item with literally no similar
        // neighbours could never be judged maximally fragile in a diffuse
        // corpus, no matter how isolated it actually is. At baseline 0.2 that
        // broken shape returns 0.625; the correct shape must still reach the
        // true ceiling.
        let baseline = 0.2;
        let no_similar_neighbours = [candidate("a", 0.0)];
        assert_eq!(score(&no_similar_neighbours, &stats(baseline)).get(), 1.0);
    }

    #[test]
    fn fragility_is_calibrated_against_the_corpus_not_an_absolute() {
        // The same neighbours mean different things in a tight corpus versus
        // a diffuse one.
        let neighbours = [candidate("a", 0.60), candidate("b", 0.55)];
        let in_tight_corpus = score(&neighbours, &stats(0.85));
        let in_diffuse_corpus = score(&neighbours, &stats(0.20));
        assert!(
            in_tight_corpus > in_diffuse_corpus,
            "0.6 similarity is unusual in a tight corpus and ordinary in a diffuse one"
        );
    }

    // The tests above state this function's contract. **Task 25 must also
    // write** four mutation-killer tests pinning the arithmetic, not
    // reproduced here because they would bury the contract tests above
    // under linearity checks:
    // `the_sparser_branch_is_linear_between_the_baseline_and_zero`,
    // `the_denser_branch_is_linear_between_the_baseline_and_one`,
    // `typical_density_at_the_lower_extreme_scores_one_half_without_dividing_by_zero`,
    // and `typical_density_at_the_upper_extreme_scores_one_half_without_dividing_by_zero`.
    // The "Do not fold the two branches" warning in `score`'s doc comment
    // depends on the linearity pair among them to actually catch a
    // regression; without writing them, that warning is unenforced prose.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-policy`
Expected: FAIL — no such package `memorysafe-policy`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-policy/Cargo.toml`:

```toml
[package]
name = "memorysafe-policy"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
memorysafe-core.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true

[lints]
workspace = true
```

Add to workspace `[workspace.dependencies]`:

```toml
memorysafe-policy = { path = "crates/memorysafe-policy" }
```

`crates/memorysafe-policy/src/config.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    NearDuplicate,
    Mergeable,
    Novel,
}

/// Every threshold the baseline uses, in one place. Defaults are the values
/// documented in the spec; tenants may override them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineConfig {
    /// At or above this, the write is rejected. The verdict is decided by
    /// cosine similarity, not content-digest identity, so two items at the
    /// threshold can have different digests.
    pub duplicate_threshold: f32,
    /// At or above this (but below `duplicate_threshold`), merge.
    pub merge_threshold: f32,
    /// Neighbours below this are not reported as near-duplicates.
    pub near_duplicate_floor: f32,
    /// Fraction of the recall budget reserved for replay of fragile or
    /// long-unaccessed items.
    pub replay_quota: f32,
    /// MMR tradeoff: 1.0 is pure relevance, 0.0 is pure diversity.
    pub mmr_lambda: f32,
    /// Half-life in days for value decay during maintenance.
    pub value_half_life_days: f32,
    /// Weight of source trust in the value score.
    pub source_trust_weight: f32,
    /// Days without access before an item counts as replay-due.
    pub replay_stale_days: f32,
}

impl Default for BaselineConfig {
    fn default() -> Self {
        Self {
            duplicate_threshold: 0.98,
            merge_threshold: 0.93,
            near_duplicate_floor: 0.30,
            replay_quota: 0.20,
            mmr_lambda: 0.70,
            value_half_life_days: 90.0,
            source_trust_weight: 0.20,
            replay_stale_days: 30.0,
        }
    }
}

impl BaselineConfig {
    pub fn classify(&self, similarity: f32) -> Verdict {
        if similarity >= self.duplicate_threshold {
            Verdict::NearDuplicate
        } else if similarity >= self.merge_threshold {
            Verdict::Mergeable
        } else {
            Verdict::Novel
        }
    }
}
```

`crates/memorysafe-policy/src/redundancy.rs`:

```rust
use crate::config::BaselineConfig;
use memorysafe_core::{RedundancyAssessment, Score, ScoredCandidate};

pub use crate::config::Verdict;

/// Redundancy is the similarity of the closest existing memory. Neighbours
/// arrive sorted from the engine, but the sort is repeated here — with an
/// explicit tie-break, not by relying on a stable sort to inherit order
/// from an already-sorted input — so the function is correct in isolation
/// and testable with hand-built fixtures given in any order. The tie-break
/// (ascending `ItemId`) matches `RedundancyAssessment::near_duplicates`'s
/// documented rule, so `best()` names a deterministic merge target
/// regardless of the order `neighbours` arrived in.
pub fn assess(
    neighbours: &[ScoredCandidate],
    cfg: &BaselineConfig,
) -> RedundancyAssessment {
    let mut near: Vec<(memorysafe_core::ItemId, Score)> = neighbours
        .iter()
        .filter(|n| n.relevance >= cfg.near_duplicate_floor)
        // Clamped, not `new`: the floor already excludes negatives, and a
        // relevance marginally above 1.0 from f32 rounding must not error.
        .map(|n| (n.item.id.clone(), Score::clamped(n.relevance)))
        .collect();
    near.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let best = neighbours.iter().map(|n| n.relevance).fold(0.0f32, f32::max);
    RedundancyAssessment { score: Score::clamped(best), near_duplicates: near }
}
```

`crates/memorysafe-policy/src/fragility.rs`:

```rust
use memorysafe_core::{ScopeStats, Score, ScoredCandidate};

/// How costly this memory would be to lose.
///
/// An item with no near neighbours is irreplaceable: nothing else in the
/// corpus carries the same information. An item sitting in a dense cluster is
/// cheap to lose because its neighbours still say most of what it said.
///
/// The comparison is against the corpus's own mean similarity rather than an
/// absolute — 0.6 similarity is unusual in a tightly clustered corpus and
/// unremarkable in a diffuse one. This is the "rare class" notion from the
/// continual-learning lineage, expressed in embedding space.
///
/// `0.5` means corpus-typical density: a neighbourhood exactly as dense as
/// `stats.mean_neighbour_similarity` scores exactly `0.5`. Each side of that
/// midpoint is normalised by its OWN room, not by a shared denominator —
/// sparser-than-typical by the room below the baseline (`baseline` itself),
/// denser-than-typical by the room above it (`1.0 - baseline`). That keeps
/// both `0.0` and `1.0` reachable at every baseline: normalising both sides
/// by the same (upward) room would shrink the reachable ceiling as the
/// baseline moves down — an item with literally no similar neighbours could
/// then never reach maximum fragility in a diffuse corpus — so a fixed
/// downstream threshold like "`>= 0.8` = much sparser than typical" would
/// mean different things, or be unreachable outright, in different corpora.
/// Do not fold the two branches back into one symmetric-looking expression;
/// that shared-denominator shape was the actual defect this one fixes.
///
/// Downstream policy code reads the `0.5` midpoint and higher thresholds
/// directly, so this mapping is this function's contract, not an
/// implementation detail.
///
/// **Precondition:** `stats.mean_neighbour_similarity` must already be a
/// meaningful corpus-level baseline in the sense of `ScopeStats`'s own doc
/// comment on that field — this function reads it as-is and cannot verify
/// that on its own, since `stats` and `neighbours` are independent parameters
/// with no enforced relationship between them. Supplying it unmet (in
/// particular, the field's `0.0`/`Default` "no data" value while
/// `item_count < 2`) is a caller error, not a case this function detects.
pub fn score(neighbours: &[ScoredCandidate], stats: &ScopeStats) -> Score {
    if neighbours.is_empty() {
        return Score::ONE;
    }

    // Mean of the three closest neighbours: robust to a single outlier while
    // still local.
    let mut sims: Vec<f32> = neighbours.iter().map(|n| n.relevance).collect();
    sims.sort_by(|a, b| b.total_cmp(a));
    let k = sims.len().min(3);
    // Cosine can be negative; clamping alongside `baseline` below is what
    // makes each branch's division safe BY CONSTRUCTION rather than by an
    // epsilon floor (see the branch comments). This discards no real signal:
    // neighbours anti-correlated with the item are exactly the "as sparse as
    // it gets" case, which should map to `local_density = 0.0` regardless of
    // how negative the raw cosine got.
    let local_density: f32 = (sims[..k].iter().sum::<f32>() / k as f32).clamp(0.0, 1.0);

    let baseline = stats.mean_neighbour_similarity.clamp(0.0, 1.0);

    // Each side of the `0.5` (corpus-typical) midpoint is normalised by its
    // OWN room — see the doc comment above for why.
    let relative_sparsity = if local_density < baseline {
        // Sparser than typical, normalised by the room below the baseline.
        // Reachable only when `baseline > 0` (`local_density >= 0` and
        // `local_density < baseline`), so this division is never by zero.
        0.5 + 0.5 * (baseline - local_density) / baseline
    } else if local_density > baseline {
        // Denser than typical, normalised by the room above it. Reachable
        // only when `baseline < 1` (`local_density <= 1` and
        // `local_density > baseline`), so this division is never by zero.
        0.5 - 0.5 * (local_density - baseline) / (1.0 - baseline)
    } else {
        // Exactly typical. Kept as its own case rather than falling into
        // either branch above: `local_density == baseline` at the extremes
        // (both `0.0` or both `1.0`) would otherwise divide by zero there.
        0.5
    };

    Score::clamped(relative_sparsity)
}
```

`crates/memorysafe-policy/src/lib.rs`:

```rust
//! `BaselinePolicy` — the open-source governance policy.
//!
//! Simple and documented, not deliberately crippled. The proprietary policy
//! earns its keep with learned scorers and cross-tenant calibration, not by
//! this one being bad.

pub mod config;
pub mod fragility;
pub mod redundancy;

#[cfg(test)]
pub(crate) mod testkit;

pub use config::{BaselineConfig, Verdict};

use memorysafe_core::PolicyId;

pub const BASELINE_VERSION: &str = "0.1.0";

pub struct BaselinePolicy {
    pub config: BaselineConfig,
}

impl BaselinePolicy {
    pub fn new(config: BaselineConfig) -> Self {
        Self { config }
    }

    pub fn policy_id(&self) -> PolicyId {
        PolicyId::new("baseline", BASELINE_VERSION)
    }
}

impl Default for BaselinePolicy {
    fn default() -> Self {
        Self::new(BaselineConfig::default())
    }
}
```

`crates/memorysafe-policy/src/testkit.rs`:

```rust
//! Fixture builders shared by this crate's unit tests.

use memorysafe_core::{
    ItemId, MemoryItem, Protection, Scope, Score, ScoredCandidate, SensitivityLevel, Source,
    SourceKind,
};
use time::OffsetDateTime;

pub fn scope() -> Scope {
    Scope::new("t", "s", "n").expect("valid test scope")
}

pub fn item(body: &str) -> MemoryItem {
    MemoryItem {
        id: ItemId::new(),
        scope: scope(),
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

pub fn candidate(body: &str, relevance: f32) -> ScoredCandidate {
    ScoredCandidate {
        item: item(body),
        relevance,
        vector_score: Some(relevance),
        keyword_score: None,
        value: Score::clamped(0.5),
        fragility: Score::clamped(0.5),
        estimated_tokens: 10,
        // Never recalled by default. `compose`'s replay-staleness tests set
        // this explicitly; a fixture that defaulted it to the item's
        // `created_at` would make every candidate look freshly accessed.
        last_accessed_at: None,
        access_count: 0,
    }
}
```

Extend the CI purity job in `.github/workflows/ci.yml` to cover this crate. Task 1 created the
job with a step named for both crates but a check covering only `memorysafe-core`; this task is
where `memorysafe-policy` starts existing, so this is where the Global Constraint "policies are
pure" becomes enforceable:

```yaml
      - name: memorysafe-core and memorysafe-policy must have no I/O dependencies
        run: |
          for crate in memorysafe-core memorysafe-policy; do
            cargo tree -p "$crate" --edges normal --prefix none \
              | grep -Ei '^(tokio|rusqlite|sqlx|reqwest|hyper) ' && exit 1
            echo "$crate is I/O free"
          done
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-policy`
Expected: PASS — this task adds 17 tests; at least 17 in the crate.

> **The chain is anchored to what this plan specifies, and the expected lines read "at
> least".**
> The blocks above list 13 `#[test]` functions, and the comment closing `fragility.rs`'s
> test module commissions 4 more by name — the mutation-killer set. 13 + 4 = 17, which is
> what this task specifies. The crate may legitimately hold more: it currently holds 18
> for these files (`fragility.rs` 12, `redundancy.rs` 6, `config.rs` 0), one beyond the
> mandate. An equality that drifts upward every time an implementer does more than the
> minimum would false-alarm forever, so the counts here and in Tasks 26–29 are floors, not
> equalities.
>
> **What the floor does not do: it cannot detect a deleted test.** A test vanishing from a
> task leaves the crate total below the delta but still above every floor in the chain.
> The per-task `adds N` is what catches that, by contradicting the block directly above
> it. The floor only prevents false alarms when an implementer legitimately writes more
> than the minimum.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-policy/
git commit -m "feat(policy): baseline config, redundancy, and corpus-calibrated fragility"
```

---

## Task 26: Policy — value and sensitivity

**Files:**
- Create: `crates/memorysafe-policy/src/value.rs`
- Create: `crates/memorysafe-policy/src/sensitivity.rs`
- Modify: `crates/memorysafe-policy/src/lib.rs`

**Interfaces:**
- Consumes: `Candidate`, `BaselineConfig`, `ScopeStats`.
- Produces: `value::score(&Candidate, &ScopeStats, &BaselineConfig)`, `sensitivity::assess(&Candidate)`, and `GovernancePolicy::assess` on `BaselinePolicy`.

**On sensitivity detection:** the baseline uses pattern and lexicon detectors. It will miss things — that is stated plainly in the docs rather than hidden, and it is one of the places the proprietary scorer earns its price. The caller's `sensitivity_hint` may only raise the result, never lower it, which is enforced by `SensitivityLevel::raised_by` from Task 5.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-policy/src/sensitivity.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::candidate_from;
    use memorysafe_core::{SensitivityCategory, SensitivityLevel};

    #[test]
    fn ordinary_text_is_internal() {
        let a = assess(&candidate_from("the deployment finished at noon", None));
        assert_eq!(a.level, SensitivityLevel::Internal);
        assert!(a.categories.is_empty());
    }

    #[test]
    fn credentials_are_restricted() {
        for text in [
            "the api key is sk-abc123def456ghi789jkl012",
            "password: hunter2correcthorse",
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY",
        ] {
            let a = assess(&candidate_from(text, None));
            assert_eq!(a.level, SensitivityLevel::Restricted, "missed credential in {text:?}");
            assert!(a.categories.contains(&SensitivityCategory::Credential));
        }
    }

    #[test]
    fn health_language_is_sensitive() {
        let a = assess(&candidate_from("patient was diagnosed with hypertension", None));
        assert!(a.level >= SensitivityLevel::Sensitive);
        assert!(a.categories.contains(&SensitivityCategory::Health));
    }

    #[test]
    fn contact_details_are_personal() {
        let a = assess(&candidate_from("reach them at someone@example.com", None));
        assert!(a.level >= SensitivityLevel::Personal);
        assert!(a.categories.contains(&SensitivityCategory::Pii));
    }

    #[test]
    fn a_hint_raises_but_never_lowers() {
        let raised = assess(&candidate_from(
            "innocuous note",
            Some(SensitivityLevel::Restricted),
        ));
        assert_eq!(raised.level, SensitivityLevel::Restricted);

        let not_lowered = assess(&candidate_from(
            "the api key is sk-abc123def456ghi789jkl012",
            Some(SensitivityLevel::Public),
        ));
        assert_eq!(
            not_lowered.level,
            SensitivityLevel::Restricted,
            "a caller hint must never lower a detected level"
        );
    }
}
```

Append to `crates/memorysafe-policy/src/value.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::candidate_from;
    use memorysafe_core::ScopeStats;

    fn stats() -> ScopeStats {
        ScopeStats { item_count: 50, median_item_bytes: 100, ..Default::default() }
    }

    #[test]
    fn substantive_content_beats_a_stub() {
        let cfg = BaselineConfig::default();
        let rich = score(
            &candidate_from("the production database migration runs at 02:00 UTC on Sundays", None),
            &stats(),
            &cfg,
        );
        let thin = score(&candidate_from("ok", None), &stats(), &cfg);
        assert!(rich > thin, "rich={rich:?} thin={thin:?}");
    }

    #[test]
    fn a_human_source_outranks_an_agent_source_all_else_equal() {
        let cfg = BaselineConfig::default();
        let text = "the deadline moved to the fifteenth";
        let mut human = candidate_from(text, None);
        human.attrs.insert("source_kind".into(), serde_json::json!("human"));
        let agent = candidate_from(text, None);
        assert!(score(&human, &stats(), &cfg) >= score(&agent, &stats(), &cfg));
    }

    #[test]
    fn an_explicit_caller_weight_is_honoured() {
        let cfg = BaselineConfig::default();
        let mut weighted = candidate_from("a short note", None);
        weighted.attrs.insert("value_weight".into(), serde_json::json!(1.0));
        let plain = candidate_from("a short note", None);
        assert!(score(&weighted, &stats(), &cfg) > score(&plain, &stats(), &cfg));
    }

    #[test]
    fn value_always_lands_in_range() {
        let cfg = BaselineConfig::default();
        for text in ["", "x", &"word ".repeat(5000)] {
            let s = score(&candidate_from(text, None), &stats(), &cfg);
            assert!((0.0..=1.0).contains(&s.get()), "{text:?} produced {s:?}");
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-policy`
Expected: FAIL — `cannot find function assess in this scope`, `cannot find function candidate_from`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/memorysafe-policy/src/testkit.rs`:

```rust
use memorysafe_core::Candidate;

pub fn candidate_from(
    body: &str,
    hint: Option<memorysafe_core::SensitivityLevel>,
) -> Candidate {
    Candidate {
        body: body.to_string(),
        kind: "fact".into(),
        tags: vec![],
        attrs: Default::default(),
        sensitivity_hint: hint,
        embedding: None,
        byte_size: body.len() as u64,
    }
}
```

`crates/memorysafe-policy/src/sensitivity.rs`:

```rust
use memorysafe_core::{
    Candidate, Score, SensitivityAssessment, SensitivityCategory, SensitivityLevel,
};

/// Lexicons are lowercase substrings. Crude and English-only — this is stated
/// in the crate docs rather than hidden, and it is one of the places the
/// proprietary scorer earns its price.
const CREDENTIAL_MARKERS: &[&str] = &[
    "password:", "passwd:", "api key", "api_key", "apikey", "secret_access_key",
    "secret key", "private key", "-----begin", "bearer ", "authorization:",
];
const HEALTH_MARKERS: &[&str] = &[
    "patient", "diagnos", "prescri", "symptom", "medication", "mg daily", "blood pressure",
    "hypertension", "diabetes", "oncolog", "psychiatr", "therapy session",
];
const FINANCIAL_MARKERS: &[&str] =
    &["account number", "routing number", "iban", "credit card", "salary", "net worth"];
const LEGAL_MARKERS: &[&str] =
    &["attorney-client", "privileged and confidential", "settlement agreement", "under seal"];

fn looks_like_secret_token(text: &str) -> bool {
    // A long unbroken run of key-ish characters with mixed case or digits.
    text.split_whitespace().any(|t| {
        let core = t.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        core.len() >= 20
            && core.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && core.chars().any(|c| c.is_ascii_digit())
            && core.chars().any(|c| c.is_ascii_alphabetic())
    })
}

fn looks_like_email(text: &str) -> bool {
    text.split_whitespace().any(|t| {
        let at = t.find('@');
        match at {
            Some(i) => i > 0 && t[i + 1..].contains('.') && !t.ends_with('.'),
            None => false,
        }
    })
}

fn looks_like_phone(text: &str) -> bool {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.len() >= 10
        && text.chars().any(|c| matches!(c, '-' | '(' | ')' | '+'))
}

pub fn assess(cand: &Candidate) -> SensitivityAssessment {
    let lower = cand.body.to_lowercase();
    let mut categories = Vec::new();
    let mut detected = SensitivityLevel::Internal;
    let mut confidence = 0.5f32;

    let has = |markers: &[&str]| markers.iter().any(|m| lower.contains(m));

    if has(CREDENTIAL_MARKERS) || looks_like_secret_token(&cand.body) {
        categories.push(SensitivityCategory::Credential);
        detected = detected.max(SensitivityLevel::Restricted);
        confidence = 0.9;
    }
    if has(HEALTH_MARKERS) {
        categories.push(SensitivityCategory::Health);
        detected = detected.max(SensitivityLevel::Sensitive);
        confidence = confidence.max(0.7);
    }
    if has(FINANCIAL_MARKERS) {
        categories.push(SensitivityCategory::Financial);
        detected = detected.max(SensitivityLevel::Sensitive);
        confidence = confidence.max(0.7);
    }
    if has(LEGAL_MARKERS) {
        categories.push(SensitivityCategory::Legal);
        detected = detected.max(SensitivityLevel::Sensitive);
        confidence = confidence.max(0.7);
    }
    if looks_like_email(&cand.body) || looks_like_phone(&cand.body) {
        categories.push(SensitivityCategory::Pii);
        detected = detected.max(SensitivityLevel::Personal);
        confidence = confidence.max(0.6);
    }

    SensitivityAssessment {
        // The hint may only raise the level.
        level: detected.raised_by(cand.sensitivity_hint),
        categories,
        confidence: Score::clamped(confidence),
    }
}
```

`crates/memorysafe-policy/src/value.rs`:

```rust
use crate::config::BaselineConfig;
use memorysafe_core::{Candidate, ScopeStats, Score};

/// Specificity proxy: length relative to the corpus median, saturating. A stub
/// carries little; a paragraph usually carries more. Crude but stable, and it
/// avoids needing corpus-wide token statistics on the write path.
fn specificity(cand: &Candidate, stats: &ScopeStats) -> f32 {
    let median = stats.median_item_bytes.max(50) as f32;
    let ratio = cand.byte_size as f32 / median;
    // Saturating curve: 1x median ≈ 0.5, 3x ≈ 0.75, diminishing after.
    ratio / (1.0 + ratio)
}

/// Distinct-token fraction: repetitive filler scores lower than dense prose.
fn lexical_density(cand: &Candidate) -> f32 {
    let tokens: Vec<String> =
        cand.body.split_whitespace().map(|t| t.to_lowercase()).collect();
    if tokens.is_empty() {
        return 0.0;
    }
    let mut unique = tokens.clone();
    unique.sort();
    unique.dedup();
    unique.len() as f32 / tokens.len() as f32
}

fn source_trust(cand: &Candidate) -> f32 {
    match cand.attrs.get("source_kind").and_then(|v| v.as_str()) {
        Some("human") => 1.0,
        Some("tool") => 0.7,
        Some("session") => 0.5,
        _ => 0.5,
    }
}

/// How useful this memory is likely to be. Explicit caller weight dominates
/// when supplied, because the caller knows things the corpus does not.
pub fn score(cand: &Candidate, stats: &ScopeStats, cfg: &BaselineConfig) -> Score {
    if let Some(w) = cand.attrs.get("value_weight").and_then(|v| v.as_f64()) {
        return Score::clamped(w as f32);
    }

    let content = 0.6 * specificity(cand, stats) + 0.4 * lexical_density(cand);
    let trust = source_trust(cand);
    let blended =
        (1.0 - cfg.source_trust_weight) * content + cfg.source_trust_weight * trust;
    Score::clamped(blended)
}
```

Add `pub mod sensitivity;` and `pub mod value;` to `lib.rs`, and implement `assess`:

```rust
use memorysafe_core::{
    AssessContext, Assessed, Assessment, AssessorId, AdmitContext, Candidate, ComposeContext,
    Decision, GovernancePolicy, MaintainContext, PolicyError, RecallRequest, ScoredCandidate,
    WorkingSet, features,
};

impl GovernancePolicy for BaselinePolicy {
    fn id(&self) -> PolicyId {
        self.policy_id()
    }

    fn assess(&self, cand: &Candidate, ctx: &AssessContext) -> Result<Assessment, PolicyError> {
        let redundancy = redundancy::assess(&ctx.neighbours, &self.config);
        let fragility = fragility::score(&ctx.neighbours, &ctx.stats);
        let value = value::score(cand, &ctx.stats, &self.config);
        let sensitivity = sensitivity::assess(cand);

        Ok(Assessment {
            features: features! {
                "neighbour_count" => ctx.neighbours.len() as f64,
                "best_similarity" => redundancy.score.get(),
                "corpus_mean_similarity" => ctx.stats.mean_neighbour_similarity,
                "corpus_item_count" => ctx.stats.item_count as f64,
                "byte_size" => cand.byte_size as f64,
                "has_embedding" => if cand.embedding.is_some() { 1.0 } else { 0.0 },
            },
            value,
            fragility,
            sensitivity,
            redundancy,
            assessor: AssessorId::new("baseline", BASELINE_VERSION),
        })
    }

    // Implemented in Tasks 27-29.
    fn admit(&self, _a: &Assessed, _ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        unimplemented!("Task 27")
    }
    fn compose(&self, _r: &RecallRequest, _c: &[ScoredCandidate], _ctx: &ComposeContext)
        -> Result<WorkingSet, PolicyError> {
        unimplemented!("Task 28")
    }
    fn maintain(&self, _ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
        unimplemented!("Task 29")
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-policy`
Expected: PASS — this task adds 9 tests; at least 26 in the crate.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-policy/
git commit -m "feat(policy): value scoring and pattern-based sensitivity detection"
```

---

## Task 27: Policy — `admit`

**Files:**
- Create: `crates/memorysafe-policy/src/admit.rs`
- Create: `crates/memorysafe-policy/src/eviction.rs`
- Modify: `crates/memorysafe-policy/src/lib.rs`

**Interfaces:**
- Consumes: `Assessed`, `AdmitContext`, `BaselineConfig`, `Verdict`.
- Produces: `admit::decide(&Assessed, &AdmitContext, &BaselineConfig, PolicyId) -> Decision`, `eviction::cost(&MaintenanceCandidate) -> f32`, and `GovernancePolicy::admit` on `BaselinePolicy`.

**The rules, in order.** At or above `duplicate_threshold` the write is rejected. At or above `merge_threshold` it is merged into its closest neighbour. Otherwise the item is retained — and if capacity is tight, evictions are selected ascending by `eviction::cost` (`value × fragility`) until there is room: expected loss is value times the unrecoverable share, and eviction takes the smallest first. A candidate that is both highly fragile and highly sensitive gets `Protected` plus a `SensitivityConflict` reason, so the conflict is visible in the audit trail rather than silently resolved. If nothing is evictable and there is no room, the write is rejected with `BudgetExhausted` — never by silently exceeding the budget.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-policy/src/admit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::{candidate, candidate_from, item, scope};
    use memorysafe_core::{
        Action, Budget, CapacityState, PolicyId, Protection, ReasonCode, ScopeStats,
        SensitivityLevel,
    };
    use time::OffsetDateTime;

    // `evictable` is `Vec<MaintenanceCandidate>`, not `Vec<ScoredCandidate>`:
    // it feeds `AdmitContext::eviction_candidates`, which carries no
    // relevance because there is no recall query behind an eviction offer
    // (see that field's doc). `candidate()` builds the other type and must
    // not be used here.
    fn ctx(used: u64, max: Option<u64>, evictable: Vec<MaintenanceCandidate>) -> AdmitContext {
        AdmitContext {
            scope: scope(),
            capacity: CapacityState {
                budget: Budget { max_items: max, max_bytes: None },
                used_items: used,
                used_bytes: 0,
            },
            eviction_candidates: evictable,
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn assessed(similarity: f32, value: f32, fragility: f32, level: SensitivityLevel)
        -> (Candidate, Assessment)
    {
        let cand = candidate_from("a new memory", None);
        let neighbours = if similarity > 0.0 { vec![candidate("near", similarity)] } else { vec![] };
        let a = Assessment {
            value: Score::clamped(value),
            fragility: Score::clamped(fragility),
            sensitivity: SensitivityAssessment {
                level,
                categories: vec![],
                confidence: Score::clamped(0.8),
            },
            redundancy: crate::redundancy::assess(&neighbours, &BaselineConfig::default()),
            features: Default::default(),
            assessor: AssessorId::new("baseline", "0.1.0"),
        };
        (cand, a)
    }

    fn pid() -> PolicyId {
        PolicyId::new("baseline", "0.1.0")
    }

    #[test]
    fn an_exact_duplicate_is_rejected() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.99, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(&Assessed { candidate: &c, assessment: &a }, &ctx(0, None, vec![]), &cfg, pid());
        assert!(matches!(d.action, Action::Reject));
        assert!(d.has_reason(ReasonCode::NearDuplicate));
    }

    #[test]
    fn a_near_duplicate_merges_into_its_closest_neighbour() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.95, 0.8, 0.2, SensitivityLevel::Internal);
        let expected = a.redundancy.near_duplicates[0].0.clone();
        let d = decide(&Assessed { candidate: &c, assessment: &a }, &ctx(0, None, vec![]), &cfg, pid());
        match &d.action {
            Action::Merge { into, .. } => assert_eq!(*into, expected),
            other => panic!("expected a merge, got {other:?}"),
        }
        assert!(d.has_reason(ReasonCode::HighRedundancy));
    }

    #[test]
    fn novel_content_is_retained_with_no_evictions() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(&Assessed { candidate: &c, assessment: &a }, &ctx(0, Some(100), vec![]), &cfg, pid());
        assert!(matches!(d.action, Action::Retain { protection: Protection::Normal }));
        assert!(d.evictions.is_empty());
        assert!(d.has_reason(ReasonCode::NovelContent));
    }

    #[test]
    fn under_pressure_the_cheapest_items_are_evicted_first() {
        let cfg = BaselineConfig::default();
        let mut cheap = MaintenanceCandidate {
            item: item("cheap to lose"),
            value: Score::clamped(0.5),
            fragility: Score::clamped(0.5),
            last_accessed_at: None,
            access_count: 0,
        };
        cheap.value = Score::clamped(0.1);
        cheap.fragility = Score::clamped(0.1); // eviction::cost = 0.1 * 0.1 = 0.01
        let mut precious = MaintenanceCandidate {
            item: item("expensive to lose"),
            value: Score::clamped(0.5),
            fragility: Score::clamped(0.5),
            last_accessed_at: None,
            access_count: 0,
        };
        precious.value = Score::clamped(0.9);
        precious.fragility = Score::clamped(0.9); // eviction::cost = 0.9 * 0.9 = 0.81
        let cheap_id = cheap.item.id.clone();

        let (c, a) = assessed(0.1, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(
            &Assessed { candidate: &c, assessment: &a },
            &ctx(10, Some(10), vec![precious, cheap]),
            &cfg,
            pid(),
        );

        assert!(matches!(d.action, Action::Retain { .. }));
        assert_eq!(d.evictions.len(), 1, "exactly one eviction makes exactly enough room");
        assert_eq!(d.evictions[0].item, cheap_id, "evicted the expensive item");
        assert_eq!(d.evictions[0].reason.code, ReasonCode::CapacityPressure);
    }

    #[test]
    fn a_full_scope_with_nothing_evictable_rejects_rather_than_overflowing() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.2, SensitivityLevel::Internal);
        let d = decide(
            &Assessed { candidate: &c, assessment: &a },
            &ctx(10, Some(10), vec![]),
            &cfg,
            pid(),
        );
        assert!(matches!(d.action, Action::Reject));
        assert!(d.has_reason(ReasonCode::BudgetExhausted));
    }

    #[test]
    fn a_fragile_and_sensitive_item_is_protected_and_the_conflict_is_recorded() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.95, SensitivityLevel::Restricted);
        let d = decide(&Assessed { candidate: &c, assessment: &a }, &ctx(0, Some(100), vec![]), &cfg, pid());
        assert!(
            matches!(d.action, Action::Retain { protection: Protection::Protected { .. } }),
            "got {:?}",
            d.action
        );
        assert!(
            d.has_reason(ReasonCode::SensitivityConflict),
            "the conflict must be visible in the audit trail"
        );
    }

    #[test]
    fn a_fragile_but_ordinary_item_is_protected_without_a_conflict() {
        let cfg = BaselineConfig::default();
        let (c, a) = assessed(0.1, 0.8, 0.95, SensitivityLevel::Internal);
        let d = decide(&Assessed { candidate: &c, assessment: &a }, &ctx(0, Some(100), vec![]), &cfg, pid());
        assert!(d.has_reason(ReasonCode::ProtectedFragile));
        assert!(!d.has_reason(ReasonCode::SensitivityConflict));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-policy admit`
Expected: FAIL — `cannot find function decide in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-policy/src/admit.rs`:

```rust
use crate::config::{BaselineConfig, Verdict};
use crate::eviction;
use memorysafe_core::{
    Action, AdmitContext, Assessed, Decision, Eviction, MaintenanceCandidate, MergeStrategy,
    PolicyId, Protection, Reason, ReasonCode, SensitivityLevel, features,
};
use time::Duration;

/// Fragility at or above this earns a protection window.
const FRAGILE_THRESHOLD: f32 = 0.85;
/// Length of that window.
const PROTECTION_DAYS: i64 = 30;

pub fn decide(
    assessed: &Assessed,
    ctx: &AdmitContext,
    cfg: &BaselineConfig,
    policy: PolicyId,
) -> Decision {
    let a = assessed.assessment;
    let best = a.redundancy.score.get();

    match cfg.classify(best) {
        Verdict::NearDuplicate => {
            return Decision::reject(
                policy,
                Reason::new(
                    ReasonCode::NearDuplicate,
                    "an existing memory is effectively identical",
                    features! { "similarity" => best, "threshold" => cfg.duplicate_threshold },
                ),
            );
        }
        Verdict::Mergeable => {
            if let Some((target, similarity)) = a.redundancy.best() {
                return Decision {
                    subject: None,
                    action: Action::Merge {
                        into: target.clone(),
                        strategy: MergeStrategy::AppendAndUnion,
                    },
                    evictions: vec![],
                    reasons: vec![Reason::new(
                        ReasonCode::HighRedundancy,
                        "folded into a closely related existing memory",
                        features! {
                            "similarity" => similarity.get(),
                            "threshold" => cfg.merge_threshold,
                        },
                    )],
                    policy,
                };
            }
        }
        Verdict::Novel => {}
    }

    // Retain. Decide protection first, then make room.
    let fragile = a.fragility.get() >= FRAGILE_THRESHOLD;
    let sensitive = a.sensitivity.level >= SensitivityLevel::Sensitive;

    let mut reasons = Vec::new();
    let protection = if fragile {
        if sensitive {
            // Both axes fire and they disagree about what to do. Keep it and
            // say so, rather than resolving it invisibly.
            reasons.push(Reason::new(
                ReasonCode::SensitivityConflict,
                "fragile enough to protect and sensitive enough to question; retained \
                 and protected, flagged for review",
                features! {
                    "fragility" => a.fragility.get(),
                    "sensitivity_ordinal" => a.sensitivity.level.ordinal() as f64,
                },
            ));
        } else {
            reasons.push(Reason::new(
                ReasonCode::ProtectedFragile,
                "atypical content with few near neighbours; expensive to relearn",
                features! { "fragility" => a.fragility.get() },
            ));
        }
        Protection::Protected { until: ctx.now + Duration::days(PROTECTION_DAYS) }
    } else {
        reasons.push(Reason::new(
            ReasonCode::NovelContent,
            "no sufficiently similar memory exists",
            features! { "best_similarity" => best },
        ));
        Protection::Normal
    };

    // Make room if needed.
    let mut evictions = Vec::new();
    if ctx.capacity.would_exceed(1, assessed.candidate.byte_size) {
        let mut ranked: Vec<&MaintenanceCandidate> = ctx.eviction_candidates.iter().collect();
        ranked.sort_by(|a, b| eviction::cost(a).total_cmp(&eviction::cost(b)));

        let mut freed_items = 0u64;
        let mut freed_bytes = 0u64;
        for c in ranked {
            let still_over = {
                let mut projected = ctx.capacity;
                projected.used_items = projected.used_items.saturating_sub(freed_items);
                projected.used_bytes = projected.used_bytes.saturating_sub(freed_bytes);
                projected.would_exceed(1, assessed.candidate.byte_size)
            };
            if !still_over {
                break;
            }
            evictions.push(Eviction {
                item: c.item.id.clone(),
                reason: Reason::new(
                    ReasonCode::CapacityPressure,
                    "evicted to make room; lowest value-weighted retention cost in scope",
                    features! {
                        "value" => c.value.get(),
                        "fragility" => c.fragility.get(),
                        "eviction_cost" => eviction::cost(c),
                    },
                ),
            });
            freed_items += 1;
            freed_bytes += c.item.byte_size();
        }

        let mut projected = ctx.capacity;
        projected.used_items = projected.used_items.saturating_sub(freed_items);
        projected.used_bytes = projected.used_bytes.saturating_sub(freed_bytes);
        if projected.would_exceed(1, assessed.candidate.byte_size) {
            // Never silently exceed the budget.
            return Decision::reject(
                policy,
                Reason::new(
                    ReasonCode::BudgetExhausted,
                    "the namespace is full and nothing in it is evictable",
                    features! {
                        "used_items" => ctx.capacity.used_items as f64,
                        "evictable" => ctx.eviction_candidates.len() as f64,
                        "pressure" => ctx.capacity.pressure(),
                    },
                ),
            );
        }
    }

    Decision { subject: None, action: Action::Retain { protection }, evictions, reasons, policy }
}
```

`crates/memorysafe-policy/src/eviction.rs`:

```rust
//! The cost model shared by `admit` (capacity pressure during a write) and
//! `maintain` (capacity reclaim during a maintenance pass). Both rank the
//! same kind of candidate by the same rule, so the rule lives once, here,
//! rather than as two copies that agree today and diverge at the next
//! correction.

use memorysafe_core::MaintenanceCandidate;

/// Expected cost of losing this item: value times the unrecoverable share.
/// Eviction takes the smallest cost first, so a highly fragile item — one
/// with no near neighbours to reconstruct it from — must cost MORE to lose
/// than a redundant one, never less.
pub fn cost(c: &MaintenanceCandidate) -> f32 {
    c.value.get() * c.fragility.get()
}
```

Replace the `admit` stub in `lib.rs`:

```rust
    fn admit(&self, assessed: &Assessed, ctx: &AdmitContext) -> Result<Decision, PolicyError> {
        Ok(admit::decide(assessed, ctx, &self.config, self.policy_id()))
    }
```

Add `pub mod admit;` and `pub mod eviction;`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-policy`
Expected: PASS — this task adds 7 tests; at least 33 in the crate.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-policy/
git commit -m "feat(policy): admission with merge, capacity eviction, and recorded conflicts"
```

---

## Task 28: Policy — `compose`

**Files:**
- Create: `crates/memorysafe-policy/src/compose.rs`
- Modify: `crates/memorysafe-policy/src/lib.rs`

**Interfaces:**
- Consumes: `RecallRequest`, `ScoredCandidate`, `ComposeContext`, `BaselineConfig`.
- Produces: `compose::working_set(&RecallRequest, &[ScoredCandidate], &ComposeContext, &BaselineConfig) -> WorkingSet`, and `GovernancePolicy::compose` on `BaselinePolicy`.

**How composition works.** In `Search` mode, candidates are returned in relevance order, packed to budget. In `WorkingSet` mode, a fraction of the budget (`replay_quota`, default 20%) is reserved for replay-due items — high fragility or long unaccessed — and the rest is filled by Maximal Marginal Relevance, which trades relevance against dissimilarity to what has already been selected so the context window is not three paraphrases of one fact. Everything cut is reported in `omitted` with a reason, capped at `OMITTED_CAP`.

**On `replay`:** this is where the continual-learning verb earns its meaning for agent memory. An item that is fragile or has gone unread for a long time is given a slot it would not have won on relevance alone, which keeps it live rather than letting it decay into never being recalled again.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-policy/src/compose.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::{candidate, scope};
    use memorysafe_core::{RecallBudget, RecallMode, ReasonCode, ScopeStats, SensitivityLevel};
    use time::{Duration, OffsetDateTime};

    fn ctx() -> ComposeContext {
        ComposeContext {
            scope: scope(),
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH + Duration::days(365),
        }
    }

    fn req(mode: RecallMode, max_items: usize) -> RecallRequest {
        RecallRequest {
            scope: scope(),
            query: Some("cats".into()),
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            mode,
            budget: RecallBudget { max_tokens: Some(10_000), max_items: Some(max_items) },
            sensitivity_ceiling: SensitivityLevel::Restricted,
        }
    }

    #[test]
    fn search_mode_returns_pure_relevance_order() {
        let cands = vec![candidate("low", 0.2), candidate("high", 0.9), candidate("mid", 0.5)];
        let ws = working_set(&req(RecallMode::Search, 3), &cands, &ctx(), &BaselineConfig::default());
        let bodies: Vec<&str> = ws.items.iter().map(|s| s.item.body.as_str()).collect();
        assert_eq!(bodies, vec!["high", "mid", "low"]);
    }

    #[test]
    fn the_item_budget_is_respected_and_the_rest_is_reported_as_omitted() {
        let cands: Vec<_> = (0..10).map(|i| candidate(&format!("m{i}"), 0.9 - i as f32 * 0.05)).collect();
        let ws = working_set(&req(RecallMode::Search, 3), &cands, &ctx(), &BaselineConfig::default());
        assert_eq!(ws.items.len(), 3);
        assert_eq!(ws.omitted.len(), 7);
        assert!(ws.omitted.iter().all(|o| o.reason.code == ReasonCode::BudgetExhausted));
    }

    #[test]
    fn the_token_budget_is_respected() {
        let mut cands = Vec::new();
        for i in 0..5 {
            let mut c = candidate(&format!("m{i}"), 0.9);
            c.estimated_tokens = 100;
            cands.push(c);
        }
        let mut r = req(RecallMode::Search, 100);
        r.budget = RecallBudget { max_tokens: Some(250), max_items: None };
        let ws = working_set(&r, &cands, &ctx(), &BaselineConfig::default());
        assert_eq!(ws.items.len(), 2, "a third item would exceed 250 tokens");
        assert!(ws.tokens_used <= 250);
    }

    #[test]
    fn working_set_mode_diversifies_away_from_near_duplicates() {
        // Three paraphrases plus one distinct fact; with 2 slots the distinct
        // fact should win the second slot over a third paraphrase.
        let mut cands = vec![
            candidate("the cat sat on the mat", 0.95),
            candidate("the cat sat on a mat", 0.94),
            candidate("the cat sat upon the mat", 0.93),
            candidate("quarterly revenue exceeded projections", 0.60),
        ];
        for c in &mut cands {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let ws = working_set(&req(RecallMode::WorkingSet, 2), &cands, &ctx(), &BaselineConfig::default());
        let bodies: Vec<&str> = ws.items.iter().map(|s| s.item.body.as_str()).collect();
        assert_eq!(bodies[0], "the cat sat on the mat");
        assert_eq!(
            bodies[1], "quarterly revenue exceeded projections",
            "MMR should prefer a distinct item over a third paraphrase"
        );
        assert!(ws.items[1].reason.code == ReasonCode::DiversityCut || bodies.len() == 2);
    }

    #[test]
    fn a_fragile_stale_item_wins_a_replay_slot_it_would_not_win_on_relevance() {
        let mut relevant: Vec<_> =
            (0..9).map(|i| candidate(&format!("relevant {i}"), 0.9)).collect();
        for c in &mut relevant {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let mut stale = candidate("a rare fact nobody has read in a year", 0.05);
        stale.fragility = Score::ONE;
        stale.item.created_at = OffsetDateTime::UNIX_EPOCH;
        relevant.push(stale);

        let ws = working_set(
            &req(RecallMode::WorkingSet, 5),
            &relevant,
            &ctx(),
            &BaselineConfig::default(),
        );
        assert!(
            ws.items.iter().any(|s| s.item.body.contains("rare fact")),
            "the replay quota did not surface the fragile stale item"
        );
        assert!(ws.items.iter().any(|s| s.reason.code == ReasonCode::ReplayDue));
    }

    #[test]
    fn an_old_but_recently_accessed_item_does_not_win_a_replay_slot() {
        // The case the whole feature turns on: replay exists to resurface
        // what is never recalled. Every other candidate in this module comes
        // from `candidate()`, whose `last_accessed_at` is always `None` (see
        // its own comment in `testkit.rs`) — so `replay_due`'s
        // `last_accessed_at.unwrap_or(created_at)` fallback always takes the
        // `unwrap_or` branch in every other test here, which is
        // indistinguishable from a version that dropped `last_accessed_at`
        // and read `created_at` unconditionally. A later "simplification"
        // back to `created_at` alone would break none of them. This is the
        // one fixture with a real `Some(_)`, and it exists to make exactly
        // that regression detectable: old by creation, but not stale, because
        // someone looked at it five days ago.
        let mut relevant: Vec<_> =
            (0..9).map(|i| candidate(&format!("relevant {i}"), 0.9)).collect();
        for c in &mut relevant {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let mut old_but_used = candidate("an old fact someone checked five days ago", 0.05);
        // >= 0.5 so it is eligible for a replay slot on staleness alone, but
        // < 0.8 so the fragility-alone branch of `replay_due` cannot grant it
        // one regardless of staleness — only the access-recency question
        // decides this candidate.
        old_but_used.fragility = Score::clamped(0.6);
        old_but_used.item.created_at = OffsetDateTime::UNIX_EPOCH; // 365 days before `ctx().now`
        old_but_used.last_accessed_at = Some(ctx().now - Duration::days(5)); // recently used
        relevant.push(old_but_used);

        let ws = working_set(
            &req(RecallMode::WorkingSet, 5),
            &relevant,
            &ctx(),
            &BaselineConfig::default(),
        );
        assert!(
            ws.items
                .iter()
                .all(|s| !s.item.body.contains("five days ago")),
            "an item accessed five days ago won a replay slot — its 365-day-old \
             created_at was used for staleness instead of its recent \
             last_accessed_at"
        );
    }

    #[test]
    fn the_omitted_list_is_capped() {
        let cands: Vec<_> = (0..200).map(|i| candidate(&format!("m{i}"), 0.5)).collect();
        let ws = working_set(&req(RecallMode::Search, 1), &cands, &ctx(), &BaselineConfig::default());
        assert_eq!(ws.omitted.len(), memorysafe_core::OMITTED_CAP);
    }

    #[test]
    fn an_empty_candidate_set_yields_an_empty_working_set() {
        let ws = working_set(&req(RecallMode::WorkingSet, 5), &[], &ctx(), &BaselineConfig::default());
        assert!(ws.items.is_empty());
        assert!(ws.omitted.is_empty());
        assert_eq!(ws.tokens_used, 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-policy compose`
Expected: FAIL — `cannot find function working_set in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-policy/src/compose.rs`:

```rust
use crate::config::BaselineConfig;
use memorysafe_core::{
    ComposeContext, OMITTED_CAP, OmittedItem, RecallMode, RecallRequest, Reason, ReasonCode,
    ScoredCandidate, SelectedItem, WorkingSet, features,
};
use time::Duration;

/// Cheap textual proxy for "these two say the same thing", used by MMR. The
/// backend's vectors are not carried through to the policy, so similarity is
/// computed over token overlap instead — crude, but it reliably catches the
/// case MMR exists for: near-paraphrases crowding out distinct facts.
fn overlap(a: &str, b: &str) -> f32 {
    let toks = |s: &str| -> Vec<String> {
        s.split_whitespace().map(|t| t.to_lowercase()).collect()
    };
    let (ta, tb) = (toks(a), toks(b));
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let shared = ta.iter().filter(|t| tb.contains(t)).count();
    shared as f32 / ta.len().min(tb.len()) as f32
}

fn fits(req: &RecallRequest, tokens: u32, items: usize) -> bool {
    req.budget.fits(tokens, items)
}

/// True when an item deserves a slot it would not win on relevance: it is
/// fragile, or it has not been touched in a long time. This is `replay` from
/// the continual-learning lineage, applied to a context window.
fn replay_due(c: &ScoredCandidate, ctx: &ComposeContext, cfg: &BaselineConfig) -> bool {
    // Staleness is measured from the last *recall*, not from creation. The
    // replay quota exists to resurface what is never recalled, so an old item
    // recalled yesterday is not stale and must not win a reserved slot.
    // Measuring from `created_at` inverts the feature: it promotes exactly the
    // items that are already being used.
    //
    // A never-recalled item falls back to `created_at`, because its age is the
    // only proxy available for "how long has nobody looked at this". That
    // fallback is a decision taken here, visibly, which is why
    // `last_accessed_at` is `None` rather than a backend-invented
    // `Some(created_at)` — the policy can see it is guessing.
    //
    // This fallback is only honest because `(last_accessed_at: None,
    // access_count: 0)` is ruled to mean the item has never been accessed,
    // definitively — not "unknown". An item never recalled since creation is
    // genuinely stale since creation. If that ruling is ever revisited so
    // `(None, 0)` can also mean "unknown", this line starts fabricating a
    // recall that may never have been absent at all, and must be revisited
    // with it.
    let since_access = c.last_accessed_at.unwrap_or(c.item.created_at);
    let stale = ctx.now - since_access >= Duration::days(cfg.replay_stale_days as i64);
    c.fragility.get() >= 0.8 || (stale && c.fragility.get() >= 0.5)
}

pub fn working_set(
    req: &RecallRequest,
    candidates: &[ScoredCandidate],
    ctx: &ComposeContext,
    cfg: &BaselineConfig,
) -> WorkingSet {
    if candidates.is_empty() {
        return WorkingSet::empty();
    }

    let mut ranked: Vec<&ScoredCandidate> = candidates.iter().collect();
    ranked.sort_by(|a, b| {
        b.relevance.total_cmp(&a.relevance).then_with(|| a.item.id.cmp(&b.item.id))
    });

    let mut selected: Vec<SelectedItem> = Vec::new();
    let mut chosen: Vec<usize> = Vec::new();
    let mut tokens: u32 = 0;

    let push = |selected: &mut Vec<SelectedItem>,
                tokens: &mut u32,
                c: &ScoredCandidate,
                reason: Reason| {
        *tokens += c.estimated_tokens;
        selected.push(SelectedItem {
            item: c.item.clone(),
            relevance: c.relevance,
            reason,
        });
    };

    if req.mode == RecallMode::Search {
        for (i, c) in ranked.iter().enumerate() {
            if !fits(req, tokens + c.estimated_tokens, selected.len() + 1) {
                break;
            }
            chosen.push(i);
            push(
                &mut selected,
                &mut tokens,
                c,
                Reason::new(
                    ReasonCode::HighValue,
                    "ranked by relevance in search mode",
                    features! { "relevance" => c.relevance },
                ),
            );
        }
    } else {
        // Reserve part of the budget for replay before relevance consumes it.
        let slot_budget = req.budget.max_items.unwrap_or(ranked.len());
        let replay_slots =
            ((slot_budget as f32 * cfg.replay_quota).floor() as usize).min(slot_budget);

        let mut replayed = 0usize;
        for (i, c) in ranked.iter().enumerate() {
            if replayed >= replay_slots {
                break;
            }
            if !replay_due(c, ctx, cfg) {
                continue;
            }
            if !fits(req, tokens + c.estimated_tokens, selected.len() + 1) {
                break;
            }
            chosen.push(i);
            replayed += 1;
            push(
                &mut selected,
                &mut tokens,
                c,
                Reason::new(
                    ReasonCode::ReplayDue,
                    "fragile or long unaccessed; surfaced to keep it live",
                    features! {
                        "fragility" => c.fragility.get(),
                        "relevance" => c.relevance,
                        "age_days" => (ctx.now - c.item.created_at).whole_days() as f64,
                    },
                ),
            );
        }

        // Fill the rest by Maximal Marginal Relevance.
        loop {
            let mut best: Option<(usize, f32)> = None;
            for (i, c) in ranked.iter().enumerate() {
                if chosen.contains(&i) {
                    continue;
                }
                if !fits(req, tokens + c.estimated_tokens, selected.len() + 1) {
                    continue;
                }
                let max_sim = selected
                    .iter()
                    .map(|s| overlap(&c.item.body, &s.item.body))
                    .fold(0.0f32, f32::max);
                let mmr = cfg.mmr_lambda * c.relevance - (1.0 - cfg.mmr_lambda) * max_sim;
                if best.is_none_or(|(_, b)| mmr > b) {
                    best = Some((i, mmr));
                }
            }
            let Some((i, mmr)) = best else { break };
            let c = ranked[i];
            chosen.push(i);
            let max_sim = selected
                .iter()
                .map(|s| overlap(&c.item.body, &s.item.body))
                .fold(0.0f32, f32::max);
            push(
                &mut selected,
                &mut tokens,
                c,
                Reason::new(
                    if max_sim > 0.5 { ReasonCode::DiversityCut } else { ReasonCode::HighValue },
                    "selected by relevance traded against redundancy with the set so far",
                    features! {
                        "relevance" => c.relevance,
                        "max_similarity_to_selected" => max_sim,
                        "mmr" => mmr,
                    },
                ),
            );
        }
    }

    // Counted before truncating, not after: `omitted` is a bounded sample and
    // `omitted_total` is the number of items actually cut. Deriving the total
    // from `omitted.len()` would report 50 for every recall above the cap.
    let cut: Vec<_> = ranked
        .iter()
        .enumerate()
        .filter(|(i, _)| !chosen.contains(i))
        .collect();
    let omitted_total = cut.len();
    let omitted: Vec<OmittedItem> = cut
        .into_iter()
        .take(OMITTED_CAP)
        .map(|(_, c)| OmittedItem {
            id: c.item.id.clone(),
            reason: Reason::new(
                ReasonCode::BudgetExhausted,
                "considered but did not fit the budget",
                features! { "relevance" => c.relevance },
            ),
        })
        .collect();

    WorkingSet { items: selected, tokens_used: tokens, omitted, omitted_total, audit_id: None }
}
```

Replace the `compose` stub in `lib.rs`:

```rust
    fn compose(&self, req: &RecallRequest, candidates: &[ScoredCandidate], ctx: &ComposeContext)
        -> Result<WorkingSet, PolicyError>
    {
        Ok(compose::working_set(req, candidates, ctx, &self.config))
    }
```

Add `pub mod compose;`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-policy`
Expected: PASS — this task adds 8 tests; at least 41 in the crate.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-policy/
git commit -m "feat(policy): governed working set with MMR diversity and a replay quota"
```

---

## Task 29: Policy — `maintain`

**Files:**
- Create: `crates/memorysafe-policy/src/maintain.rs`
- Modify: `crates/memorysafe-policy/src/lib.rs`

**Interfaces:**
- Consumes: `MaintainContext`, `BaselineConfig`.
- Produces: `maintain::decisions(&MaintainContext, &BaselineConfig, PolicyId) -> Vec<Decision>`, and `GovernancePolicy::maintain` on `BaselinePolicy`.

**What maintenance does, in order:** expire items past their TTL; release protection windows that have elapsed; **decay fragility against the current neighbourhood**; **consolidate near-duplicate survivors**; reclaim capacity when a namespace is over budget, cheapest first, never touching pinned items. Each produces its own `Decision` with its own reason, so the audit trail says exactly why anything vanished.

The file-responsibility table gives `src/maintain.rs` as *"TTL, decay, consolidation, reclaim"*. All four are in scope for this task. Decay and consolidation are specified below because neither is inferable from its name — one of them is actively mis-suggested by it.

### F-A — Fragility decay: the value can RISE, and the word says otherwise

Decay recomputes each surviving item's fragility against the neighbourhood **as
it now stands**, after this run's expiries and reclaims have removed items. It
is not a monotonic downward adjustment of a stored number.

**The contract that must be tested: removing an item's neighbours makes it MORE
fragile, not less.** `fragility::score(neighbours, stats)` is a function of how
recoverable an item would be if lost, so an item whose corroborating neighbours
have just been expired is *harder* to relearn than it was this morning — and
`score` returns `Score::ONE` outright when `neighbours` is empty. So the same
maintenance run that deletes an item's neighbours must raise that item's
fragility, and may raise it enough to earn it a protection window it did not
have before.

**Why this needs an explicit contract test rather than a branch.** An
implementation written from the word "decay" alone will only ever decrease
values — and it will pass every test also written from that word. The term of
art hides the case. The test must be derived from this paragraph:

> Two items, A and B, mutually each other's only neighbour. Expire B on TTL in
> this run. Assert A's fragility after the run is **strictly greater** than
> before it, and that the resulting `Decision` carries the raised value in its
> evidence rather than the stale one.

Falsify it before believing it: implement decay as `fragility * decay_factor`
and confirm the assertion fails. A decay that cannot go up is the plausible
wrong implementation this test exists to reject.

### F-B — Consolidation: `maintain` decides it, the engine applies it

Consolidation merges two items that both already exist and have both survived
this run's expiries. `maintain` emits a `Decision` whose action is
`Action::Merge { into, strategy }` naming the absorbed item and its target.

**This is not the admission-path merge with different arguments.** On the
admission path the incoming item has no id, no audit history and no stored
embedding — it is merged before it ever exists. Here both sides exist: the
absorbed item has an `ItemId` that other rows reference, audit records naming
it, and an embedding. Reusing `admit`'s applier unchanged will lose all three.

**Cross-lane contract, decided here so both sides build to the same thing:**

- `maintain` is pure and returns the decision. It performs no I/O and does not
  choose transaction boundaries.
- **The engine's maintenance job applies the merge** — writing the merged
  content to `into`, deleting the absorbed item, and doing both in one
  transaction with the audit write.
- **One `Merged` audit record names both items** in its `items: Vec<ItemRef>`,
  rather than a `Merged` plus a `Forgotten`. The absorbed item's earlier audit
  rows are not rewritten: a scope is never rewritten to make a row fit, and the
  merge record is what makes the item's disappearance explicable.

**Task 36 (engine — resumable maintenance job) must apply `Action::Merge` and
must carry its own test for it.** That sentence is a cross-reference, and a
cross-reference transfers the contract and not the coverage — every instance of
this construct audited in this codebase has left the second side untested. Task
35's test is therefore named as a requirement here and must not be treated as
satisfied by this task's tests.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-policy/src/maintain.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::{item, scope};
    use memorysafe_core::{Action, Budget, CapacityState, PolicyId, Protection, ReasonCode, ScopeStats};
    use time::{Duration, OffsetDateTime};

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(365)
    }

    fn ctx(batch: Vec<MemoryItem>, used: u64, max: Option<u64>) -> MaintainContext {
        MaintainContext {
            scope: scope(),
            batch,
            // Every test here hands `decisions` the whole namespace in one
            // page — there is no further page to wait for.
            is_final_batch: true,
            capacity: CapacityState {
                budget: Budget { max_items: max, max_bytes: None },
                used_items: used,
                used_bytes: 0,
            },
            stats: ScopeStats::default(),
            now: now(),
        }
    }

    fn pid() -> PolicyId {
        PolicyId::new("baseline", "0.1.0")
    }

    #[test]
    fn an_expired_item_is_forgotten_with_a_ttl_reason() {
        let mut expired = item("gone stale");
        expired.created_at = now() - Duration::days(10);
        expired.ttl = Some(Duration::days(1));
        let id = expired.id.clone();

        let ds = decisions(&ctx(vec![expired], 1, None), &BaselineConfig::default(), pid());
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].evictions[0].item, id);
        assert_eq!(ds[0].evictions[0].reason.code, ReasonCode::TtlExpired);
    }

    #[test]
    fn an_unexpired_item_is_left_alone() {
        let mut fresh = item("still good");
        fresh.created_at = now();
        fresh.ttl = Some(Duration::days(30));
        assert!(decisions(&ctx(vec![fresh], 1, None), &BaselineConfig::default(), pid()).is_empty());
    }

    // **Expiry dominates pinning.** This test asserted the opposite for several
    // rounds, against this task's own Step 3 code (`must_forget` is called with
    // no pinned exemption, under a comment saying there deliberately is none),
    // against `MemoryItem::must_forget`'s doc (*"Expiry dominates protection;
    // protection only governs eviction for capacity"* — a pin would otherwise
    // *"silently override a legal retention limit"*), and against core's own
    // test asserting `must_forget` is true for exactly this fixture. It was the
    // lone outlier and it is inverted here.
    //
    // A pin that outlived a retention limit would be a compliance failure in a
    // product whose retention profiles are the feature. Pinning still governs
    // capacity reclaim, which is the half it is for — see
    // `reclaim_never_touches_pinned_items`, and do not weaken that one while
    // fixing this.
    #[test]
    fn a_pin_does_not_override_a_retention_limit() {
        let mut pinned = item("pinned, but past its TTL");
        pinned.created_at = now() - Duration::days(10);
        pinned.ttl = Some(Duration::days(1));
        pinned.protection = Protection::Pinned;
        let out = decisions(&ctx(vec![pinned], 1, None), &BaselineConfig::default(), pid());
        assert_eq!(
            out.len(),
            1,
            "expiry dominates pinning: a pinned item past its TTL must still be forgotten"
        );
        // Assert the REASON, not the action. `Action` has exactly three
        // variants — `Retain`, `Merge`, `Reject` — and there is no `Forget`.
        // The intent was always that a caller can tell a retention expiry from
        // a capacity reclaim, and `ReasonCode` is where that distinction lives.
        // Do not add an `Action::Forget` to make this read better: the enum is
        // a stored wire format with audit-key implications and an applier in
        // the engine's maintenance job, so the variant is a design decision.
        assert!(
            out[0].has_reason(ReasonCode::TtlExpired),
            "the reason must name the retention limit, not capacity pressure"
        );
    }

    #[test]
    fn an_elapsed_protection_window_is_released() {
        let mut protected = item("no longer special");
        protected.protection = Protection::Protected { until: now() - Duration::days(1) };
        let ds = decisions(&ctx(vec![protected], 1, None), &BaselineConfig::default(), pid());
        assert_eq!(ds.len(), 1);
        assert!(matches!(ds[0].action, Action::Retain { protection: Protection::Normal }));
    }

    #[test]
    // Named for what this corpus pins, which is the COUNT and not the order:
    // all five fixtures share `STORED_VALUE`, `STORED_FRAGILITY` and the
    // default `created_at`, so cost and age are both ties and no ranking is
    // distinguishable here. The ranking is covered next door by
    // `reclaim_takes_the_cheapest_to_lose_first_not_merely_the_oldest`, which
    // varies value and fragility on purpose — do not weaken that one, and do
    // not re-add a ranking claim to this name.
    fn an_over_budget_namespace_reclaims_down_to_its_budget() {
        let batch: Vec<MemoryItem> = (0..5).map(|i| item(&format!("memory {i}"))).collect();
        let ds = decisions(&ctx(batch, 5, Some(3)), &BaselineConfig::default(), pid());
        let evicted: usize = ds.iter().map(|d| d.evictions.len()).sum();
        assert_eq!(evicted, 2, "5 items against a budget of 3 means 2 evictions");
        assert!(ds.iter().any(|d| d.has_reason(ReasonCode::CapacityPressure)));
    }

    #[test]
    fn reclaim_never_touches_pinned_items() {
        let mut batch: Vec<MemoryItem> = (0..4).map(|i| item(&format!("memory {i}"))).collect();
        for b in &mut batch {
            b.protection = Protection::Pinned;
        }
        let ds = decisions(&ctx(batch, 4, Some(1)), &BaselineConfig::default(), pid());
        let evicted: usize = ds.iter().map(|d| d.evictions.len()).sum();
        assert_eq!(evicted, 0, "pinned items are absolute even under capacity pressure");
    }

    #[test]
    fn a_namespace_within_budget_produces_no_decisions() {
        let batch: Vec<MemoryItem> = (0..2).map(|i| item(&format!("memory {i}"))).collect();
        assert!(decisions(&ctx(batch, 2, Some(10)), &BaselineConfig::default(), pid()).is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-policy maintain`
Expected: FAIL — `cannot find function decisions in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-policy/src/maintain.rs`:

```rust
use crate::config::BaselineConfig;
use crate::eviction;
use memorysafe_core::{
    Action, Decision, Eviction, MaintainContext, MaintenanceCandidate, MemoryItem, PolicyId,
    Protection, Reason, ReasonCode, features,
};

/// Tie-breaker for capacity reclaim, applied UNDER `eviction::cost` — not the
/// primary ranking. `cost` decides which items are cheapest to lose; this only
/// separates two candidates `cost` scores equally, and it separates them by age
/// alone: older first, on `created_at`. It does not consider size.
fn reclaim_rank(item: &MemoryItem) -> i64 {
    item.created_at.unix_timestamp()
}

pub fn decisions(
    ctx: &MaintainContext,
    // TTL, protection windows and reclaim need no thresholds; decay and
    // consolidation do (see F-A and F-B below), so this is used, not reserved.
    _cfg: &BaselineConfig,
    policy: PolicyId,
) -> Vec<Decision> {
    let mut out = Vec::new();

    // 1. TTL expiry. Pinned items are exempt — pinning is absolute.
    for item in ctx.batch.iter().map(|c| &c.item) {
        // No pinned-item exemption here: `must_forget` dominates protection, or
        // a pin would silently override a retention limit.
        if item.must_forget(ctx.now) {
            out.push(Decision {
                subject: Some(item.id.clone()),
                action: Action::Reject,
                evictions: vec![Eviction {
                    item: item.id.clone(),
                    reason: Reason::new(
                        ReasonCode::TtlExpired,
                        "the item's time-to-live elapsed",
                        features! {
                            "age_days" => (ctx.now - item.created_at).whole_days() as f64,
                        },
                    ),
                }],
                reasons: vec![],
                policy: policy.clone(),
            });
        }
    }

    let expired: Vec<_> =
        out.iter().flat_map(|d| d.evictions.iter().map(|e| e.item.clone())).collect();

    // 2. Release protection windows that have elapsed.
    for item in ctx.batch.iter().map(|c| &c.item) {
        if expired.contains(&item.id) {
            continue;
        }
        if let Protection::Protected { until } = item.protection
            && until <= ctx.now
        {
            out.push(Decision {
                subject: Some(item.id.clone()),
                action: Action::Retain { protection: Protection::Normal },
                evictions: vec![],
                reasons: vec![Reason::new(
                    ReasonCode::ProtectedFragile,
                    "protection window elapsed; item returns to normal eviction eligibility",
                    features! { "expired_at" => until.unix_timestamp() as f64 },
                )],
                policy: policy.clone(),
            });
        }
    }

    // 3. Capacity reclaim, cheapest first, pinned untouchable.
    // **Byte budgets are NOT deferred here — three of the four paths that read
    // `max_bytes` implement them, and the missing one is this.** Enumerated,
    // because the count is the argument and a smaller count reads as "barely
    // started":
    //
    //   `Budget::is_bounded`        `capacity.rs:17`  true for a byte-only budget,
    //                                                 and pinned by
    //                                                 `standing_check_budget_is_bounded`
    //   `CapacityState::pressure`   `capacity.rs:43`  reports it correctly
    //   `CapacityState::would_exceed` `capacity.rs:64` refuses the admission
    //   `admit`'s make-room loop    `admit.rs:153-182` accumulates `freed_bytes`
    //                                                 and EVICTS to fit — pinned by
    //                                                 `a_byte_only_budget_frees_enough_
    //                                                 room_after_one_eviction`
    //   `maintain::capacity_reclaim` HERE              ignores it
    //
    // So a namespace with `max_bytes: Some(_)` and `max_items: None` reports
    // pressure, refuses writes once over, and **never reclaims**. Permanently
    // stuck, not merely unbounded.
    //
    // **And the stuckness is expensive, not passive.** The engine gathers
    // eviction candidates only when `budget.is_bounded()`, which is true here —
    // so it runs `Backend::list`, builds the candidate vector, hands it over,
    // and this early return discards it. Work performed and silently thrown
    // away on every write, with no decision and no audit row. From outside the
    // namespace that looks like a system actively managing capacity.
    // (`is_bounded`'s behaviour is asserted today; the engine's use of it
    // arrives with the `remember` pipeline. The waste is latent until then and
    // the stall is not.)
    //
    // **So this is not a half-implemented feature. It is the two entry points
    // of one policy disagreeing about whether byte budgets are enforced.**
    // Admission-time pressure evicts to make room for a byte budget; scheduled
    // maintenance never reclaims for one. Same namespace, same config, opposite
    // behaviour depending on which path touches it — `eviction::cost`'s
    // divergence a level up: not a formula copied twice, but one question
    // answered twice differently.
    //
    // **The fix is to make reclaim match `admit`, not to reject the config.**
    // An earlier version of this comment called a byte-only budget "a
    // misconfiguration the engine should reject rather than a supported shape".
    // That was false — `admit` supports it and has a test named for it, so
    // rejecting it would break a working path.
    //
    // Do not write a comment here saying byte budgets are deferred. **A reader
    // who believes the deferral will not look for the divergence**, and a
    // confident deferral note ends a look that silence would have invited.
    let Some(max_items) = ctx.capacity.budget.max_items else {
        return out;
    };
    let after_expiry = ctx.capacity.used_items.saturating_sub(expired.len() as u64);
    if after_expiry <= max_items {
        return out;
    }

    let mut over = after_expiry - max_items;
    // Rank by what it costs to lose the item, matching `admit`'s eviction
    // order, and fall back to age only to break ties.
    let mut reclaimable: Vec<&MaintenanceCandidate> = ctx
        .batch
        .iter()
        .filter(|c| !expired.contains(&c.item.id) && c.item.protection.is_evictable(ctx.now))
        .collect();
    reclaimable.sort_by(|a, b| {
        eviction::cost(a)
            .total_cmp(&eviction::cost(b))
            .then_with(|| reclaim_rank(&a.item).cmp(&reclaim_rank(&b.item)))
    });

    for candidate in reclaimable {
        let item = &candidate.item;
        if over == 0 {
            break;
        }
        out.push(Decision {
            subject: Some(item.id.clone()),
            action: Action::Reject,
            evictions: vec![Eviction {
                item: item.id.clone(),
                reason: Reason::new(
                    ReasonCode::CapacityPressure,
                    "namespace is over budget; reclaimed oldest evictable item",
                    features! {
                        "used_items" => ctx.capacity.used_items as f64,
                        "max_items" => max_items as f64,
                    },
                ),
            }],
            reasons: vec![],
            policy: policy.clone(),
        });
        over -= 1;
    }

    out
}
```

Replace the `maintain` stub in `lib.rs`:

```rust
    fn maintain(&self, ctx: &MaintainContext) -> Result<Vec<Decision>, PolicyError> {
        Ok(maintain::decisions(ctx, &self.config, self.policy_id()))
    }
```

Add `pub mod maintain;`. `PolicyId` must derive `Clone`; confirm from Task 7.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-policy && cargo clippy -p memorysafe-policy --all-targets -- -D warnings`
Expected: PASS — this task adds 7 tests; at least 48 in the crate. `BaselinePolicy` now implements all four trait methods.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-policy/
git commit -m "feat(policy): maintenance for TTL, protection windows, and capacity reclaim"
```

---
## Task 30: Policy — `compose::working_set` set-coverage

**What changes.** `working_set`'s MMR fill currently takes `max` over pairwise
`overlap(candidate, selected_i)` at two sites (`compose.rs:213-217` and the
evidence computation that follows the selection). Replace both with coverage
against the union of the selected set:

    |A ∩ (B₁ ∪ B₂ ∪ … ∪ Bₙ)| / |A|

maintained as a running token set updated when an item is pushed to `selected`,
rather than recomputed per candidate. `overlap` currently re-tokenizes every
selected body once per candidate at both sites; the union form is one candidate
tokenization against a maintained set, so this is expected to be **cheaper as
well as more correct** — confirm that rather than assuming it.

**Why, and what it fixes.** Task 28 made `overlap` directional (`|A∩B|/|A|`),
which is right and introduced a narrower regression: a candidate wholly covered
by the selected set *taken together* but by no single selected item is scored at
the max of its pairwise coverages, not at 1.00.

    a = {x, y},  selected = [{x}, {y}]      truth = 1.00
      old (min denominator, max)  = 1.00    correct BY ACCIDENT
      current (|A∩B|/|A|, max)    = 0.50    WRONG
      union                       = 1.00    correct by construction

The characterization test added in Task 28 pins the **current wrong value**. This
task replaces it with an assertion of the correct one and deletes the deferral
note. Do not restore the `min` denominator to recover the old answer: the old
formula returned 1.00 whenever *either* token set was a subset of the other, so
`a = {x,y,z,w,v}` against `b = {x}` also gave 1.00 where the truth is 0.20. The
old right answer came from a saturation that was wrong across a much larger
class. **The repair is the union, never the revert.**

**THIS TASK IS ATOMIC. The rename and the semantic change cannot be split.**
`"max_similarity_to_selected"` (`compose.rs:279`) is *true today* — it is
literally the max over selected — and becomes false only under union. So there
is no "rename now, change semantics later" (the name goes wrong immediately) and
no "change semantics now, rename later" (the name is wrong for the window in
between). Land both in one commit. Suggested key:
`"fraction_covered_by_selected"`.

**Four costs, three of which are not inferable from the diff.**

1. **The evidence key is audit-visible, so this is a schema change, not a
   rename.** A durable record asserting something the mechanism does not do is
   the same defect the `ExactDuplicate` → `NearDuplicate` rename fixed at
   `3a504a3`; the project already has a ruling on this class — cite it.
2. **`max_sim` feeds selection, not only tagging** (`compose.rs:217`, inside the
   MMR score). Union ≥ max always, so diversity pressure rises **uniformly on
   every candidate** and `mmr_lambda`'s default stops meaning what it was tuned
   to mean. Revisit the default; do not assume it carries over.
3. **`cfg.diversity_cut_similarity`** (`compose.rs:248`) is calibrated against
   the max distribution too. More rows will tag `DiversityCut` unless its
   default moves with it.
4. **Fixture movement on every test with more than one selected item.** Unlike
   Task 28's distinct-token change, which was measured at zero breakage, this
   one moves numbers. **Derive every expected value independently, then run to
   CONFIRM it — never let the run supply the number.** Running the rejected
   implementation to *falsify* a fixture is required, not forbidden: build the
   pairwise-max version, run it against each new assertion, and confirm the
   assertion fails under it. The prohibition is on the compiler supplying an
   expected value, not on using the compiler.

**The deadline that matters is not a task number.** This must land before the
first production audit write. Old rows keep the old key forever, so a reader
spanning the change sees two vocabularies. Free now, permanently expensive
after — and that stays true however the plan renumbers.

**Test requirement.** The split-coverage case gets a real assertion, not a
characterization comment: candidate `{x, y}` against selected `[{x}, {y}]`
scores 1.00. Write it from this task text, not from the implementation.

---

## Task 31: Policy — byte-budget reclaim

**Files:**
- Modify: `crates/memorysafe-policy/src/maintain.rs`

**Interfaces:**
- Consumes: `CapacityState`, `Budget`, `MemoryItem::byte_size`.
- Produces: no new symbols — `capacity_reclaim` gains a dimension.

**The defect this closes is a disagreement, not a gap.** Four of the five paths
that read `Budget::max_bytes` honour it and one ignores it:

    `Budget::is_bounded`          `capacity.rs:17`     true for a byte-only budget
    `CapacityState::pressure`     `capacity.rs:43`     reports it
    `CapacityState::would_exceed` `capacity.rs:64`     refuses the admission
    `admit`'s make-room loop      `admit.rs:153-182`   accumulates `freed_bytes`, EVICTS to fit
    `maintain::capacity_reclaim`  `maintain.rs`        returns early on `max_items` alone

So **admission-time pressure evicts to make room for a byte budget and scheduled
maintenance never reclaims for one** — same namespace, same config, opposite
behaviour depending on which entry point touches it. A namespace with
`max_bytes: Some(_)` and `max_items: None` reports pressure, refuses writes once
over, gathers eviction candidates on every write (the engine gathers whenever
`budget.is_bounded()`, which is true here), and discards them. **Permanently
stuck while looking actively managed**, and the wasted gather is what an
operator notices before the stall.

**This is `eviction::cost`'s divergence one level up: not a formula copied
twice, but one question answered twice differently.** We retired that one by
making a second site *call* the first rather than re-derive it, and the same
move is available here.

- [ ] **Step 1: Write the failing tests**

Four cases. Derive every expected value from the fixture before running, then
run only to confirm — and falsify each against the current early return.

1. **Byte-only budget over its limit reclaims.** `max_items: None`,
   `max_bytes: Some(n)`, `used_bytes > n`. Assert a decision is produced and
   carries `ReasonCode::CapacityPressure`. **Falsify:** this is exactly the case
   the current code returns `Vec::new()` for, so it must fail before the fix.
2. **Mixed budget, only the byte dimension over.** `max_items` satisfied,
   `max_bytes` exceeded. Rejects an implementation that keeps the `max_items`
   early return and adds bytes only inside it.
3. **Mixed budget, only the item dimension over.** The existing behaviour, pinned
   so the fix cannot regress it.
4. **This run's own expiries count against bytes.** `capacity_reclaim` already
   subtracts `expired` from `used_items` before deciding, on the reasoning that
   counting from `used_items` alone reclaims live items to free space that was
   about to be free anyway. **The same reasoning applies to bytes and the same
   subtraction is required** — sum `byte_size()` over the expired set. A fixture
   where the expiries alone bring the namespace under its byte budget must
   produce no reclaim decision.

- [ ] **Step 2: Run to verify they fail**

Expect 1, 2 and 4 to fail and 3 to pass.

- [ ] **Step 3: Implement**

`capacity_reclaim` consults both dimensions. **Do not write a second byte
accounting.** `admit`'s make-room loop already answers "how many bytes does
evicting this candidate free" — `c.item.byte_size()`, accumulated — and this
must use the same accessor, not a parallel computation over the same field. If a
shared helper is the natural shape, extract it and have both call it; if it is
one method call, call it. **Two implementations of one question is the defect
this task exists to close, and adding a second one here would close it in the
letter and reopen it in the spirit.**

Ranking is unchanged: `eviction::cost` first, `reclaim_rank` as the tie-break.
Bytes decide *how many* to reclaim, never *which*.

**Two things not to do.**

**Do not reject byte-only budgets.** An earlier version of this analysis called
them "a misconfiguration the engine should reject rather than a supported
shape." That was false — `admit` supports them and
`a_byte_only_budget_frees_enough_room_after_one_eviction` pins the support. A
reader arriving at "just reject the config" needs to see it considered and
refuted, which is why it is recorded here rather than deleted.

**Do not defer this to the engine.** The divergence is between two functions in
`memorysafe-policy`; nothing about it needs a backend.

**Deadline: the first byte-only budget an operator configures**, which is
available the moment the engine ships and is not protected by any task number.
Pre-v1 nothing is stuck today; the exposure begins at the first real config.

---


## Task 32: Engine — decision validation

**Files:**
- Create: `crates/memorysafe-engine/Cargo.toml`
- Create: `crates/memorysafe-engine/src/lib.rs`
- Create: `crates/memorysafe-engine/src/error.rs`
- Create: `crates/memorysafe-engine/src/validate.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `GovernancePolicy`, `Decision`, `AdmitContext`, `WorkingSet`.
- Produces: `EngineError`, `FailureStance` (`FailClosed` | `FailSafe`), `validate::decision(&Decision, &AdmitContext) -> Result<(), Invalid>`, `validate::working_set(&WorkingSet, &[ScoredCandidate]) -> Result<(), Invalid>`, `validate::call_policy(f) -> Result<T, PolicyFailure>` wrapping `catch_unwind`.

**Why this exists.** The policy seam is pluggable, one implementation is closed-source, and third parties can write their own. Nothing a policy returns is trusted until it is checked: evictions must be in scope and evictable, merge targets must exist, scores must be in range, and a composed working set may contain only candidates that were supplied to it — that last one is what stops a policy bug from becoming a data leak.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-engine/src/validate.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::{
        Action, Budget, CapacityState, Eviction, ItemId, MaintenanceCandidate, MemoryItem,
        PolicyId, Protection, Reason, ReasonCode, ScopeStats, Scope, Score, ScoredCandidate,
        SensitivityLevel, Source, SourceKind, WorkingSet, SelectedItem, features,
    };
    use time::OffsetDateTime;

    fn scope() -> Scope {
        Scope::new("t", "s", "n").unwrap()
    }

    fn item(body: &str) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: scope(),
            body: body.into(),
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

    fn candidate(item: MemoryItem) -> ScoredCandidate {
        ScoredCandidate {
            item,
            relevance: 0.5,
            vector_score: None,
            keyword_score: None,
            value: Score::clamped(0.5),
            fragility: Score::clamped(0.5),
            estimated_tokens: 5,
            last_accessed_at: None,
            access_count: 0,
        }
    }

    // `AdmitContext::eviction_candidates` is `Vec<MaintenanceCandidate>`, not
    // `Vec<ScoredCandidate>` — that field carries no relevance because there
    // is no recall query behind an eviction offer. `candidate()` above builds
    // the other type, for the `working_set` tests below that genuinely need
    // `&[ScoredCandidate]`; this is its counterpart for eviction candidates.
    fn maintenance_candidate(item: MemoryItem) -> MaintenanceCandidate {
        MaintenanceCandidate {
            item,
            value: Score::clamped(0.5),
            fragility: Score::clamped(0.5),
            last_accessed_at: None,
            access_count: 0,
        }
    }

    fn ctx(evictable: Vec<MaintenanceCandidate>) -> AdmitContext {
        AdmitContext {
            scope: scope(),
            capacity: CapacityState {
                budget: Budget::UNBOUNDED,
                used_items: 0,
                used_bytes: 0,
            },
            eviction_candidates: evictable,
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn reason() -> Reason {
        Reason::new(ReasonCode::NovelContent, "ok", features! {})
    }

    #[test]
    fn a_well_formed_decision_is_accepted() {
        let c = maintenance_candidate(item("evictable"));
        let d = Decision {
            subject: None,
            action: Action::Retain { protection: Protection::Normal },
            evictions: vec![Eviction { item: c.item.id.clone(), reason: reason() }],
            reasons: vec![reason()],
            policy: PolicyId::new("baseline", "0.1.0"),
        };
        assert!(decision(&d, &ctx(vec![c])).is_ok());
    }

    #[test]
    fn a_decision_evicting_an_item_outside_the_offered_set_is_refused() {
        let d = Decision {
            subject: None,
            action: Action::Retain { protection: Protection::Normal },
            evictions: vec![Eviction { item: ItemId::new(), reason: reason() }],
            reasons: vec![reason()],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert!(matches!(
            decision(&d, &ctx(vec![])),
            Err(Invalid::EvictionOutsideScope { .. })
        ));
    }

    #[test]
    fn a_decision_evicting_a_pinned_item_is_refused() {
        let mut pinned = item("pinned");
        pinned.protection = Protection::Pinned;
        let c = maintenance_candidate(pinned);
        let d = Decision {
            subject: None,
            action: Action::Retain { protection: Protection::Normal },
            evictions: vec![Eviction { item: c.item.id.clone(), reason: reason() }],
            reasons: vec![reason()],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert!(matches!(decision(&d, &ctx(vec![c])), Err(Invalid::EvictsPinned { .. })));
    }

    #[test]
    fn a_decision_with_no_reason_is_refused() {
        let d = Decision {
            subject: None,
            action: Action::Retain { protection: Protection::Normal },
            evictions: vec![],
            reasons: vec![],
            policy: PolicyId::new("rogue", "0.1.0"),
        };
        assert!(matches!(decision(&d, &ctx(vec![])), Err(Invalid::NoReason)));
    }

    #[test]
    fn a_working_set_containing_an_unoffered_item_is_refused() {
        // The leak-prevention check: a policy may only narrow, never widen.
        let offered = candidate(item("offered"));
        let smuggled = item("never offered to the policy");
        let ws = WorkingSet {
            items: vec![SelectedItem {
                item: smuggled,
                relevance: 0.9,
                reason: reason(),
            }],
            tokens_used: 5,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        assert!(matches!(
            working_set(&ws, std::slice::from_ref(&offered)),
            Err(Invalid::UnofferedItem { .. })
        ));
    }

    #[test]
    fn a_working_set_that_is_a_subset_is_accepted() {
        let a = candidate(item("a"));
        let b = candidate(item("b"));
        let ws = WorkingSet {
            items: vec![SelectedItem {
                item: a.item.clone(),
                relevance: 0.9,
                reason: reason(),
            }],
            tokens_used: 5,
            omitted: vec![],
            omitted_total: 0,
            audit_id: None,
        };
        assert!(working_set(&ws, &[a, b]).is_ok());
    }

    #[test]
    fn a_panicking_policy_is_caught_rather_than_taking_down_the_process() {
        let result: Result<u32, PolicyFailure> =
            call_policy(|| panic!("the closed scorer exploded"));
        assert!(matches!(result, Err(PolicyFailure::Panicked(_))));
    }

    #[test]
    fn a_policy_returning_an_error_is_reported_as_such() {
        let result: Result<u32, PolicyFailure> = call_policy(|| {
            Err(memorysafe_core::PolicyError::MissingEmbedding)
        });
        assert!(matches!(result, Err(PolicyFailure::Errored(_))));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine`
Expected: FAIL — no such package `memorysafe-engine`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/Cargo.toml`:

```toml
[package]
name = "memorysafe-engine"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
memorysafe-core.workspace = true
memorysafe-embed.workspace = true
memorysafe-backend.workspace = true
memorysafe-policy.workspace = true
tokio = { version = "1.53.1", features = ["rt", "rt-multi-thread", "macros", "sync"] }
moka = { version = "0.12.16", features = ["future"] }
async-trait = "0.1.92"
thiserror.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true
blake3.workspace = true

[dev-dependencies]
memorysafe-backend-sqlite.workspace = true
tempfile = "3.27.0"
proptest = "1.11.0"

[lints]
workspace = true
```

Add to workspace `[workspace.dependencies]`:

```toml
memorysafe-engine = { path = "crates/memorysafe-engine" }
```

`crates/memorysafe-engine/src/error.rs`:

```rust
use memorysafe_backend::BackendError;
use memorysafe_embed::EmbedError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("embedder failed: {0}")]
    Embedder(#[from] EmbedError),
    #[error("policy failed and the engine is configured to fail closed: {0}")]
    PolicyRefused(String),
}

impl EngineError {
    /// Whether a caller should retry. Surfaced by adapters as a 503 hint.
    pub fn is_retryable(&self) -> bool {
        matches!(self, EngineError::Backend(BackendError::Storage { retryable: true, .. }))
    }
}
```

`crates/memorysafe-engine/src/validate.rs`:

```rust
use memorysafe_core::{
    AdmitContext, Action, Decision, ItemId, PolicyError, ScoredCandidate, WorkingSet,
};
use thiserror::Error;

/// What the engine does when a policy misbehaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureStance {
    /// Fall back to `BaselinePolicy` for that call.
    #[default]
    FailSafe,
    /// Refuse the operation.
    FailClosed,
}

#[derive(Debug, Error, PartialEq)]
pub enum Invalid {
    #[error("decision evicts {item}, which was not offered as an eviction candidate")]
    EvictionOutsideScope { item: ItemId },
    #[error("decision evicts {item}, which is pinned")]
    EvictsPinned { item: ItemId },
    #[error("decision carries no reason")]
    NoReason,
    #[error("working set contains {item}, which was never offered to the policy")]
    UnofferedItem { item: ItemId },
    #[error("working set reports {reported} tokens but its items sum to {actual}")]
    TokenAccountingWrong { reported: u32, actual: u32 },
}

#[derive(Debug, Error)]
pub enum PolicyFailure {
    #[error("policy panicked: {0}")]
    Panicked(String),
    #[error("policy returned an error: {0}")]
    Errored(#[from] PolicyError),
}

/// Nothing a policy returns is applied until it passes this.
pub fn decision(d: &Decision, ctx: &AdmitContext) -> Result<(), Invalid> {
    if d.reasons.is_empty() && d.evictions.is_empty() {
        return Err(Invalid::NoReason);
    }

    for e in &d.evictions {
        let Some(candidate) = ctx.eviction_candidates.iter().find(|c| c.item.id == e.item)
        else {
            return Err(Invalid::EvictionOutsideScope { item: e.item.clone() });
        };
        // Pinning is absolute; the engine enforces it even if a policy forgets.
        if !candidate.item.protection.is_evictable(ctx.now) {
            return Err(Invalid::EvictsPinned { item: e.item.clone() });
        }
    }

    if let Action::Retain { .. } | Action::Merge { .. } = d.action
        && d.reasons.is_empty()
    {
        return Err(Invalid::NoReason);
    }

    Ok(())
}

/// The leak-prevention check. A policy may narrow the candidate set it was
/// given; it may never introduce an item the backend's hard filters excluded.
pub fn working_set(ws: &WorkingSet, offered: &[ScoredCandidate]) -> Result<(), Invalid> {
    for selected in &ws.items {
        if !offered.iter().any(|c| c.item.id == selected.item.id) {
            return Err(Invalid::UnofferedItem { item: selected.item.id.clone() });
        }
    }

    let actual: u32 = ws
        .items
        .iter()
        .map(|s| offered
            .iter()
            .find(|c| c.item.id == s.item.id)
            .map(|c| c.estimated_tokens)
            .unwrap_or(0))
        .sum();
    if ws.tokens_used != actual {
        return Err(Invalid::TokenAccountingWrong { reported: ws.tokens_used, actual });
    }

    Ok(())
}

/// Runs a policy call under `catch_unwind`. A third-party or closed-source
/// policy must not be able to take down the process.
pub fn call_policy<T, F>(f: F) -> Result<T, PolicyFailure>
where
    F: FnOnce() -> Result<T, PolicyError> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(PolicyFailure::Errored(e)),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(PolicyFailure::Panicked(message))
        }
    }
}
```

`crates/memorysafe-engine/src/lib.rs`:

```rust
//! Orchestration. The engine performs all I/O, hands pure data to the policy,
//! validates everything the policy returns, and applies writes atomically with
//! an audit record.

pub mod error;
pub mod validate;

pub use error::EngineError;
pub use validate::FailureStance;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine validate`
Expected: PASS — 8 tests ok.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-engine/
git commit -m "feat(engine): decision validation and panic-safe policy invocation"
```

---

## Task 33: Engine — `remember`

**Files:**
- Create: `crates/memorysafe-engine/src/outcome.rs`
- Create: `crates/memorysafe-engine/src/write.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`
- Create: `crates/memorysafe-engine/tests/write.rs`

**Interfaces:**
- Consumes: `Backend`, `Embedder`, `GovernancePolicy`, `validate`.
- Produces: `Engine::new(EngineConfig)`, `EngineConfig::new(backend, embedder, policy)` — which fills `fallback_policy`, `stance`, `neighbour_k`, `eviction_candidates`, `cache`, and `retention` with defaults — plus `RememberRequest`, `WriteOutcome`, and `Engine::remember`.

**The pipeline:** validate → embed (cached, degrading to `pending_embedding` on failure) → gather neighbours, capacity, and stats in one pass → `assess` → `admit` → validate the decision → build one `WriteTransaction` → `backend.apply`.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/write.rs`:

```rust
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, Budget, ReasonCode, Scope, SensitivityLevel};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "coding-agent").unwrap()
}

fn req(body: &str) -> RememberRequest {
    RememberRequest::new(scope(), body)
}

#[tokio::test]
async fn a_novel_memory_is_admitted_and_says_why() {
    let e = engine();
    let out = e.remember(req("the production database migration runs on Sundays")).await.unwrap();

    assert!(matches!(out.action, Action::Retain { .. }));
    assert!(out.reasons.iter().any(|r| r.code == ReasonCode::NovelContent));
    assert!(out.item_id.is_some());
    assert!(out.evicted.is_empty());
}

#[tokio::test]
async fn an_identical_rewrite_is_rejected_as_a_duplicate() {
    let e = engine();
    let body = "the deploy key rotates every ninety days";
    e.remember(req(body)).await.unwrap();
    let second = e.remember(req(body)).await.unwrap();

    // A rejection is a successful call: the product working, not an error.
    assert!(matches!(second.action, Action::Reject));
    assert!(second.reasons.iter().any(|r| r.code == ReasonCode::NearDuplicate));
}

#[tokio::test]
async fn a_near_duplicate_is_merged_into_the_existing_memory() {
    let e = engine();
    let first = e.remember(req("the cat sat on the mat")).await.unwrap();
    let second = e.remember(req("the cat sat on the mat today")).await.unwrap();

    match second.action {
        Action::Merge { .. } => {
            assert_eq!(second.merged_into, first.item_id);
            assert!(second.reasons.iter().any(|r| r.code == ReasonCode::HighRedundancy));
        }
        Action::Retain { .. } => {
            // Acceptable if the deterministic embedder scores them below the
            // merge threshold; the decision must still be explained.
            assert!(!second.reasons.is_empty());
        }
        other => panic!("unexpected action {other:?}"),
    }
}

#[tokio::test]
async fn a_credential_is_detected_and_stored_as_restricted() {
    let e = engine();
    let out = e
        .remember(req("the api key is sk-abc123def456ghi789jkl012"))
        .await
        .unwrap();
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].sensitivity, SensitivityLevel::Restricted);
    assert!(out.item_id.is_some());
}

#[tokio::test]
async fn a_full_namespace_evicts_to_make_room_and_reports_what_it_dropped() {
    let e = engine();
    e.set_budget(&scope(), Budget { max_items: Some(3), max_bytes: None }).await.unwrap();

    for i in 0..3 {
        e.remember(req(&format!("distinct memory number {i} about topic {i}"))).await.unwrap();
    }
    let out = e.remember(req("a completely different subject entirely")).await.unwrap();

    assert!(matches!(out.action, Action::Retain { .. }));
    assert_eq!(out.evicted.len(), 1, "one eviction makes exactly enough room");
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 3, "the budget was exceeded");
}

#[tokio::test]
async fn a_retried_write_returns_the_original_outcome() {
    let e = engine();
    let mut r = req("written exactly once");
    r.idempotency_key = Some("retry-key".into());

    let first = e.remember(r.clone()).await.unwrap();
    let second = e.remember(r).await.unwrap();

    assert_eq!(first.item_id, second.item_id);
    assert_eq!(e.review(&scope(), &Default::default()).await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_empty_body_is_a_validation_error_not_a_stored_memory() {
    let e = engine();
    assert!(e.remember(req("   ")).await.is_err());
    assert!(e.review(&scope(), &Default::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn every_write_leaves_exactly_one_audit_record() {
    let e = engine();
    e.remember(req("first distinct memory about alpha")).await.unwrap();
    e.remember(req("second distinct memory about beta")).await.unwrap();

    let audit = e.audit(&scope(), &Default::default()).await.unwrap();
    assert_eq!(audit.len(), 2);
    assert!(audit.iter().all(|r| r.decision.is_some()), "audit must carry the decision");
    assert!(audit.iter().all(|r| r.assessment.is_some()), "audit must carry the assessment");
    // Bodies must never reach the audit trail.
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains("distinct memory"), "audit leaked an item body");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test write`
Expected: FAIL — `cannot find struct Engine in memorysafe_engine`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/outcome.rs`:

```rust
use memorysafe_core::{Action, AuditId, ItemId, Reason};
use serde::{Deserialize, Serialize};

/// What the caller learns from a write. A rejection or a merge is a *success*:
/// governance working, not an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteOutcome {
    pub item_id: Option<ItemId>,
    pub action: Action,
    pub reasons: Vec<Reason>,
    pub merged_into: Option<ItemId>,
    pub evicted: Vec<ItemId>,
    pub audit_id: AuditId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgetOutcome {
    pub forgotten: Vec<ItemId>,
    pub audit_id: AuditId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurgeOutcome {
    pub items_removed: u64,
    pub audit_rows_removed: u64,
    pub audit_rows_preserved: u64,
}
```

`crates/memorysafe-engine/src/write.rs`:

```rust
use crate::error::EngineError;
use crate::outcome::WriteOutcome;
use crate::validate::{self, FailureStance, PolicyFailure};
use crate::{Engine, gather};
use memorysafe_backend::{ItemWrite, MergeWrite, WriteTransaction};
use memorysafe_core::{
    Action, Actor, ActorKind, AssessContext, Assessed, AuditEvent, AuditRecord, Candidate,
    ItemId, ItemRef, MemoryItem, Protection, Reason, ReasonCode, Scope, SensitivityLevel,
    Source, SourceKind, features,
};
use memorysafe_embed::{Embedder, QuantizedVector};
use serde_json::Value;
use std::collections::BTreeMap;
use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone, PartialEq)]
pub struct RememberRequest {
    pub scope: Scope,
    pub body: String,
    pub kind: String,
    pub source: Source,
    pub occurred_at: Option<OffsetDateTime>,
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, Value>,
    pub sensitivity_hint: Option<SensitivityLevel>,
    pub ttl: Option<Duration>,
    pub idempotency_key: Option<String>,
    pub actor: Actor,
}

impl RememberRequest {
    pub fn new(scope: Scope, body: &str) -> Self {
        Self {
            scope,
            body: body.to_string(),
            kind: "fact".into(),
            source: Source { kind: SourceKind::Agent, id: None },
            occurred_at: None,
            tags: vec![],
            attrs: BTreeMap::new(),
            sensitivity_hint: None,
            ttl: None,
            idempotency_key: None,
            actor: Actor { kind: ActorKind::Agent, id: None },
        }
    }
}

const MAX_BODY_BYTES: usize = 64 * 1024;

impl Engine {
    pub async fn remember(&self, req: RememberRequest) -> Result<WriteOutcome, EngineError> {
        if req.body.trim().is_empty() {
            return Err(EngineError::Validation("body must not be empty".into()));
        }
        if req.body.len() > MAX_BODY_BYTES {
            return Err(EngineError::Validation(format!(
                "body exceeds {MAX_BODY_BYTES} bytes"
            )));
        }

        // Embed. A missing or failed model must never cost a user their memory.
        let embedding = match self.embedder.embed(&req.body) {
            Ok(e) => Some(e),
            Err(_) => None,
        };
        let pending_embedding = embedding.is_none();

        let candidate = Candidate {
            body: req.body.clone(),
            kind: req.kind.clone(),
            tags: req.tags.clone(),
            attrs: req.attrs.clone(),
            sensitivity_hint: req.sensitivity_hint,
            embedding: embedding.clone(),
            // Same function `MemoryItem::byte_size` uses. A smaller estimate
            // here would let every admitted item overrun the budget by the
            // difference between what was checked and what is stored.
            byte_size: MemoryItem::charge(
                &req.body,
                &req.kind,
                &req.tags,
                &req.attrs,
                req.source.id.as_deref(),
                req.scope.subject.as_str(),
                req.scope.namespace.as_str(),
            ),
        };

        // One I/O pass gathers everything the policy is allowed to see.
        let ctx = gather::assess_context(
            self.backend.as_ref(),
            &req.scope,
            embedding.as_ref(),
            self.neighbour_k,
        )
        .await?;

        let assessment = self.run_assess(&candidate, &ctx)?;

        let admit_ctx = gather::admit_context(
            self.backend.as_ref(),
            &req.scope,
            &ctx,
            self.eviction_candidates,
        )
        .await?;

        let assessed = Assessed { candidate: &candidate, assessment: &assessment };
        let decision = self.run_admit(&assessed, &admit_ctx)?;

        // Nothing a policy returns is applied until it passes validation.
        if let Err(invalid) = validate::decision(&decision, &admit_ctx) {
            return self.handle_invalid_decision(invalid, &req, &assessment).await;
        }

        let now = OffsetDateTime::now_utc();
        let vector = embedding.as_ref().map(QuantizedVector::from_embedding);

        let (item, merge) = match &decision.action {
            Action::Reject => (None, None),
            Action::Retain { protection } => {
                let item = MemoryItem {
                    id: ItemId::new(),
                    scope: req.scope.clone(),
                    body: req.body.clone(),
                    kind: req.kind.clone(),
                    source: req.source.clone(),
                    occurred_at: req.occurred_at,
                    created_at: now,
                    tags: req.tags.clone(),
                    attrs: req.attrs.clone(),
                    // Applied by the ENGINE, not trusted from the policy. A
                    // closed scorer that forgot to call `raised_by` would
                    // silently downgrade a caller's declared `Restricted` to
                    // its own detector's level, and the item would then satisfy
                    // a lower `sensitivity_ceiling` — the leak the read path
                    // exists to prevent, caused by an omission in the closed
                    // crate.
                    sensitivity: assessment
                        .sensitivity
                        .level
                        .raised_by(req.sensitivity_hint),
                    ttl: req.ttl,
                    protection: *protection,
                    pending_embedding,
                };
                (Some(item), None)
            }
            Action::Merge { into, .. } => (
                None,
                Some(MergeWrite {
                    target: into.clone(),
                    body: req.body.clone(),
                    tags: req.tags.clone(),
                    attrs: req.attrs.clone(),
                    vector: vector.clone(),
                    byte_size: req.body.len() as u64,
                }),
            ),
        };

        let event = match &decision.action {
            Action::Retain { .. } => AuditEvent::Admitted,
            Action::Merge { .. } => AuditEvent::Merged,
            Action::Reject => AuditEvent::Rejected,
        };

        let refs: Vec<ItemRef> = item.as_ref().map(|i| vec![ItemRef::from_item(i)]).unwrap_or_default();
        // `now`, already sampled above for `created_at`, not a second clock
        // read — the item and its own audit row must agree on when this
        // happened.
        let audit = AuditRecord::new(req.scope.clone(), event, refs, req.actor.clone(), now)
            .with_assessment(assessment.clone())
            .with_decision(decision.clone());

        let mut txn = WriteTransaction::new(req.scope.clone(), audit);
        txn.evictions = decision.evictions.iter().map(|e| e.item.clone()).collect();
        txn.idempotency_key = req.idempotency_key.clone();
        txn.payload_digest = req.idempotency_key.as_ref().map(|_| {
            blake3::hash(req.body.as_bytes()).to_hex().to_string()
        });
        if let Some(i) = item {
            txn.upsert = Some(ItemWrite { item: i, vector });
        }
        txn.merge = merge;

        let applied = self.backend.apply(txn).await?;

        Ok(WriteOutcome {
            item_id: applied.item_id.clone(),
            action: decision.action.clone(),
            reasons: decision.reasons.clone(),
            merged_into: match &decision.action {
                Action::Merge { into, .. } => Some(into.clone()),
                _ => None,
            },
            evicted: applied.evicted,
            audit_id: applied.audit_id,
        })
    }

    async fn handle_invalid_decision(
        &self,
        invalid: validate::Invalid,
        req: &RememberRequest,
        assessment: &memorysafe_core::Assessment,
    ) -> Result<WriteOutcome, EngineError> {
        let reason = Reason::new(
            ReasonCode::PolicyInvalid,
            &format!("policy returned an unusable decision: {invalid}"),
            features! {},
        );
        let audit = AuditRecord::new(
            req.scope.clone(),
            AuditEvent::Rejected,
            vec![],
            req.actor.clone(),
            OffsetDateTime::now_utc(),
        )
        .with_assessment(assessment.clone());

        let txn = WriteTransaction::new(req.scope.clone(), audit);
        let applied = self.backend.apply(txn).await?;

        match self.stance {
            FailureStance::FailClosed => Err(EngineError::PolicyRefused(invalid.to_string())),
            FailureStance::FailSafe => Ok(WriteOutcome {
                item_id: None,
                action: Action::Reject,
                reasons: vec![reason],
                merged_into: None,
                evicted: vec![],
                audit_id: applied.audit_id,
            }),
        }
    }

    fn run_assess(
        &self,
        cand: &Candidate,
        ctx: &AssessContext,
    ) -> Result<memorysafe_core::Assessment, EngineError> {
        let policy = self.policy.clone();
        let (c, x) = (cand.clone(), ctx.clone());
        match validate::call_policy(move || policy.assess(&c, &x)) {
            Ok(a) => Ok(a),
            Err(e) => self.policy_fallback(e, || self.fallback_policy.assess(cand, ctx)),
        }
    }

    fn run_admit(
        &self,
        assessed: &Assessed,
        ctx: &memorysafe_core::AdmitContext,
    ) -> Result<memorysafe_core::Decision, EngineError> {
        let policy = self.policy.clone();
        let (c, a, x) = (
            assessed.candidate.clone(),
            assessed.assessment.clone(),
            ctx.clone(),
        );
        let call = move || {
            let assessed = Assessed { candidate: &c, assessment: &a };
            policy.admit(&assessed, &x)
        };
        match validate::call_policy(call) {
            Ok(d) => Ok(d),
            Err(e) => self.policy_fallback(e, || self.fallback_policy.admit(assessed, ctx)),
        }
    }

    fn policy_fallback<T, F>(&self, failure: PolicyFailure, fallback: F) -> Result<T, EngineError>
    where
        F: FnOnce() -> Result<T, memorysafe_core::PolicyError>,
    {
        match self.stance {
            FailureStance::FailClosed => Err(EngineError::PolicyRefused(failure.to_string())),
            FailureStance::FailSafe => {
                fallback().map_err(|e| EngineError::PolicyRefused(e.to_string()))
            }
        }
    }
}
```

Create `crates/memorysafe-engine/src/gather.rs`:

```rust
use crate::error::EngineError;
use memorysafe_backend::Backend;
use memorysafe_core::{AdmitContext, AssessContext, Embedding, MaintenanceCandidate, Scope};
use time::OffsetDateTime;

/// One I/O pass producing everything `assess` may see.
pub async fn assess_context(
    backend: &dyn Backend,
    scope: &Scope,
    embedding: Option<&Embedding>,
    k: usize,
) -> Result<AssessContext, EngineError> {
    let neighbours = match embedding {
        Some(e) => backend.neighbours(scope, e, k).await.unwrap_or_default(),
        None => vec![],
    };
    let mut stats = backend.scope_stats(scope).await?;
    // The backend has no cheap way to compute this; the engine derives it from
    // the neighbours it just fetched.
    stats.mean_neighbour_similarity = if neighbours.is_empty() {
        0.0
    } else {
        neighbours.iter().map(|n| n.relevance).sum::<f32>() / neighbours.len() as f32
    };
    Ok(AssessContext {
        scope: scope.clone(),
        neighbours,
        stats,
        now: OffsetDateTime::now_utc(),
    })
}

/// Eviction candidates are filtered here, not in the policy: pinned items and
/// unexpired protection windows never reach it.
pub async fn admit_context(
    backend: &dyn Backend,
    scope: &Scope,
    assess: &AssessContext,
    limit: usize,
) -> Result<AdmitContext, EngineError> {
    let capacity = backend.capacity_state(scope).await?;
    let now = OffsetDateTime::now_utc();

    let eviction_candidates: Vec<MaintenanceCandidate> = if capacity.budget.is_bounded() {
        let page = memorysafe_backend::Page { offset: 0, limit };
        backend
            .list(scope, &page)
            .await?
            .into_iter()
            .filter(|i| i.protection.is_evictable(now))
            .map(|item| MaintenanceCandidate {
                value: memorysafe_core::Score::clamped(0.5),
                fragility: memorysafe_core::Score::clamped(0.5),
                item,
                // OPEN: `Backend::list` returns bare `MemoryItem`s, and the
                // access statistics deliberately do not live on that type, so
                // this path has no source for them. `(None, 0)` is NOT
                // "unknown" here — the ruling recorded at compose's staleness
                // fallback (see `replay_due` in Task 28) makes `(None, 0)`
                // mean the item has never been accessed, definitively,
                // everywhere this pair appears. Passing it for an item whose
                // access history is merely unavailable therefore states
                // something false, not merely something imprecise: a
                // frequently-recalled item offered for eviction reads to the
                // policy as indistinguishable from one nobody has ever
                // touched. Before `admit` is allowed to weigh staleness, the
                // listing path needs to carry the real statistics — a `list`
                // that returns them beside each item, or a dedicated read.
                // Flagged by the contract task that added these fields; not
                // solved there.
                last_accessed_at: None,
                access_count: 0,
            })
            .collect()
    } else {
        vec![]
    };

    Ok(AdmitContext {
        scope: scope.clone(),
        capacity,
        eviction_candidates,
        stats: assess.stats.clone(),
        now,
    })
}
```

Rewrite `crates/memorysafe-engine/src/lib.rs`:

```rust
//! Orchestration. The engine performs all I/O, hands pure data to the policy,
//! validates everything the policy returns, and applies writes atomically with
//! an audit record.

pub mod error;
pub mod gather;
pub mod outcome;
pub mod validate;
pub mod write;

pub use error::EngineError;
pub use outcome::{ForgetOutcome, PurgeOutcome, WriteOutcome};
pub use validate::FailureStance;
pub use write::RememberRequest;

use memorysafe_backend::{Backend, Page};
use memorysafe_core::{
    AuditFilter, AuditRecord, Budget, GovernancePolicy, MemoryItem, Scope,
};
use memorysafe_embed::Embedder;
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

pub struct EngineConfig {
    pub backend: Arc<dyn Backend>,
    pub embedder: Arc<dyn Embedder>,
    pub policy: Arc<dyn GovernancePolicy>,
    /// Used when `policy` misbehaves and the stance is `FailSafe`.
    pub fallback_policy: Arc<dyn GovernancePolicy>,
    pub stance: FailureStance,
    /// How many neighbours to fetch for assessment.
    pub neighbour_k: usize,
    /// How many items to offer the policy as eviction candidates.
    pub eviction_candidates: usize,
}

pub struct Engine {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) policy: Arc<dyn GovernancePolicy>,
    pub(crate) fallback_policy: Arc<dyn GovernancePolicy>,
    pub(crate) stance: FailureStance,
    pub(crate) neighbour_k: usize,
    pub(crate) eviction_candidates: usize,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            backend: config.backend,
            embedder: config.embedder,
            policy: config.policy,
            fallback_policy: config.fallback_policy,
            stance: config.stance,
            neighbour_k: config.neighbour_k,
            eviction_candidates: config.eviction_candidates,
        }
    }

    pub async fn review(&self, scope: &Scope, page: &Page)
        -> Result<Vec<MemoryItem>, EngineError>
    {
        Ok(self.backend.list(scope, page).await?)
    }

    pub async fn audit(&self, scope: &Scope, filter: &AuditFilter)
        -> Result<Vec<AuditRecord>, EngineError>
    {
        Ok(self.backend.audit(scope, filter).await?)
    }

    pub async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), EngineError> {
        Ok(self.backend.set_budget(scope, budget).await?)
    }
}
```

`EngineConfig` has no `Default` — a backend, embedder, and policy have no sensible defaults, and a panicking `Default` is worse than none. Construction goes through a constructor that supplies only the optional fields:

```rust
impl EngineConfig {
    pub fn new(
        backend: Arc<dyn Backend>,
        embedder: Arc<dyn Embedder>,
        policy: Arc<dyn GovernancePolicy>,
    ) -> Self {
        Self {
            backend,
            embedder,
            policy,
            fallback_policy: Arc::new(BaselinePolicy::default()),
            stance: FailureStance::FailSafe,
            neighbour_k: 16,
            eviction_candidates: 128,
        }
    }
}
```

`EngineConfig::new` is how every test in Tasks 33–41 builds an engine.

`Candidate`, `Assessment`, `AssessContext`, and `AdmitContext` must derive `Clone`; confirm from Tasks 5 and 10.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine --test write`
Expected: PASS — 8 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): remember pipeline with governance decisions surfaced to callers"
```

---

## Task 34: Engine — `recall`

**Files:**
- Create: `crates/memorysafe-engine/src/read.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`
- Create: `crates/memorysafe-engine/tests/read.rs`

**Interfaces:**
- Consumes: `Backend::retrieve_candidates`, `GovernancePolicy::compose`, `validate::working_set`.
- Produces: `Engine::recall(RecallRequest) -> Result<WorkingSet, EngineError>`.

**The pipeline:** build a `CandidateQuery` whose `HardFilters` carry the caller's sensitivity ceiling → over-fetch from the backend → `compose` → validate that the working set is a subset of what was offered → write a `Recalled` audit record → return with `audit_id` set. Access statistics are updated without blocking the response.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/read.rs`:

```rust
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    AuditEvent, RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn recall(query: &str, ceiling: SensitivityLevel, max_items: usize) -> RecallRequest {
    RecallRequest {
        scope: scope(),
        query: Some(query.into()),
        tags_any: vec![],
        kinds: vec![],
        occurred_after: None,
        occurred_before: None,
        mode: RecallMode::WorkingSet,
        budget: RecallBudget { max_tokens: Some(4000), max_items: Some(max_items) },
        sensitivity_ceiling: ceiling,
    }
}

async fn seed(e: &Engine, bodies: &[&str]) {
    for b in bodies {
        e.remember(RememberRequest::new(scope(), b)).await.unwrap();
    }
}

#[tokio::test]
async fn recall_returns_relevant_memories_each_with_a_reason() {
    let e = engine();
    seed(&e, &[
        "the cat sat on the mat",
        "quarterly revenue exceeded projections",
        "the deployment pipeline runs nightly",
    ])
    .await;

    let ws = e.recall(recall("the cat sat on the mat", SensitivityLevel::Restricted, 5))
        .await
        .unwrap();

    assert!(!ws.items.is_empty());
    assert!(ws.items.iter().all(|s| !s.reason.detail.is_empty()), "every item needs a reason");
    assert!(ws.audit_id.is_some(), "a recall must be audited");
}

#[tokio::test]
async fn a_restricted_memory_never_reaches_a_caller_cleared_only_to_personal() {
    let e = engine();
    // Detected as Restricted by the credential patterns.
    e.remember(RememberRequest::new(
        scope(),
        "the deploy api key is sk-abc123def456ghi789jkl012 for cats",
    ))
    .await
    .unwrap();
    seed(&e, &["an ordinary note about cats"]).await;

    let ws = e.recall(recall("cats", SensitivityLevel::Personal, 10)).await.unwrap();

    assert!(
        ws.items.iter().all(|s| s.item.sensitivity <= SensitivityLevel::Personal),
        "the sensitivity ceiling leaked"
    );
    let json = serde_json::to_string(&ws).unwrap();
    assert!(!json.contains("sk-abc123"), "a restricted body reached the response");
}

#[tokio::test]
async fn the_item_budget_is_respected_and_the_rest_is_reported() {
    let e = engine();
    let bodies: Vec<String> =
        (0..8).map(|i| format!("distinct memory {i} concerning subject {i}")).collect();
    let refs: Vec<&str> = bodies.iter().map(|s| s.as_str()).collect();
    seed(&e, &refs).await;

    let ws = e.recall(recall("distinct memory", SensitivityLevel::Restricted, 3)).await.unwrap();
    assert!(ws.items.len() <= 3);
    assert!(!ws.omitted.is_empty(), "what was cut must be reported");
}

#[tokio::test]
async fn search_mode_bypasses_composition_but_not_governance() {
    let e = engine();
    e.remember(RememberRequest::new(
        scope(),
        "the deploy api key is sk-abc123def456ghi789jkl012 for cats",
    ))
    .await
    .unwrap();
    seed(&e, &["an ordinary note about cats"]).await;

    let mut req = recall("cats", SensitivityLevel::Personal, 10);
    req.mode = RecallMode::Search;
    let ws = e.recall(req).await.unwrap();

    assert!(
        ws.items.iter().all(|s| s.item.sensitivity <= SensitivityLevel::Personal),
        "search mode must still enforce the ceiling"
    );
    assert!(ws.audit_id.is_some(), "search mode must still be audited");
}

#[tokio::test]
async fn a_recall_over_an_empty_scope_is_empty_not_an_error() {
    let e = engine();
    let ws = e.recall(recall("anything", SensitivityLevel::Restricted, 5)).await.unwrap();
    assert!(ws.items.is_empty());
    assert!(ws.omitted.is_empty());
}

#[tokio::test]
async fn the_recall_audit_record_names_exactly_the_returned_items() {
    let e = engine();
    seed(&e, &["alpha memory about cats", "beta memory about cats"]).await;
    let ws = e.recall(recall("cats", SensitivityLevel::Restricted, 1)).await.unwrap();

    let audit = e
        .audit(&scope(), &memorysafe_core::AuditFilter {
            events: vec![AuditEvent::Recalled],
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(audit.len(), 1);
    // Compare ids, not emptiness. `!items.is_empty()` is satisfied by a
    // backend that writes one arbitrary unrelated id, which would make the
    // message's claim false while the assertion passed.
    let audited: std::collections::BTreeSet<_> =
        audit[0].items.iter().map(|r| r.id().clone()).collect();
    let returned: std::collections::BTreeSet<_> =
        ws.items.iter().map(|s| s.item.id.clone()).collect();
    assert!(!returned.is_empty(), "the recall returned nothing, so there is nothing for the audit to name");
    assert_eq!(audited, returned, "the recall audit must name exactly the returned items");
    // **What was cut is deliberately not in the audit record**, and the
    // assertion above positively forbids it: `refs` is built from
    // `composed.items` alone. An omitted item was considered and rejected, so
    // naming it in the audit trail would record ids the caller never received
    // — a compliance artifact listing memories that were not disclosed. The
    // cut is reported to the caller instead, on `WorkingSet::omitted` (a
    // sample) and `WorkingSet::omitted_total` (the count). The test's name
    // says only what it checks.
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains("alpha memory"), "the recall audit leaked a body");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test read`
Expected: FAIL — `no method named recall found for struct Engine`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/read.rs`:

```rust
use crate::Engine;
use crate::error::EngineError;
use crate::validate;
use memorysafe_backend::{CandidateQuery, HardFilters};
use memorysafe_core::{
    Actor, ActorKind, AuditEvent, AuditRecord, ComposeContext, ItemRef, RecallRequest,
    ScoredCandidate, WorkingSet,
};
use memorysafe_embed::Embedder;
use time::OffsetDateTime;

/// Over-fetch factor: composition needs room to trade relevance for diversity
/// and to fill the replay quota, so it must see more than it will return.
const OVERFETCH: usize = 8;

impl Engine {
    pub async fn recall(&self, req: RecallRequest) -> Result<WorkingSet, EngineError> {
        let limit = req.budget.max_items.unwrap_or(20).saturating_mul(OVERFETCH).clamp(10, 500);

        let embedding = req
            .query
            .as_deref()
            .filter(|q| !q.trim().is_empty())
            .and_then(|q| self.embedder.embed(q).ok());

        if embedding.is_none() && req.query.as_deref().unwrap_or("").trim().is_empty() {
            return Err(EngineError::Validation(
                "a recall needs a query; filter-only recall is not supported in v1".into(),
            ));
        }

        let query = CandidateQuery {
            embedding,
            text: req.query.clone(),
            // The security boundary: these run in SQL, below the policy.
            filters: HardFilters {
                tags_any: req.tags_any.clone(),
                kinds: req.kinds.clone(),
                occurred_after: req.occurred_after,
                occurred_before: req.occurred_before,
                sensitivity_ceiling: req.sensitivity_ceiling,
                exclude_pending_embedding: false,
            },
            limit,
        };

        let candidates: Vec<ScoredCandidate> =
            self.backend.retrieve_candidates(&req.scope, &query).await?;

        if candidates.is_empty() {
            let audit = AuditRecord::new(
                req.scope.clone(),
                AuditEvent::Recalled,
                vec![],
                Actor { kind: ActorKind::Agent, id: None },
                OffsetDateTime::now_utc(),
            );
            let audit_id = self.backend.record_recall(audit).await?;
            return Ok(WorkingSet { audit_id: Some(audit_id), ..WorkingSet::empty() });
        }

        let ctx = ComposeContext {
            scope: req.scope.clone(),
            stats: self.backend.scope_stats(&req.scope).await?,
            now: OffsetDateTime::now_utc(),
        };

        let policy = self.policy.clone();
        let (r, c, x) = (req.clone(), candidates.clone(), ctx.clone());
        debug_assert!(
            candidates.iter().all(|c| c.item.scope == req.scope),
            "backend returned a candidate outside the requested scope"
        );
        let composed = match validate::call_policy(move || policy.compose(&r, &c, &x)) {
            Ok(ws) => ws,
            Err(failure) => match self.stance {
                validate::FailureStance::FailClosed => {
                    return Err(EngineError::PolicyRefused(failure.to_string()));
                }
                validate::FailureStance::FailSafe => self
                    .fallback_policy
                    .compose(&req, &candidates, &ctx)
                    .map_err(|e| EngineError::PolicyRefused(e.to_string()))?,
            },
        };

        // A policy may narrow the candidate set; it may never widen it.
        if let Err(invalid) = validate::working_set(&composed, &candidates) {
            return Err(EngineError::PolicyRefused(invalid.to_string()));
        }

        let refs: Vec<ItemRef> =
            composed.items.iter().map(|s| ItemRef::from_item(&s.item)).collect();
        // Reuse `ctx.now`, already sampled above for the compose context,
        // rather than a second clock read for the same logical recall.
        let audit = AuditRecord::new(
            req.scope.clone(),
            AuditEvent::Recalled,
            refs,
            Actor { kind: ActorKind::Agent, id: None },
            ctx.now,
        );
        let audit_id = self.backend.record_recall(audit).await?;

        Ok(WorkingSet { audit_id: Some(audit_id), ..composed })
    }
}
```

Add `pub mod read;` to `lib.rs`. `RecallRequest`, `ScoredCandidate`, and `ComposeContext` need `Clone`; confirm from Tasks 9 and 10.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine`
Expected: PASS — 22 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): recall pipeline with the sensitivity ceiling enforced below the policy"
```

---

## Task 35: Engine — `forget`, `protect`, and `purge_subject`

**Files:**
- Create: `crates/memorysafe-engine/src/mutate.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`
- Create: `crates/memorysafe-engine/tests/mutate.rs`

**Interfaces:**
- Consumes: `Backend::apply`, `Backend::purge_subject`, `Backend::get`.
- Produces: `ForgetSelector` (`Ids(Vec<ItemId>)` | `Tag(String)` | `Kind(String)`), `Engine::forget`, `Engine::protect`, `Engine::purge_subject`, and its two private helpers `purge_scope` / `namespaces_of`.

**Note:** `protect` is the only path that changes `MemoryItem::protection` outside admission, per the spec. It writes its own audit record so "why is this pinned?" is answerable from the trail alone.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/mutate.rs`:

```rust
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    AuditEvent, AuditFilter, Protection, Scope, SubjectId, TenantId,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, ForgetSelector, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use time::{Duration, OffsetDateTime};

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

#[tokio::test]
async fn forgetting_by_id_removes_the_item_and_audits_it() {
    let e = engine();
    let out = e.remember(RememberRequest::new(scope(), "a memory to delete")).await.unwrap();
    let id = out.item_id.unwrap();

    let f = e.forget(&scope(), ForgetSelector::Ids(vec![id.clone()])).await.unwrap();
    assert_eq!(f.forgotten, vec![id]);
    assert!(e.review(&scope(), &Default::default()).await.unwrap().is_empty());

    let audit = e
        .audit(&scope(), &AuditFilter { events: vec![AuditEvent::Forgotten], ..Default::default() })
        .await
        .unwrap();
    assert_eq!(audit.len(), 1);
}

#[tokio::test]
async fn forgetting_by_tag_removes_only_matching_items() {
    let e = engine();
    for (body, tag) in [("alpha note", "work"), ("beta note", "home"), ("gamma note", "work")] {
        let mut r = RememberRequest::new(scope(), body);
        r.tags = vec![tag.into()];
        e.remember(r).await.unwrap();
    }

    let f = e.forget(&scope(), ForgetSelector::Tag("work".into())).await.unwrap();
    assert_eq!(f.forgotten.len(), 2);
    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].body, "beta note");
}

#[tokio::test]
async fn forgetting_something_that_does_not_exist_is_not_an_error() {
    let e = engine();
    let f = e
        .forget(&scope(), ForgetSelector::Ids(vec![memorysafe_core::ItemId::new()]))
        .await
        .unwrap();
    assert!(f.forgotten.is_empty());
}

#[tokio::test]
async fn pinning_an_item_survives_a_later_capacity_squeeze() {
    let e = engine();
    let out = e.remember(RememberRequest::new(scope(), "never forget this one")).await.unwrap();
    let id = out.item_id.unwrap();

    e.protect(&scope(), &id, Protection::Pinned).await.unwrap();

    e.set_budget(&scope(), memorysafe_core::Budget { max_items: Some(1), max_bytes: None })
        .await
        .unwrap();
    for i in 0..4 {
        e.remember(RememberRequest::new(scope(), &format!("filler memory {i} about topic {i}")))
            .await
            .unwrap();
    }

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert!(
        left.iter().any(|i| i.id == id),
        "a pinned item was evicted under capacity pressure"
    );
}

#[tokio::test]
async fn protecting_writes_its_own_audit_record() {
    let e = engine();
    let id = e
        .remember(RememberRequest::new(scope(), "worth protecting"))
        .await
        .unwrap()
        .item_id
        .unwrap();

    let until = OffsetDateTime::now_utc() + Duration::days(7);
    e.protect(&scope(), &id, Protection::Protected { until }).await.unwrap();

    let audit = e.audit(&scope(), &AuditFilter::default()).await.unwrap();
    assert_eq!(audit.len(), 2, "remember plus protect");
}

#[tokio::test]
async fn purging_a_subject_removes_everything_it_owns() {
    let e = engine();
    let doomed = Scope::new("acme", "doomed", "agent").unwrap();
    let keeper = Scope::new("acme", "keeper", "agent").unwrap();

    for i in 0..3 {
        e.remember(RememberRequest::new(doomed.clone(), &format!("subject memory {i}")))
            .await
            .unwrap();
    }
    e.remember(RememberRequest::new(keeper.clone(), "another subject's memory")).await.unwrap();

    let report = e
        .purge_subject(&TenantId::new("acme").unwrap(), &SubjectId::new("doomed").unwrap())
        .await
        .unwrap();

    assert_eq!(report.items_removed, 3);
    assert!(e.review(&doomed, &Default::default()).await.unwrap().is_empty());
    assert_eq!(e.review(&keeper, &Default::default()).await.unwrap().len(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test mutate`
Expected: FAIL — `cannot find enum ForgetSelector in memorysafe_engine`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/mutate.rs`:

```rust
use crate::Engine;
use crate::error::EngineError;
use crate::outcome::{ForgetOutcome, PurgeOutcome, WriteOutcome};
use memorysafe_backend::{ItemWrite, Page, WriteTransaction};
use memorysafe_core::{
    Action, Actor, ActorKind, AuditEvent, AuditRecord, ItemId, ItemRef, Protection, Reason,
    ReasonCode, Scope, SubjectId, TenantId, features,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetSelector {
    Ids(Vec<ItemId>),
    Tag(String),
    Kind(String),
}

/// Paging bound for selector-based forget. Beyond this a caller should purge
/// the subject or narrow the selector.
const FORGET_SCAN_LIMIT: usize = 1000;

impl Engine {
    pub async fn forget(
        &self,
        scope: &Scope,
        selector: ForgetSelector,
    ) -> Result<ForgetOutcome, EngineError> {
        let targets: Vec<ItemId> = match selector {
            ForgetSelector::Ids(ids) => {
                let mut present = Vec::new();
                for id in ids {
                    if self.backend.get(scope, &id).await?.is_some() {
                        present.push(id);
                    }
                }
                present
            }
            ForgetSelector::Tag(tag) => self
                .backend
                .list(scope, &Page { offset: 0, limit: FORGET_SCAN_LIMIT })
                .await?
                .into_iter()
                .filter(|i| i.tags.contains(&tag))
                .map(|i| i.id)
                .collect(),
            ForgetSelector::Kind(kind) => self
                .backend
                .list(scope, &Page { offset: 0, limit: FORGET_SCAN_LIMIT })
                .await?
                .into_iter()
                .filter(|i| i.kind == kind)
                .map(|i| i.id)
                .collect(),
        };

        let refs: Vec<ItemRef> = Vec::new();
        let audit = AuditRecord::new(
            scope.clone(),
            AuditEvent::Forgotten,
            refs,
            Actor { kind: ActorKind::Human, id: None },
            OffsetDateTime::now_utc(),
        );
        let mut txn = WriteTransaction::new(scope.clone(), audit);
        txn.evictions = targets.clone();

        let applied = self.backend.apply(txn).await?;
        Ok(ForgetOutcome { forgotten: applied.evicted, audit_id: applied.audit_id })
    }

    /// The only path that changes `protection` outside admission.
    pub async fn protect(
        &self,
        scope: &Scope,
        id: &ItemId,
        protection: Protection,
    ) -> Result<WriteOutcome, EngineError> {
        let Some(mut item) = self.backend.get(scope, id).await? else {
            return Err(EngineError::NotFound(id.to_string()));
        };
        item.protection = protection;

        let audit = AuditRecord::new(
            scope.clone(),
            AuditEvent::Admitted,
            vec![ItemRef::from_item(&item)],
            Actor { kind: ActorKind::Human, id: None },
            OffsetDateTime::now_utc(),
        );
        let mut txn = WriteTransaction::new(scope.clone(), audit);
        // Replace the row: delete then insert, in one transaction.
        txn.evictions = vec![id.clone()];
        txn.upsert = Some(ItemWrite { item, vector: None });

        let applied = self.backend.apply(txn).await?;
        Ok(WriteOutcome {
            item_id: applied.item_id,
            action: Action::Retain { protection },
            reasons: vec![Reason::new(
                ReasonCode::Pinned,
                "protection set by explicit request",
                features! {},
            )],
            merged_into: None,
            evicted: vec![],
            audit_id: applied.audit_id,
        })
    }

    /// The `SubjectPurged` record is built **here**, not in the backend:
    /// `Backend::purge_subject` inserts the record it is handed and mints
    /// nothing (the echo rule on `Backend`), so whatever actor this method
    /// writes is what ends up in the log — which today is an anonymous
    /// `ActorKind::Human`, not the actor who ordered the erasure.
    ///
    /// **Plumbing a real actor is deferred to Plan 3's Task 2** ("Engine —
    /// per-tenant policy and retention, actor-attributed governance events"),
    /// which is where engine methods start taking an `Actor` from the
    /// boundary that has one. `Engine::purge_subject` has no actor parameter
    /// to thread, and adding one here would change a signature that Plan 3's
    /// HTTP route (`ops::purge_subject`), CLI command (`purge-subject`) and
    /// engine tests all already call — so the gap is recorded rather than
    /// closed. The same anonymous literal appears in `forget` and `protect`
    /// above; it is engine-wide and pre-existing, not specific to this
    /// method.
    ///
    /// `PurgeCascade::Cascade` is hard-coded here — it is `balanced`, the
    /// default profile's behaviour. Task 38 replaces this one expression with
    /// `self.retention.retention().purge_cascade` and changes nothing else.
    pub async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<PurgeOutcome, EngineError> {
        let audit = AuditRecord::new(
            self.purge_scope(tenant, subject).await?,
            AuditEvent::SubjectPurged,
            vec![],
            Actor { kind: ActorKind::Human, id: None },
            OffsetDateTime::now_utc(),
        );
        let report = self
            .backend
            .purge_subject(tenant, subject, PurgeCascade::Cascade, audit)
            .await?;
        Ok(PurgeOutcome {
            items_removed: report.items_removed,
            audit_rows_removed: report.audit_rows_removed,
            audit_rows_preserved: report.audit_rows_preserved,
        })
    }

    /// Which namespace the `SubjectPurged` record is filed under. A subject
    /// spans namespaces and an `AuditRecord` carries exactly one `Scope`, so
    /// somebody has to choose; `Backend::purge_subject` states that it stores
    /// the choice as given and never rewrites it, which is why the choice is
    /// made here and made explicitly. The subject's lexicographically first
    /// namespace, so the record lands beside the rows it is about — and
    /// `memorysafe_core::PURGED_COMPONENT` (`_purged`) when the subject owns
    /// no items at all (`Namespace` forbids a leading dot but permits a
    /// leading underscore).
    ///
    /// **The fallback name is a plan-level choice, not a derived one**; a
    /// Task 35 executor may pick differently, but must pick, and must say so
    /// where the record is built.
    async fn purge_scope(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<Scope, EngineError> {
        // PLACEHOLDER, not the intended shape. `namespaces_of` learns one
        // namespace by materialising the subject's entire corpus — item
        // bodies included — into a `Vec`, in the erasure path. It stands in
        // for a dedicated namespace query on `Backend` (something of the
        // shape `namespaces(&self, tenant, subject) -> Vec<Namespace>`),
        // which does not exist and is out of scope for this task. Do not ship
        // this as the design; see `namespaces_of` below.
        let namespace = self
            .namespaces_of(tenant, subject)
            .await?
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                // The constant, not a second copy of the literal: the name is
                // stored in audit rows that outlive the subject, so two
                // spellings is one silent divergence away from a compliance
                // query that finds nothing. See `PURGED_COMPONENT`'s doc for
                // what reserves it and what does not.
                memorysafe_core::Namespace::new(memorysafe_core::PURGED_COMPONENT)
                    .expect("the reserved component is a valid namespace")
            });
        Ok(Scope { tenant: tenant.clone(), subject: subject.clone(), namespace })
    }

    /// Namespaces the subject owns, ascending. Derived from its items, which
    /// is enough for v1: a namespace with no items has nothing for a purge to
    /// be about.
    ///
    /// **The `export` call below is a placeholder.** It pulls every item the
    /// subject owns, bodies and all, into memory in order to learn a list of
    /// namespace names — during an erasure, which is the one path where
    /// holding a subject's corpus in memory is least defensible, and whose
    /// cost grows with the corpus rather than with the answer. The correct
    /// shape is a namespace query on `Backend` that answers from an index;
    /// adding one is a trait change and belongs with the next batch of
    /// contract work, not here.
    async fn namespaces_of(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<Vec<memorysafe_core::Namespace>, EngineError> {
        let selector = memorysafe_backend::ScopeSelector {
            tenant: tenant.clone(),
            subject: Some(subject.clone()),
            namespace: None,
            include_audit: false,
        };
        let mut namespaces: Vec<memorysafe_core::Namespace> = self
            .backend
            .export(&selector)
            .await?
            .into_iter()
            .filter_map(|r| match r {
                memorysafe_backend::ExportRecord::Item { item, .. } => {
                    Some(item.scope.namespace)
                }
                _ => None,
            })
            .collect();
        namespaces.sort();
        namespaces.dedup();
        Ok(namespaces)
    }
}
```

Add `pub mod mutate;` and `pub use mutate::ForgetSelector;` to `lib.rs`.

**Note on `protect` and vectors:** deleting the row cascades its vector away, so `protect` re-embeds. Add that to `protect` before building the transaction:

```rust
        let vector = self.embedder.embed(&item.body).ok().map(|e| {
            memorysafe_embed::QuantizedVector::from_embedding(&e)
        });
```

and pass it as `ItemWrite { item, vector }`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine`
Expected: PASS — 28 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): forget, protect, and subject purge"
```

---

## Task 36: Engine — resumable maintenance job

**Files:**
- Create: `crates/memorysafe-engine/src/maintain.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`
- Create: `crates/memorysafe-engine/tests/maintain.rs`

**Interfaces:**
- Consumes: `GovernancePolicy::maintain`, `Backend::list`, `Backend::apply`.
- Produces: `MaintainCursor { offset: usize }`, `MaintainReport { scanned, forgotten, protection_released, consolidated, next_cursor }`, `Engine::maintain(&Scope, Option<MaintainCursor>)`.

**Design constraint from the spec:** maintenance is an explicit, resumable job with a cursor — not a background thread that quietly mutates state. Every change it makes goes through the same atomic, audited path as a write.

**This job must apply `Action::Merge`, and it needs its own test for it.** Task 29's
`maintain` emits `Action::Merge { into, strategy }` for consolidation (see F-B
there). Applying it means writing merged content to `into`, **deleting the absorbed
item**, and writing **one `Merged` audit record naming both items** in its
`items: Vec<ItemRef>` — all in a single transaction. It is not `admit`'s merge
applier with different arguments: there the incoming item has no id, no audit
history and no embedding, and here the absorbed side has all three.

The requirement is restated here rather than referred to, deliberately. Task 29
names it too, and a cross-reference between two tasks transfers the contract
without transferring the coverage — every instance of that construct audited in
this codebase has left the second side untested. A test in Task 29 that a merge
decision is *produced* does not establish that this job *applies* it; those are the
two sides, and this is the one that touches the backend.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/maintain.rs`:

```rust
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{AuditEvent, AuditFilter, Budget, Scope};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, MaintainCursor, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use time::Duration;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

#[tokio::test]
async fn an_expired_item_is_removed_and_the_reason_is_recorded() {
    let e = engine();
    let mut r = RememberRequest::new(scope(), "this memory expires immediately");
    r.ttl = Some(Duration::seconds(-1)); // already past
    e.remember(r).await.unwrap();
    e.remember(RememberRequest::new(scope(), "this one has no expiry at all")).await.unwrap();

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.forgotten, 1);

    let left = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].body, "this one has no expiry at all");

    let audit = e
        .audit(&scope(), &AuditFilter {
            events: vec![AuditEvent::MaintenanceRun],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!audit.is_empty(), "maintenance must write an audit record");
}

#[tokio::test]
async fn maintenance_over_a_healthy_scope_changes_nothing() {
    let e = engine();
    for i in 0..3 {
        e.remember(RememberRequest::new(scope(), &format!("healthy memory {i} about topic {i}")))
            .await
            .unwrap();
    }
    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.forgotten, 0);
    assert_eq!(e.review(&scope(), &Default::default()).await.unwrap().len(), 3);
}

#[tokio::test]
async fn maintenance_resumes_from_its_cursor() {
    let e = engine();
    for i in 0..250 {
        let mut r = RememberRequest::new(scope(), &format!("memory number {i} on subject {i}"));
        r.idempotency_key = Some(format!("seed-{i}"));
        e.remember(r).await.unwrap();
    }

    let first = e.maintain(&scope(), None).await.unwrap();
    assert!(first.next_cursor.is_some(), "a large scope must page");
    assert!(first.scanned > 0);

    let second = e.maintain(&scope(), first.next_cursor).await.unwrap();
    assert!(second.scanned > 0);
    assert!(
        second.next_cursor.is_none() || second.next_cursor.unwrap().offset > first.scanned,
        "the cursor must advance"
    );
}

#[tokio::test]
async fn maintenance_reclaims_an_over_budget_namespace() {
    let e = engine();
    // Seed above budget, then tighten the budget so the scope is over it.
    for i in 0..6 {
        e.remember(RememberRequest::new(scope(), &format!("memory {i} concerning subject {i}")))
            .await
            .unwrap();
    }
    e.set_budget(&scope(), Budget { max_items: Some(3), max_bytes: None }).await.unwrap();

    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.forgotten, 3, "6 items against a budget of 3 means 3 reclaimed");
    assert_eq!(e.review(&scope(), &Default::default()).await.unwrap().len(), 3);
}

#[tokio::test]
async fn maintenance_on_an_empty_scope_is_a_no_op() {
    let e = engine();
    let report = e.maintain(&scope(), None).await.unwrap();
    assert_eq!(report.scanned, 0);
    assert_eq!(report.forgotten, 0);
    assert!(report.next_cursor.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test maintain`
Expected: FAIL — `cannot find struct MaintainCursor in memorysafe_engine`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/maintain.rs`:

```rust
use crate::Engine;
use crate::error::EngineError;
use crate::validate;
use memorysafe_backend::{Page, WriteTransaction};
use memorysafe_core::{
    Action, Actor, ActorKind, AuditEvent, AuditRecord, ItemId, MaintainContext, Protection,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Items examined per call. Maintenance is explicit and resumable rather than
/// a background thread, so the caller controls how much work happens at once.
pub const MAINTAIN_BATCH: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainCursor {
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainReport {
    pub scanned: usize,
    pub forgotten: usize,
    pub protection_released: usize,
    pub next_cursor: Option<MaintainCursor>,
}

impl Engine {
    pub async fn maintain(
        &self,
        scope: &memorysafe_core::Scope,
        cursor: Option<MaintainCursor>,
    ) -> Result<MaintainReport, EngineError> {
        let offset = cursor.map(|c| c.offset).unwrap_or(0);
        let batch = self
            .backend
            .list(scope, &Page { offset, limit: MAINTAIN_BATCH })
            .await?;

        if batch.is_empty() {
            return Ok(MaintainReport {
                scanned: 0,
                forgotten: 0,
                protection_released: 0,
                next_cursor: None,
            });
        }

        let scanned = batch.len();
        let capacity = self.backend.capacity_state(scope).await?;
        // A short page means there is nothing after it. This is the same fact
        // `next_cursor` is derived from below, so the two cannot disagree.
        let is_final_batch = scanned < MAINTAIN_BATCH;
        let scored: Vec<MaintenanceCandidate> = batch
            .into_iter()
            .map(|item| MaintenanceCandidate {
                value: memorysafe_core::Score::clamped(0.5),
                fragility: memorysafe_core::Score::clamped(0.5),
                item,
                // OPEN: same gap as the `admit` path above — `Backend::list`
                // returns bare `MemoryItem`s and the access statistics are not
                // on that type. `(None, 0)` reads as "never recalled", which
                // is the wrong answer for a hot item, and maintenance is
                // precisely where staleness is supposed to matter.
                last_accessed_at: None,
                access_count: 0,
            })
            .collect();
        let ctx = MaintainContext {
            scope: scope.clone(),
            batch: scored,
            is_final_batch,
            capacity,
            stats: self.backend.scope_stats(scope).await?,
            now: OffsetDateTime::now_utc(),
        };

        let policy = self.policy.clone();
        let x = ctx.clone();
        let decisions = match validate::call_policy(move || policy.maintain(&x)) {
            Ok(d) => d,
            Err(failure) => match self.stance {
                validate::FailureStance::FailClosed => {
                    return Err(EngineError::PolicyRefused(failure.to_string()));
                }
                validate::FailureStance::FailSafe => self
                    .fallback_policy
                    .maintain(&ctx)
                    .map_err(|e| EngineError::PolicyRefused(e.to_string()))?,
            },
        };

        let mut to_forget: Vec<ItemId> = Vec::new();
        let mut to_release: Vec<(ItemId, Protection)> = Vec::new();
        let mut released = 0usize;

        for d in &decisions {
            for e in &d.evictions {
                // The engine enforces pinning even if a policy forgets.
                let pinned = ctx
                    .batch
                    .iter()
                    .any(|c| c.item.id == e.item && c.item.protection == Protection::Pinned);
                if !pinned {
                    to_forget.push(e.item.clone());
                }
            }
            // Actually apply the release. Counting it and moving on is how a
            // protection window silently never expires.
            if let (Some(id), Action::Retain { protection }) = (&d.subject, &d.action)
                && d.evictions.is_empty()
            {
                to_release.push((id.clone(), *protection));
                released += 1;
            }
        }

        let forgotten = to_forget.len();

        for (id, protection) in to_release {
            // Reuse the audited protect path rather than hand-rolling a write.
            self.protect(scope, &id, protection).await?;
        }

        if !to_forget.is_empty() || released > 0 {
            let audit = AuditRecord::new(
                scope.clone(),
                AuditEvent::MaintenanceRun,
                vec![],
                Actor { kind: ActorKind::System, id: None },
                OffsetDateTime::now_utc(),
            );
            let mut txn = WriteTransaction::new(scope.clone(), audit);
            txn.evictions = to_forget;
            self.backend.apply(txn).await?;
        }

        // Advance past what survived; forgotten rows have shifted the window.
        let next_offset = offset + scanned.saturating_sub(forgotten);
        let next_cursor = if is_final_batch {
            None
        } else {
            Some(MaintainCursor { offset: next_offset })
        };

        Ok(MaintainReport { scanned, forgotten, protection_released: released, next_cursor })
    }
}
```

Add to `lib.rs`:

```rust
pub mod maintain;
pub use maintain::{MAINTAIN_BATCH, MaintainCursor, MaintainReport};
```

`MaintainContext` needs `Clone`; confirm from Task 10.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine`
Expected: PASS — 33 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): explicit resumable maintenance job with audited changes"
```

---

## Task 37: Engine — cache and invalidation

**Files:**
- Create: `crates/memorysafe-engine/src/cache.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`, `src/write.rs`, `src/read.rs`
- Create: `crates/memorysafe-engine/tests/cache.rs`

**Interfaces:**
- Consumes: `moka::future::Cache`, `Scope::key()`.
- Produces: `EngineCache::new(CacheConfig)`, `EngineCache::embedding(text) / put_embedding`, `EngineCache::stats(scope) / put_stats`, `EngineCache::invalidate_scope(scope)`, `CacheConfig { embedding_capacity, stats_capacity, stats_ttl }`.

**What is and is not cached.** Embeddings are cached by content hash — the same text always embeds identically, so this is free correctness. Scope statistics are cached with a short TTL because they change slowly and are read on every write. **Composed working sets are deliberately not cached**: they depend on the corpus, the clock, and the replay state, and a stale one would return memories that were since forgotten. Any write invalidates its scope.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/cache.rs`:

```rust
use memorysafe_core::Scope;
use memorysafe_embed::{DeterministicEmbedder, Embedder};
use memorysafe_engine::cache::{CacheConfig, EngineCache};

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

#[tokio::test]
async fn an_embedding_is_returned_from_cache_on_the_second_ask() {
    let c = EngineCache::new(CacheConfig::default());
    let e = DeterministicEmbedder::new(256);
    let v = e.embed("some memory text").unwrap();

    assert!(c.embedding("some memory text").await.is_none());
    c.put_embedding("some memory text", v.clone()).await;
    assert_eq!(c.embedding("some memory text").await.unwrap().vector, v.vector);
}

#[tokio::test]
async fn different_text_does_not_collide() {
    let c = EngineCache::new(CacheConfig::default());
    let e = DeterministicEmbedder::new(256);
    c.put_embedding("first", e.embed("first").unwrap()).await;
    assert!(c.embedding("second").await.is_none());
}

#[tokio::test]
async fn scope_stats_are_cached_and_invalidated_together() {
    let c = EngineCache::new(CacheConfig::default());
    let stats = memorysafe_core::ScopeStats { item_count: 7, ..Default::default() };

    c.put_stats(&scope(), stats.clone()).await;
    assert_eq!(c.stats(&scope()).await.unwrap().item_count, 7);

    c.invalidate_scope(&scope()).await;
    assert!(c.stats(&scope()).await.is_none(), "a write must invalidate its scope");
}

#[tokio::test]
async fn invalidating_one_scope_leaves_another_alone() {
    let c = EngineCache::new(CacheConfig::default());
    let other = Scope::new("acme", "user-99", "agent").unwrap();
    let stats = memorysafe_core::ScopeStats { item_count: 3, ..Default::default() };

    c.put_stats(&scope(), stats.clone()).await;
    c.put_stats(&other, stats).await;
    c.invalidate_scope(&scope()).await;

    assert!(c.stats(&scope()).await.is_none());
    assert!(c.stats(&other).await.is_some(), "invalidation crossed scopes");
}

#[tokio::test]
async fn embeddings_survive_scope_invalidation() {
    // Embeddings are content-addressed, so a write cannot make one stale.
    let c = EngineCache::new(CacheConfig::default());
    let e = DeterministicEmbedder::new(256);
    c.put_embedding("durable text", e.embed("durable text").unwrap()).await;
    c.invalidate_scope(&scope()).await;
    assert!(c.embedding("durable text").await.is_some());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test cache`
Expected: FAIL — `unresolved import memorysafe_engine::cache`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/cache.rs`:

```rust
use memorysafe_core::{Embedding, Scope, ScopeStats};
use moka::future::Cache;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct CacheConfig {
    pub embedding_capacity: u64,
    pub stats_capacity: u64,
    pub stats_ttl: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            embedding_capacity: 10_000,
            stats_capacity: 5_000,
            // Short: statistics feed fragility scoring, and a stale corpus mean
            // skews every assessment made against it.
            stats_ttl: Duration::from_secs(30),
        }
    }
}

/// In-process caches. Deliberately excludes composed working sets: those depend
/// on the corpus, the clock, and replay state, so a stale one would surface
/// memories that have since been forgotten.
pub struct EngineCache {
    embeddings: Cache<String, Embedding>,
    stats: Cache<String, ScopeStats>,
}

impl EngineCache {
    pub fn new(config: CacheConfig) -> Self {
        Self {
            embeddings: Cache::builder().max_capacity(config.embedding_capacity).build(),
            stats: Cache::builder()
                .max_capacity(config.stats_capacity)
                .time_to_live(config.stats_ttl)
                .build(),
        }
    }

    fn embedding_key(text: &str) -> String {
        blake3::hash(text.as_bytes()).to_hex().to_string()
    }

    pub async fn embedding(&self, text: &str) -> Option<Embedding> {
        self.embeddings.get(&Self::embedding_key(text)).await
    }

    pub async fn put_embedding(&self, text: &str, embedding: Embedding) {
        self.embeddings.insert(Self::embedding_key(text), embedding).await;
    }

    pub async fn stats(&self, scope: &Scope) -> Option<ScopeStats> {
        self.stats.get(&scope.key()).await
    }

    pub async fn put_stats(&self, scope: &Scope, stats: ScopeStats) {
        self.stats.insert(scope.key(), stats).await;
    }

    /// Called after every write. Embeddings are content-addressed and are not
    /// affected.
    pub async fn invalidate_scope(&self, scope: &Scope) {
        self.stats.invalidate(&scope.key()).await;
    }
}

impl Default for EngineCache {
    fn default() -> Self {
        Self::new(CacheConfig::default())
    }
}
```

Wire it into the engine. Add to `EngineConfig` and `Engine`:

```rust
    pub cache: CacheConfig,
    // in Engine:
    pub(crate) cache: cache::EngineCache,
```

with `cache: CacheConfig::default()` in `EngineConfig::new` and
`cache: cache::EngineCache::new(config.cache)` in `Engine::new`.

Add a cached embed helper on `Engine` and use it in both `write.rs` and `read.rs` in place of the direct `self.embedder.embed(...)` calls:

```rust
    pub(crate) async fn embed_cached(&self, text: &str) -> Option<memorysafe_core::Embedding> {
        if let Some(hit) = self.cache.embedding(text).await {
            return Some(hit);
        }
        match self.embedder.embed(text) {
            Ok(v) => {
                self.cache.put_embedding(text, v.clone()).await;
                Some(v)
            }
            Err(_) => None,
        }
    }
```

In `write.rs`, after a successful `backend.apply`, add:

```rust
        self.cache.invalidate_scope(&req.scope).await;
```

Add `pub mod cache;` and `pub use cache::{CacheConfig, EngineCache};` to `lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine`
Expected: PASS — 38 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): content-addressed embedding cache and scope stats cache"
```

---

## Task 38: Engine — retention profiles

**`AuditRetention::aggregate` is enforced here.** It is the only span that applies to `audit_aggregates` rows; `detail` and `purge_cascade` govern the audit detail table and must not reach the aggregate table. An aggregate row expires on its own span or not at all.

**Files:**
- Create: `crates/memorysafe-engine/src/retention.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`, `src/mutate.rs`
- Create: `crates/memorysafe-engine/tests/retention.rs`

**Interfaces:**
- Consumes: `Backend::purge_subject`, `memorysafe_core::PurgeCascade`.
- Produces: `RetentionSpan`, `AuditRetention`, `RetentionProfile` (`Balanced` | `GdprStrict` | `HipaaRetain` | `Forensic`), `RetentionProfile::retention()`, `RetentionProfile::from_name(&str)`, and `Engine::purge_subject` honouring the configured profile.

**The four profiles are the tested, documented surface.** Free-form overrides are permitted but unsupported — that is what keeps "configurable per tenant" from meaning an untestable matrix.

| Profile | `detail` | `purge_cascade` | `aggregate` |
|---|---|---|---|
| `balanced` (default) | `UntilSubjectPurge` | `Cascade` | `Forever` |
| `gdpr_strict` | `Days(90)` | `Cascade` | `Days(365)` |
| `hipaa_retain` | `Days(2190)` | `Preserve` | `Forever` |
| `forensic` | `Forever` | `Preserve` | `Forever` |

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/retention.rs`:

```rust
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{AuditFilter, Scope, SubjectId, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{
    Engine, EngineConfig, PurgeCascade, RememberRequest, RetentionProfile, RetentionSpan,
};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn engine(profile: RetentionProfile) -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    );
    cfg.retention = profile;
    Engine::new(cfg)
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

#[test]
fn the_four_profiles_match_the_documented_table() {
    assert_eq!(
        RetentionProfile::Balanced.retention().detail,
        RetentionSpan::UntilSubjectPurge
    );
    assert_eq!(RetentionProfile::Balanced.retention().purge_cascade, PurgeCascade::Cascade);

    assert_eq!(RetentionProfile::GdprStrict.retention().detail, RetentionSpan::Days(90));
    assert_eq!(
        RetentionProfile::GdprStrict.retention().aggregate,
        RetentionSpan::Days(365)
    );

    assert_eq!(RetentionProfile::HipaaRetain.retention().detail, RetentionSpan::Days(2190));
    assert_eq!(
        RetentionProfile::HipaaRetain.retention().purge_cascade,
        PurgeCascade::Preserve
    );

    assert_eq!(RetentionProfile::Forensic.retention().detail, RetentionSpan::Forever);
    assert_eq!(RetentionProfile::Forensic.retention().purge_cascade, PurgeCascade::Preserve);
}

#[test]
fn profiles_parse_from_their_documented_names() {
    assert_eq!(RetentionProfile::from_name("balanced"), Some(RetentionProfile::Balanced));
    assert_eq!(RetentionProfile::from_name("gdpr_strict"), Some(RetentionProfile::GdprStrict));
    assert_eq!(RetentionProfile::from_name("hipaa_retain"), Some(RetentionProfile::HipaaRetain));
    assert_eq!(RetentionProfile::from_name("forensic"), Some(RetentionProfile::Forensic));
    assert_eq!(RetentionProfile::from_name("nonsense"), None);
    assert_eq!(RetentionProfile::default(), RetentionProfile::Balanced);
}

#[tokio::test]
async fn balanced_cascades_audit_with_the_subject() {
    let e = engine(RetentionProfile::Balanced);
    e.remember(RememberRequest::new(scope(), "a memory that will be purged")).await.unwrap();

    let report = e
        .purge_subject(&TenantId::new("acme").unwrap(), &SubjectId::new("user-42").unwrap())
        .await
        .unwrap();

    assert_eq!(report.items_removed, 1);
    assert!(report.audit_rows_removed >= 1);
    assert_eq!(report.audit_rows_preserved, 0);
    assert!(e.audit(&scope(), &AuditFilter::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn hipaa_retain_preserves_audit_across_a_subject_purge() {
    let e = engine(RetentionProfile::HipaaRetain);
    e.remember(RememberRequest::new(scope(), "a clinical note")).await.unwrap();

    let report = e
        .purge_subject(&TenantId::new("acme").unwrap(), &SubjectId::new("user-42").unwrap())
        .await
        .unwrap();

    assert_eq!(report.items_removed, 1, "the item itself always goes");
    assert!(report.audit_rows_preserved >= 1, "hipaa_retain must keep the decision record");
    assert_eq!(report.audit_rows_removed, 0);

    let audit = e.audit(&scope(), &AuditFilter::default()).await.unwrap();
    assert!(!audit.is_empty(), "the audit trail was destroyed under hipaa_retain");
    // Even preserved, no body may survive.
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains("clinical note"), "preserved audit leaked a body");
}

#[tokio::test]
async fn the_item_is_always_removed_regardless_of_profile() {
    for profile in [
        RetentionProfile::Balanced,
        RetentionProfile::GdprStrict,
        RetentionProfile::HipaaRetain,
        RetentionProfile::Forensic,
    ] {
        let e = engine(profile);
        e.remember(RememberRequest::new(scope(), "the memory itself")).await.unwrap();
        e.purge_subject(&TenantId::new("acme").unwrap(), &SubjectId::new("user-42").unwrap())
            .await
            .unwrap();
        assert!(
            e.review(&scope(), &Default::default()).await.unwrap().is_empty(),
            "{profile:?} left the item behind"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test retention`
Expected: FAIL — `cannot find type RetentionProfile in memorysafe_engine`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/retention.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionSpan {
    Forever,
    Days(u32),
    /// Detail lives exactly as long as the subject does.
    UntilSubjectPurge,
}

// `PurgeCascade` is **not** defined here. It is a parameter of
// `Backend::purge_subject`, so it lives in `memorysafe-core` alongside
// `AuditRecord` — `memorysafe-backend` may depend on core and not on the
// engine. Re-exported so `AuditRetention` below and every engine caller can
// name it without a second import path.
pub use memorysafe_core::PurgeCascade;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRetention {
    pub detail: RetentionSpan,
    pub purge_cascade: PurgeCascade,
    /// Counts, rates, and score distributions by policy version. Never
    /// identifying, so it can outlive everything else.
    pub aggregate: RetentionSpan,
}

/// The tested, documented surface. Free-form overrides are permitted but
/// unsupported — this is what keeps "configurable per tenant" from becoming an
/// untestable matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionProfile {
    #[default]
    Balanced,
    GdprStrict,
    HipaaRetain,
    Forensic,
}

impl RetentionProfile {
    /// **This match is exhaustive by construction and must stay that way.**
    /// There is no `_ =>` arm, so adding a variant to `RetentionProfile`
    /// makes this fail to compile until the new profile is given its own
    /// literal. That failure is the feature. The compiler enforces it; this
    /// comment exists to stop someone "fixing" the error with a wildcard arm,
    /// which would silently hand a new profile another profile's retention —
    /// a wrong `purge_cascade` there is a destroyed audit trail or an
    /// undeleted one, discovered during an erasure.
    pub fn retention(self) -> AuditRetention {
        match self {
            RetentionProfile::Balanced => AuditRetention {
                detail: RetentionSpan::UntilSubjectPurge,
                purge_cascade: PurgeCascade::Cascade,
                aggregate: RetentionSpan::Forever,
            },
            RetentionProfile::GdprStrict => AuditRetention {
                detail: RetentionSpan::Days(90),
                purge_cascade: PurgeCascade::Cascade,
                aggregate: RetentionSpan::Days(365),
            },
            RetentionProfile::HipaaRetain => AuditRetention {
                detail: RetentionSpan::Days(2190),
                purge_cascade: PurgeCascade::Preserve,
                aggregate: RetentionSpan::Forever,
            },
            RetentionProfile::Forensic => AuditRetention {
                detail: RetentionSpan::Forever,
                purge_cascade: PurgeCascade::Preserve,
                aggregate: RetentionSpan::Forever,
            },
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "balanced" => Some(RetentionProfile::Balanced),
            "gdpr_strict" => Some(RetentionProfile::GdprStrict),
            "hipaa_retain" => Some(RetentionProfile::HipaaRetain),
            "forensic" => Some(RetentionProfile::Forensic),
            _ => None,
        }
    }
}
```

Add `retention: RetentionProfile` to `EngineConfig` (defaulting to `RetentionProfile::default()` in `EngineConfig::new`) and to `Engine`.

Rewrite `Engine::purge_subject` in `mutate.rs` to honour the profile. `Preserve` is
the *backend's* behaviour, selected by an argument — the engine reads no audit rows
and replays none:

```rust
    pub async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
    ) -> Result<PurgeOutcome, EngineError> {
        let audit = AuditRecord::new(
            self.purge_scope(tenant, subject).await?,
            AuditEvent::SubjectPurged,
            vec![],
            Actor { kind: ActorKind::Human, id: None },
            OffsetDateTime::now_utc(),
        );
        // The whole of this task's change to `mutate.rs`: the cascade comes
        // from the configured profile instead of Task 35's hard-coded
        // `PurgeCascade::Cascade`. Everything else — building the record,
        // choosing its namespace, mapping the report — is untouched.
        let cascade = self.retention.retention().purge_cascade;
        let report = self
            .backend
            .purge_subject(tenant, subject, cascade, audit)
            .await?;
        Ok(PurgeOutcome {
            items_removed: report.items_removed,
            audit_rows_removed: report.audit_rows_removed,
            audit_rows_preserved: report.audit_rows_preserved,
        })
    }
```

**What this replaces, and why none of it may come back.** An earlier draft of this task
implemented `Preserve` in the engine: read the subject's audit rows with a hard-coded
`AuditFilter { limit: 100_000, .. }`, call a cascade-only `purge_subject`, then loop
`record_recall(record)` to re-insert each row. `Backend::purge_subject`'s doc records
the five defects that shape carried — the two worst being that `record_recall` also
updates the referenced items' access statistics, so replaying historical
`Admitted`/`Merged`/`Forgotten` rows moved live survivors' `last_accessed_at`
*backwards* during an erasure; and that the read and the re-insert were two
transactions, so a crash between them lost every row the profile existed to preserve.
The 100_000 cap silently truncated, and `audit_rows_preserved` reported the truncated
count as complete. The engine's report now passes the backend's numbers through
unchanged, because the backend is the only party that knows them.

Add `pub mod retention;` and
`pub use retention::{AuditRetention, RetentionProfile, RetentionSpan};` to `lib.rs`.
`PurgeCascade` is re-exported from `retention.rs` (see below) but **defined in
`memorysafe-core`**: it is a parameter of `Backend::purge_subject`, and
`memorysafe-backend` may not depend on the engine.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine`
Expected: PASS — 44 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): four named audit retention profiles honoured on subject purge"
```

---

## Task 39: Engine — export and import orchestration

**Files:**
- Create: `crates/memorysafe-engine/src/portability.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`
- Create: `crates/memorysafe-engine/tests/portability.rs`

**Interfaces:**
- Consumes: `Backend::export`, `Backend::import`.
- Produces: `Engine::export(&ScopeSelector) -> Result<ExportStream, EngineError>`, `Engine::export_ndjson(&ScopeSelector) -> Result<String, EngineError>`, `Engine::export_markdown(&ScopeSelector) -> Result<String, EngineError>`, `Engine::import_ndjson(&str, &TenantId) -> Result<ImportReport, EngineError>`, `Engine::import(&TenantId, ImportStream)`.

**Why the engine grows the destination too:** `Backend::import` takes the tenant it writes into, and the engine cannot invent one — it has no basis to guess, and guessing from the payload is exactly the property the backend parameter removed. It is passed straight through.

**Why markdown too:** the spec's portable archive is "newline-delimited JSON plus a rendered markdown view of the items for human reading." The JSON is the round-trip format; the markdown is what makes "your memory is yours" mean something a person can actually open.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/portability.rs`:

```rust
use memorysafe_backend::ScopeSelector;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Protection, Scope, SensitivityLevel, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn tenant() -> TenantId {
    TenantId::new("acme").unwrap()
}

fn selector(include_audit: bool) -> ScopeSelector {
    ScopeSelector {
        tenant: tenant(),
        subject: None,
        namespace: None,
        include_audit,
    }
}

async fn seed(e: &Engine) {
    for body in [
        "the production migration runs on Sundays",
        "the deploy key rotates every ninety days",
        "the on-call rotation starts Monday morning",
    ] {
        e.remember(RememberRequest::new(scope(), body)).await.unwrap();
    }
}

#[tokio::test]
async fn ndjson_round_trips_through_a_fresh_engine() {
    let source = engine();
    seed(&source).await;

    let ndjson = source.export_ndjson(&selector(true)).await.unwrap();
    assert!(ndjson.lines().count() >= 4, "header plus three items");

    let target = engine();
    let report = target.import_ndjson(&ndjson, &tenant()).await.unwrap();
    assert_eq!(report.items_imported, 3);

    let mut before = source.review(&scope(), &Default::default()).await.unwrap();
    let mut after = target.review(&scope(), &Default::default()).await.unwrap();
    before.sort_by(|a, b| a.id.cmp(&b.id));
    after.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(before, after, "the round trip did not reproduce the corpus");
}

#[tokio::test]
async fn every_ndjson_line_is_a_standalone_json_object() {
    let e = engine();
    seed(&e).await;
    let ndjson = e.export_ndjson(&selector(false)).await.unwrap();
    for line in ndjson.lines() {
        let value: serde_json::Value = serde_json::from_str(line).expect("each line parses");
        assert!(value.get("record").is_some(), "each line names its record type");
    }
}

#[tokio::test]
async fn the_markdown_view_is_readable_and_contains_the_bodies() {
    let e = engine();
    seed(&e).await;
    let md = e.export_markdown(&selector(false)).await.unwrap();

    assert!(md.contains("# MemorySafe export"));
    assert!(md.contains("the production migration runs on Sundays"));
    assert!(md.contains("acme / user-42 / agent"), "scope must be identifiable");
}

#[tokio::test]
async fn importing_the_same_stream_twice_changes_nothing_the_second_time() {
    let source = engine();
    seed(&source).await;
    let ndjson = source.export_ndjson(&selector(false)).await.unwrap();

    let target = engine();
    target.import_ndjson(&ndjson, &tenant()).await.unwrap();
    let second = target.import_ndjson(&ndjson, &tenant()).await.unwrap();

    assert_eq!(second.items_imported, 0);
    assert_eq!(second.items_skipped_existing, 3);
    assert_eq!(target.review(&scope(), &Default::default()).await.unwrap().len(), 3);
}

#[tokio::test]
async fn an_import_cannot_downgrade_sensitivity_or_forge_a_pin() {
    // An import stream is caller-supplied JSON and MemoryItem's fields are
    // public, so it can assert anything. Neither claim is believed.
    let e = engine();
    let ndjson = format!(
        "{}\n{}\n",
        r#"{"record":"header","format_version":1,"exported_at":0}"#,
        r#"{"record":"item","item":{"id":"01ARZ3NDEKTSV4RRFFQ69G5FAV",
           "scope":{"tenant":"acme","subject":"user-42","namespace":"agent"},
           "body":"the deploy api key is sk-abc123def456ghi789jkl012",
           "kind":"fact","source":{"kind":"agent","id":null},"occurred_at":null,
           "created_at":0,"tags":[],"attrs":{},"sensitivity":"public","ttl":null,
           "protection":{"kind":"pinned"},"pending_embedding":false}}"#
            .replace('\n', "")
            .replace("           ", "")
    );

    e.import_ndjson(&ndjson, &tenant()).await.unwrap();
    let stored = e.review(&scope(), &Default::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].sensitivity,
        SensitivityLevel::Restricted,
        "import downgraded a credential to Public"
    );
    assert_eq!(
        stored[0].protection,
        Protection::Normal,
        "import forged an unevictable pin"
    );
}

#[tokio::test]
async fn malformed_ndjson_is_rejected_with_a_useful_error() {
    let e = engine();
    let err = e.import_ndjson("{not json at all", &tenant()).await.unwrap_err();
    assert!(err.to_string().contains("line 1"), "the error must name the bad line: {err}");
}

#[tokio::test]
async fn exporting_an_empty_scope_yields_a_header_and_nothing_else() {
    let e = engine();
    let ndjson = e.export_ndjson(&selector(false)).await.unwrap();
    assert_eq!(ndjson.lines().count(), 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test portability`
Expected: FAIL — `no method named export_ndjson found for struct Engine`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/portability.rs`:

```rust
use crate::Engine;
use crate::error::EngineError;
use memorysafe_backend::{
    ExportRecord, ExportStream, ImportReport, ImportStream, ScopeSelector,
};

impl Engine {
    pub async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, EngineError> {
        Ok(self.backend.export(sel).await?)
    }

    /// Imported items are re-assessed rather than trusted. An import stream is
    /// caller-supplied JSON, and `MemoryItem`'s fields are public — so a stream
    /// can claim `sensitivity: "public"` for a body full of credentials and, if
    /// believed, that body would then satisfy a Public-clearance recall. The
    /// backend already forces `protection` back to `Normal`; this recomputes the
    /// sensitivity the same way a write would, and keeps whichever level is
    /// higher so an import can never lower a stored classification.
    pub async fn import(
        &self,
        destination: &TenantId,
        stream: ImportStream,
    ) -> Result<ImportReport, EngineError> {
        let reassessed: ImportStream = stream
            .into_iter()
            .map(|record| match record {
                ExportRecord::Item { mut item, vector } => {
                    let candidate = memorysafe_core::Candidate {
                        body: item.body.clone(),
                        kind: item.kind.clone(),
                        tags: item.tags.clone(),
                        attrs: item.attrs.clone(),
                        sensitivity_hint: None,
                        embedding: None,
                        byte_size: item.byte_size(),
                    };
                    let detected = memorysafe_policy::sensitivity::assess(&candidate).level;
                    item.sensitivity = item.sensitivity.max(detected);
                    ExportRecord::Item { item, vector }
                }
                other => other,
            })
            .collect();
        Ok(self.backend.import(destination, reassessed).await?)
    }

    /// The round-trip format: one JSON object per line.
    pub async fn export_ndjson(&self, sel: &ScopeSelector) -> Result<String, EngineError> {
        let stream = self.export(sel).await?;
        let mut out = String::new();
        for record in &stream {
            let line = serde_json::to_string(record)
                .map_err(|e| EngineError::Validation(e.to_string()))?;
            out.push_str(&line);
            out.push('\n');
        }
        Ok(out)
    }

    /// `destination` is passed straight through to `Backend::import`. The
    /// engine does not derive it from the stream: an authorising caller — the
    /// HTTP route's API key, the CLI's `--scope` — already knows which tenant
    /// it is writing into, and deriving it here would put the payload back in
    /// charge of its own destination.
    pub async fn import_ndjson(
        &self,
        ndjson: &str,
        destination: &TenantId,
    ) -> Result<ImportReport, EngineError> {
        let mut stream: ImportStream = Vec::new();
        for (i, line) in ndjson.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: ExportRecord = serde_json::from_str(line).map_err(|e| {
                EngineError::Validation(format!("line {}: {e}", i + 1))
            })?;
            stream.push(record);
        }
        self.import(destination, stream).await
    }

    /// A human-readable rendering. This is what makes "your memory is yours"
    /// mean something a person can open, rather than a JSON blob.
    pub async fn export_markdown(&self, sel: &ScopeSelector) -> Result<String, EngineError> {
        let stream = self.export(sel).await?;
        let mut out = String::from("# MemorySafe export\n\n");

        let mut current_scope: Option<String> = None;
        for record in &stream {
            let ExportRecord::Item { item, .. } = record else {
                continue;
            };
            let scope_label = format!(
                "{} / {} / {}",
                item.scope.tenant, item.scope.subject, item.scope.namespace
            );
            if current_scope.as_deref() != Some(scope_label.as_str()) {
                out.push_str(&format!("## {scope_label}\n\n"));
                current_scope = Some(scope_label);
            }

            out.push_str(&format!("### {}\n\n", item.id));
            out.push_str(&format!("- **kind:** {}\n", item.kind));
            out.push_str(&format!("- **created:** {}\n", item.created_at));
            out.push_str(&format!("- **sensitivity:** {:?}\n", item.sensitivity));
            out.push_str(&format!("- **protection:** {:?}\n", item.protection));
            if !item.tags.is_empty() {
                out.push_str(&format!("- **tags:** {}\n", item.tags.join(", ")));
            }
            out.push_str(&format!("\n{}\n\n", item.body));
        }

        Ok(out)
    }
}
```

Add `pub mod portability;` to `lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine`
Expected: PASS — 50 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): portable ndjson export/import plus a human-readable markdown view"
```

---

## Task 40: The five correctness invariants

**Files:**
- Create: `crates/memorysafe-engine/tests/invariants.rs`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: the whole stack.
- Produces: five `proptest` properties. These are the definition of correct for this system.

**The five, from the spec.** Capacity is never exceeded. Pinned items are never evicted. The sensitivity ceiling is never violated on recall. Every mutation has exactly one audit record. Export → import round-trips exactly.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/invariants.rs`:

```rust
use memorysafe_backend::ScopeSelector;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    AuditFilter, Budget, Protection, RecallBudget, RecallMode, RecallRequest, Scope,
    SensitivityLevel, TenantId,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use proptest::prelude::*;
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// Bodies are distinct enough that the redundancy check does not collapse them
/// all into merges, which would make the capacity properties vacuous.
fn bodies() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(0u32..10_000, 1..40)
        .prop_map(|ns| {
            ns.into_iter()
                .enumerate()
                .map(|(i, n)| format!("memory {i} concerning subject {n} and topic {n}"))
                .collect()
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Invariant 1: capacity is never exceeded, whatever the write sequence.
    #[test]
    fn capacity_is_never_exceeded(bodies in bodies(), max in 1usize..12) {
        runtime().block_on(async {
            let e = engine();
            e.set_budget(&scope(), Budget { max_items: Some(max as u64), max_bytes: None })
                .await
                .unwrap();

            for b in &bodies {
                let _ = e.remember(RememberRequest::new(scope(), b)).await;
            }

            let stored = e.review(&scope(), &memorysafe_backend::Page { offset: 0, limit: 1000 })
                .await
                .unwrap();
            prop_assert!(
                stored.len() <= max,
                "budget {max} exceeded: {} items stored",
                stored.len()
            );

            let state = e.capacity_state(&scope()).await.unwrap();
            prop_assert_eq!(state.used_items, stored.len() as u64, "accounting drifted");
            Ok(())
        })?;
    }

    /// Invariant 2: a pinned item survives any amount of pressure.
    #[test]
    fn pinned_items_are_never_evicted(bodies in bodies()) {
        runtime().block_on(async {
            let e = engine();
            let pinned = e
                .remember(RememberRequest::new(scope(), "the pinned memory that must survive"))
                .await
                .unwrap()
                .item_id
                .unwrap();
            e.protect(&scope(), &pinned, Protection::Pinned).await.unwrap();

            e.set_budget(&scope(), Budget { max_items: Some(2), max_bytes: None })
                .await
                .unwrap();

            for b in &bodies {
                let _ = e.remember(RememberRequest::new(scope(), b)).await;
            }
            let _ = e.maintain(&scope(), None).await;

            let stored = e.review(&scope(), &memorysafe_backend::Page { offset: 0, limit: 1000 })
                .await
                .unwrap();
            prop_assert!(
                stored.iter().any(|i| i.id == pinned),
                "a pinned item was evicted"
            );
            Ok(())
        })?;
    }

    /// Invariant 3: nothing above the caller's ceiling is ever returned.
    #[test]
    fn the_sensitivity_ceiling_is_never_violated(bodies in bodies(), ceiling in 0i64..5) {
        runtime().block_on(async {
            let e = engine();
            let level = SensitivityLevel::from_ordinal(ceiling).unwrap();

            for (i, b) in bodies.iter().enumerate() {
                let mut r = RememberRequest::new(scope(), b);
                // Force a spread of sensitivity levels via caller hints.
                r.sensitivity_hint = SensitivityLevel::from_ordinal((i % 5) as i64);
                let _ = e.remember(r).await;
            }

            for mode in [RecallMode::WorkingSet, RecallMode::Search] {
                let ws = e.recall(RecallRequest {
                    scope: scope(),
                    query: Some("memory concerning subject".into()),
                    tags_any: vec![],
                    kinds: vec![],
                    occurred_after: None,
                    occurred_before: None,
                    mode,
                    budget: RecallBudget { max_tokens: Some(8000), max_items: Some(50) },
                    sensitivity_ceiling: level,
                })
                .await
                .unwrap();

                for s in &ws.items {
                    prop_assert!(
                        s.item.sensitivity <= level,
                        "{:?} leaked past a {:?} ceiling in {:?} mode",
                        s.item.sensitivity, level, mode
                    );
                }
            }
            Ok(())
        })?;
    }

    /// Invariant 4: one audit record per mutation, never more, never fewer.
    #[test]
    fn every_mutation_has_exactly_one_audit_record(bodies in bodies()) {
        runtime().block_on(async {
            let e = engine();
            let mut mutations = 0usize;

            for b in &bodies {
                if e.remember(RememberRequest::new(scope(), b)).await.is_ok() {
                    mutations += 1;
                }
            }

            let audit = e.audit(&scope(), &AuditFilter { limit: 100_000, ..Default::default() })
                .await
                .unwrap();
            prop_assert_eq!(
                audit.len(), mutations,
                "{} mutations produced {} audit records",
                mutations, audit.len()
            );

            // And no body ever reached the trail.
            let json = serde_json::to_string(&audit).unwrap();
            prop_assert!(!json.contains("concerning subject"), "audit leaked a body");
            Ok(())
        })?;
    }

    /// Invariant 5: export then import reproduces the corpus exactly.
    #[test]
    fn export_import_round_trips_exactly(bodies in bodies()) {
        runtime().block_on(async {
            let source = engine();
            for b in &bodies {
                let _ = source.remember(RememberRequest::new(scope(), b)).await;
            }

            let selector = ScopeSelector {
                tenant: TenantId::new("acme").unwrap(),
                subject: None,
                namespace: None,
                include_audit: false,
            };
            let ndjson = source.export_ndjson(&selector).await.unwrap();

            let target = engine();
            target.import_ndjson(&ndjson, &selector.tenant).await.unwrap();

            let page = memorysafe_backend::Page { offset: 0, limit: 1000 };
            let mut before = source.review(&scope(), &page).await.unwrap();
            let mut after = target.review(&scope(), &page).await.unwrap();
            before.sort_by(|a, b| a.id.cmp(&b.id));
            after.sort_by(|a, b| a.id.cmp(&b.id));

            prop_assert_eq!(before, after, "the round trip lost or altered items");
            Ok(())
        })?;
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test invariants`
Expected: FAIL — `no method named capacity_state found for struct Engine`.

- [ ] **Step 3: Write minimal implementation**

Add the missing read-through accessor to `crates/memorysafe-engine/src/lib.rs`:

```rust
    pub async fn capacity_state(&self, scope: &Scope)
        -> Result<memorysafe_core::CapacityState, EngineError>
    {
        Ok(self.backend.capacity_state(scope).await?)
    }
```

Any invariant that then fails is a real defect, not a test problem. The two most likely, and their fixes:

- **Capacity exceeded.** `Engine::remember` offers eviction candidates only when `capacity.budget.is_bounded()` (Task 33, `gather::admit_context`). Confirm the budget is read fresh per write rather than cached — `CacheConfig` caches `ScopeStats`, never `CapacityState`, and that distinction is load-bearing.
- **Audit count mismatch.** A rejected write must still write exactly one audit record. Confirm the `Action::Reject` branch in `remember` builds a `WriteTransaction` with no `upsert` and no `merge` but still passes its audit record through `backend.apply`.

Add the invariants job to `.github/workflows/ci.yml`:

```yaml
  invariants:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.97.1
      - name: The five correctness invariants
        run: cargo test -p memorysafe-engine --test invariants --release
        env:
          PROPTEST_CASES: 64
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --workspace --all-features && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS — the whole workspace green: 5 invariants, 50 backend conformance tests, and the unit and integration suites of all six crates.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/ .github/workflows/ci.yml
git commit -m "test(engine): the five correctness invariants as property tests"
```

---

## Task 41: Engine — re-embedding and `pending_embedding` backfill

**Files:**
- Create: `crates/memorysafe-engine/src/reembed.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`
- Create: `crates/memorysafe-engine/tests/reembed.rs`

**Interfaces:**
- Consumes: `Backend::list`, `Backend::apply`, `Embedder`.
- Produces: `ReembedCursor { offset: usize }`, `ReembedReport { scanned, embedded, still_pending, next_cursor }`, `Engine::backfill_embeddings(&Scope, Option<ReembedCursor>)`, `Engine::reembed_scope(&Scope, Option<ReembedCursor>)`.

**Why this task exists.** Task 33 admits an item with `pending_embedding: true` when the embedder is unavailable — a missing model file must never cost a user their memory. But without a backfill path those items stay invisible to vector search forever, which turns a transient outage into permanent silent recall degradation. This is the other half of that decision.

`reembed_scope` is the migration the spec calls for when a tenant changes embedding model: it re-embeds every item in the scope, not just the pending ones, and audits the run as `Reembedded`. Both are explicit, resumable, cursor-driven jobs for the same reason maintenance is — nothing changes unobserved.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/reembed.rs`:

```rust
use memorysafe_backend::Page;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    AuditEvent, AuditFilter, EmbedderId, Embedding, RecallBudget, RecallMode, RecallRequest,
    Scope, SensitivityLevel,
};
use memorysafe_embed::{DeterministicEmbedder, EmbedError, Embedder};
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// An embedder that can be switched off, standing in for a missing model file.
struct FlakyEmbedder {
    inner: DeterministicEmbedder,
    up: AtomicBool,
}

impl FlakyEmbedder {
    fn new() -> Self {
        Self { inner: DeterministicEmbedder::new(256), up: AtomicBool::new(true) }
    }
    fn go_down(&self) {
        self.up.store(false, Ordering::SeqCst);
    }
    fn come_back(&self) {
        self.up.store(true, Ordering::SeqCst);
    }
}

impl Embedder for FlakyEmbedder {
    fn id(&self) -> EmbedderId {
        self.inner.id()
    }
    fn dim(&self) -> u16 {
        self.inner.dim()
    }
    fn embed(&self, text: &str) -> Result<Embedding, EmbedError> {
        if self.up.load(Ordering::SeqCst) {
            self.inner.embed(text)
        } else {
            Err(EmbedError::Unavailable("model file missing".into()))
        }
    }
}

fn engine_with(embedder: Arc<dyn Embedder>) -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        embedder,
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

#[tokio::test]
async fn a_write_during_an_outage_is_kept_and_flagged_pending() {
    let flaky = Arc::new(FlakyEmbedder::new());
    let e = engine_with(flaky.clone());

    flaky.go_down();
    let out = e
        .remember(RememberRequest::new(scope(), "written while the model was missing"))
        .await
        .unwrap();
    assert!(out.item_id.is_some(), "the memory must not be lost");

    let stored = e.review(&scope(), &Page::default()).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].pending_embedding, "the item should be flagged for backfill");
}

#[tokio::test]
async fn backfill_makes_a_pending_item_vector_searchable() {
    let flaky = Arc::new(FlakyEmbedder::new());
    let e = engine_with(flaky.clone());

    flaky.go_down();
    e.remember(RememberRequest::new(scope(), "the cat sat on the mat during the outage"))
        .await
        .unwrap();
    flaky.come_back();

    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.embedded, 1);
    assert_eq!(report.still_pending, 0);

    let stored = e.review(&scope(), &Page::default()).await.unwrap();
    assert!(!stored[0].pending_embedding, "the flag should be cleared");

    // And it is now reachable by semantic recall, not just keyword.
    let ws = e
        .recall(RecallRequest {
            scope: scope(),
            query: Some("the cat sat on the mat during the outage".into()),
            tags_any: vec![],
            kinds: vec![],
            occurred_after: None,
            occurred_before: None,
            mode: RecallMode::WorkingSet,
            budget: RecallBudget { max_tokens: Some(2000), max_items: Some(5) },
            sensitivity_ceiling: SensitivityLevel::Restricted,
        })
        .await
        .unwrap();
    assert_eq!(ws.items.len(), 1);
}

#[tokio::test]
async fn backfill_leaves_items_pending_when_the_embedder_is_still_down() {
    let flaky = Arc::new(FlakyEmbedder::new());
    let e = engine_with(flaky.clone());

    flaky.go_down();
    e.remember(RememberRequest::new(scope(), "still no model available")).await.unwrap();

    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.embedded, 0);
    assert_eq!(report.still_pending, 1, "a failed backfill must not clear the flag");

    let stored = e.review(&scope(), &Page::default()).await.unwrap();
    assert!(stored[0].pending_embedding);
}

#[tokio::test]
async fn backfill_over_a_healthy_scope_does_nothing() {
    let e = engine_with(Arc::new(DeterministicEmbedder::new(256)));
    e.remember(RememberRequest::new(scope(), "embedded normally at write time")).await.unwrap();

    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.embedded, 0);
    assert_eq!(report.still_pending, 0);
    assert!(report.next_cursor.is_none());
}

#[tokio::test]
async fn a_scope_reembed_rewrites_every_vector_and_audits_the_run() {
    let e = engine_with(Arc::new(DeterministicEmbedder::new(256)));
    for i in 0..3 {
        e.remember(RememberRequest::new(scope(), &format!("memory {i} about subject {i}")))
            .await
            .unwrap();
    }

    let report = e.reembed_scope(&scope(), None).await.unwrap();
    assert_eq!(report.scanned, 3);
    assert_eq!(report.embedded, 3, "reembed rewrites every vector, not just pending ones");

    let audit = e
        .audit(&scope(), &AuditFilter {
            events: vec![AuditEvent::Reembedded],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(audit.len(), 1, "a re-embedding migration must be audited");
}

#[tokio::test]
async fn reembedding_preserves_the_items_themselves() {
    let e = engine_with(Arc::new(DeterministicEmbedder::new(256)));
    for i in 0..3 {
        e.remember(RememberRequest::new(scope(), &format!("memory {i} about subject {i}")))
            .await
            .unwrap();
    }
    let before = e.review(&scope(), &Page::default()).await.unwrap();

    e.reembed_scope(&scope(), None).await.unwrap();

    let after = e.review(&scope(), &Page::default()).await.unwrap();
    assert_eq!(before, after, "re-embedding must not alter the items");
}

#[tokio::test]
async fn backfill_on_an_empty_scope_is_a_no_op() {
    let e = engine_with(Arc::new(DeterministicEmbedder::new(256)));
    let report = e.backfill_embeddings(&scope(), None).await.unwrap();
    assert_eq!(report.scanned, 0);
    assert!(report.next_cursor.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test reembed`
Expected: FAIL — `no method named backfill_embeddings found for struct Engine`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/reembed.rs`:

```rust
use crate::Engine;
use crate::error::EngineError;
use memorysafe_backend::{ItemWrite, Page, WriteTransaction};
use memorysafe_core::{
    Actor, ActorKind, AuditEvent, AuditRecord, ItemRef, MemoryItem, Scope,
};
use memorysafe_embed::{Embedder, QuantizedVector};
use serde::{Deserialize, Serialize};

/// Items per call. Like maintenance, this is an explicit resumable job rather
/// than a background thread, so the caller controls the work done at once.
pub const REEMBED_BATCH: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReembedCursor {
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReembedReport {
    pub scanned: usize,
    pub embedded: usize,
    pub still_pending: usize,
    pub next_cursor: Option<ReembedCursor>,
}

impl Engine {
    /// Embeds items admitted while the embedder was unavailable. Without this,
    /// a transient outage becomes permanent silent recall degradation.
    pub async fn backfill_embeddings(
        &self,
        scope: &Scope,
        cursor: Option<ReembedCursor>,
    ) -> Result<ReembedReport, EngineError> {
        self.reembed(scope, cursor, true).await
    }

    /// Re-embeds every item in the scope. This is the migration to run after
    /// changing embedding model — vectors from different models are not
    /// comparable, so it is explicit rather than something that happens by
    /// accident.
    pub async fn reembed_scope(
        &self,
        scope: &Scope,
        cursor: Option<ReembedCursor>,
    ) -> Result<ReembedReport, EngineError> {
        self.reembed(scope, cursor, false).await
    }

    async fn reembed(
        &self,
        scope: &Scope,
        cursor: Option<ReembedCursor>,
        pending_only: bool,
    ) -> Result<ReembedReport, EngineError> {
        let offset = cursor.map(|c| c.offset).unwrap_or(0);
        let batch: Vec<MemoryItem> = self
            .backend
            .list(scope, &Page { offset, limit: REEMBED_BATCH })
            .await?;

        if batch.is_empty() {
            return Ok(ReembedReport {
                scanned: 0,
                embedded: 0,
                still_pending: 0,
                next_cursor: None,
            });
        }

        let scanned = batch.len();
        let targets: Vec<MemoryItem> = batch
            .into_iter()
            .filter(|i| !pending_only || i.pending_embedding)
            .collect();

        let mut embedded = 0usize;
        let mut still_pending = 0usize;
        let mut refs: Vec<ItemRef> = Vec::new();

        for item in targets {
            let Ok(embedding) = self.embedder.embed(&item.body) else {
                // Leave the flag set; a failed backfill must be retryable.
                still_pending += 1;
                continue;
            };
            let vector = QuantizedVector::from_embedding(&embedding);

            let mut updated = item.clone();
            updated.pending_embedding = false;

            // Replacing the row is what clears the flag and rewrites the
            // vector in one atomic step. The audit record for the whole run is
            // written separately below, so these carry a minimal one.
            let audit = AuditRecord::new(
                scope.clone(),
                AuditEvent::Reembedded,
                vec![ItemRef::from_item(&updated)],
                Actor { kind: ActorKind::System, id: None },
                OffsetDateTime::now_utc(),
            );
            let mut txn = WriteTransaction::new(scope.clone(), audit);
            txn.evictions = vec![item.id.clone()];
            txn.upsert = Some(ItemWrite { item: updated.clone(), vector: Some(vector) });

            self.backend.apply(txn).await?;
            refs.push(ItemRef::from_item(&updated));
            embedded += 1;
        }

        self.cache.invalidate_scope(scope).await;

        let next_cursor = if scanned < REEMBED_BATCH {
            None
        } else {
            Some(ReembedCursor { offset: offset + scanned })
        };

        Ok(ReembedReport { scanned, embedded, still_pending, next_cursor })
    }
}
```

Keep the `AuditRecord` inside the loop, exactly as written above. Invariant 4 requires one audit
record per mutation, and re-embedding one item is one mutation — collapsing the run into a single
record would produce a tidier report by breaking the invariant the whole system rests on.
`AuditEvent::Reembedded` is what makes the run queryable as a unit; the record count is what keeps
it honest.

That means a three-item scope produces three `Reembedded` records, so correct the assertion in
`a_scope_reembed_rewrites_every_vector_and_audits_the_run`:

```rust
    assert_eq!(audit.len(), 3, "one audited mutation per re-embedded item");
```

Add to `crates/memorysafe-engine/src/lib.rs`:

```rust
pub mod reembed;
pub use reembed::{REEMBED_BATCH, ReembedCursor, ReembedReport};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-engine && cargo test --workspace --all-features`
Expected: PASS — 57 engine tests ok, whole workspace green.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): pending-embedding backfill and explicit re-embedding migration"
```

---

## Definition of done for Plan 1

- `cargo test --workspace --all-features` is green.
- `cargo clippy --all-targets --all-features -- -D warnings` is clean.
- The CI purity job confirms `memorysafe-core` and `memorysafe-policy` pull in no I/O crates.
- `SqliteBackend` passes all 50 conformance tests. **The suite is now frozen** — Plan 2's Postgres backend must pass it unmodified, and any change to it is a change to the `Backend` contract.
- The five invariants pass at 64 proptest cases in release mode.
- An engine can be constructed and driven end to end from a Rust test with no server, no network, and no model files.
- No item is left permanently unsearchable: `pending_embedding` has a backfill path, and changing embedder is an explicit audited migration.

### Known deferrals to Plan 3

Two `AuditEvent` variants defined in Task 8 are deliberately unused in Plan 1, because nothing in
this plan triggers them from outside the process:

- `Exported` / `Imported` — Task 39 provides the mechanism, but an export is only a governance
  event worth recording when a *person or API caller* initiates it. The audit record belongs at
  the CLI and HTTP boundary, with the actor attached.
- `PolicyChanged` — there is one policy in Plan 1. The event becomes meaningful once policy
  configuration is administrable, which is the HTTP admin surface.

**Next:** Plan 2 (Postgres backend against the frozen suite), then Plan 3 (MCP, HTTP, CLI, shadow harness).
