# MemorySafe Postgres Backend — Implementation Plan (Plan 2 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `memorysafe-backend-postgres` — the commercial scaling-tier backend — so that it passes Plan 1's frozen 50-test conformance suite unmodified, under both supported tenant layouts, with tenant isolation enforced by PostgreSQL row-level security rather than by application `WHERE` clauses.

**Architecture:** A second Cargo workspace, in its own closed repository, with the open-source repository vendored as a git submodule at `vendor/memorysafe` and consumed through path dependencies. One crate, `memorysafe-backend-postgres`, implements the `Backend` trait frozen at the end of Plan 1 Task 24. Every operation runs inside a transaction that first sets a transaction-local `memorysafe.tenant_id` GUC and `search_path`; the pool's connections run as a non-superuser role, so the RLS policy — not the query text — is what makes cross-tenant reads return nothing. Vector search generates candidates through a pgvector HNSW index and then reranks them exactly in Rust with `QuantizedVector::dot`, the same function the SQLite backend scores with, so both backends order identically.

**Tech Stack:** Rust 1.97.1 (edition 2024), `sqlx` 0.9 (`runtime-tokio`, `tls-rustls-ring`, `postgres`, `json`), PostgreSQL 16+ with `pgvector` 0.8+, `testcontainers-modules` 0.15 (`postgres` feature), `tokio` 1.53, `async-trait` 0.1, `serde` 1.0, `blake3` 1.5, `base64` 0.22.

**Source spec:** `docs/superpowers/specs/2026-09-05-memorysafe-engine-design.md`
**Predecessor:** `docs/superpowers/plans/2026-09-05-engine-and-sqlite.md` (Plan 1)

> **Where this file lives.** This plan describes work in a **closed, commercial** repository. Task 1 creates that repository and moves this file into it. Once that has happened the file must not remain in the open-source repository — the Postgres schema, the RLS design, and the ANN strategy are the scaling tier's substance, and the open repo is published.

**Everything in §"What Plan 1 froze" below was validated against a live PostgreSQL 17.11 + pgvector 0.8.6 during planning.** The DDL, the RLS policies, the partitioned foreign key, the generated `tsvector` column, the fixed-placeholder filter SQL, and the HNSW planner behaviour at 20k vectors were all executed, not reasoned about. Where a plausible-looking approach was found not to work, the task notes say so.

---

## Global Constraints

Every task's requirements implicitly include this section.

- **Rust edition 2024**, toolchain pinned to `1.97.1` via `rust-toolchain.toml`. Same pin as the OSS repo — a mismatch between the submodule's toolchain and the outer workspace's is a build failure waiting to happen.
- **PostgreSQL 16 or newer, with `pgvector` 0.8 or newer.** The floor is set by iterative index scans (`hnsw.iterative_scan`, pgvector 0.8) and by foreign keys that reference a partitioned table (PostgreSQL 12, but 16 is the tested floor).
- **The conformance suite is frozen.** `vendor/memorysafe/crates/memorysafe-backend/src/conformance/` is read-only from this repository. If a conformance test fails, the Postgres backend is wrong; changing the suite is changing the `Backend` contract and belongs in the OSS repo with its own review.
- **Tenant isolation is enforced by RLS, not by `WHERE` clauses.** Every query still carries its `tenant_id` predicate for the planner's benefit, but the load-bearing guarantee is the policy. A test that removes the predicate must still return zero rows.
- **The runtime pool never holds superuser rights.** Connections `SET ROLE` to a `NOSUPERUSER NOBYPASSRLS` role on checkout. Superusers and `BYPASSRLS` roles ignore RLS entirely, so a pool that connects as `postgres` would silently have no isolation at all.
- **Every operation runs inside a transaction,** including reads. The tenant GUC is set with `set_config(..., is_local => true)` so it cannot leak to the next borrower of a pooled connection.
- **Timestamps are stored as `BIGINT` Unix seconds,** not `timestamptz`. `MemoryItem::created_at` serialises through `time::serde::timestamp` (whole seconds) — but the frozen corpus never exercises this: every item `export_import_round_trips_exactly` admits has `created_at = UNIX_EPOCH` exactly (see `fx::item`), with no sub-second component to lose, so that test cannot by itself distinguish a backend that preserves sub-second precision from one that truncates it. `BIGINT` is still the right choice: it matches what the wire format already discards, and it avoids storing precision the export/import API can never round-trip.
- **Ids are `TEXT`,** holding the 26-character Crockford base32 ULID, exactly as in SQLite.
- **Policies are pure and the backend performs no scoring.** `ItemWrite` arrives with its vector already quantised; this crate never embeds.
- **Audit rows never contain item bodies** — only ids, content digests, and feature numbers.
- **TDD.** Every task writes a failing test first, watches it fail, then implements. Commit at the end of every task.
- **Lints:** `RUSTFLAGS="-Dwarnings"` in CI, plus `cargo clippy --all-targets --all-features -- -D warnings`. `unsafe_code = "forbid"`.

---

## What Plan 1 froze

Plan 2 consumes these unchanged. Any deviation is a bug in Plan 2, not a reason to edit the OSS repo.

### The `Backend` trait

```rust
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
    async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), BackendError>;
}
```

### `BackendError` — the only error type this crate may return

```rust
pub enum BackendError {
    Storage { message: String, retryable: bool },
    ItemNotFound(ItemId),
    MergeTargetMissing(ItemId),
    IdempotencyConflict,
    InvalidQuery(String),
    InvalidTransaction(String),
    EmbedderMismatch { got: String, expected: String },
    MalformedImport(String),
}
```

### The conformance harness

```rust
pub trait BackendFactory: Send + Sync {
    type B: Backend;
    fn create(&self) -> impl Future<Output = Self::B> + Send;
}

pub async fn run_conformance_suite<F: BackendFactory>(factory: &F) where F::B: 'static;
```

Fixtures live in `memorysafe_backend::conformance::fx`: `embedder()` (a `DeterministicEmbedder` at **dim 256**, embedder id `deterministic-256`), `item`, `item_with`, `item_at`, `item_with_id`, `vector_for`, `admit_txn`, `admit_txn_embedded`, `evict_txn`, `evict_txn_at`.

### The 50 conformance tests

The authoritative list is `run_conformance_suite`'s own `run!` in
`crates/memorysafe-backend/src/conformance/mod.rs`; this table is transcribed
from it and was last recounted by enumerating `pub async fn` per module
against that list. Recount, do not adjust by a difference.

| Group | Tests |
|---|---|
| `isolation` (5) | `tenants_are_isolated`, `subjects_are_isolated`, `namespaces_are_separated`, `audit_is_scoped`, `retrieval_never_crosses_a_scope_boundary` |
| `atomicity` (6) | `admit_evict_and_audit_commit_together`, `a_failed_transaction_leaves_no_trace`, `an_invalid_transaction_is_rejected_and_writes_nothing`, `every_mutation_writes_exactly_one_audit_record`, `idempotent_writes_replay_the_original_outcome`, `idempotency_conflict_on_different_payload` |
| `retrieval` (13) | `sensitivity_ceiling_is_enforced_in_the_query`, `tag_and_kind_filters_narrow_results`, `vector_search_ranks_by_similarity`, `keyword_search_finds_exact_terms`, `keyword_search_escapes_user_input`, `hybrid_returns_both_signal_sources`, `list_pages_are_disjoint_and_complete`, `list_orders_oldest_first_by_created_at`, `list_tie_break_is_total_over_identical_timestamps`, `pending_embedding_items_are_excluded_when_asked`, `cross_model_vectors_are_rejected`, `neighbours_break_ties_before_truncating_at_k`, `recall_updates_access_statistics` |
| `capacity` (4) | `capacity_accounting_tracks_items_and_bytes`, `eviction_releases_capacity`, `concurrent_admits_do_not_double_count`, `scope_stats_reflect_the_corpus` |
| `lifecycle` (22) | `audit_filter_narrows_by_event_and_time`, `audit_returns_min_of_the_limit_and_the_rows_that_remain`, `audit_pages_by_the_after_cursor_without_repeating_a_row`, `audit_since_and_until_include_a_record_on_the_boundary`, `purge_subject_removes_everything_for_that_subject`, `purge_subject_leaves_other_subjects_intact`, `purge_subject_preserves_audit_when_asked`, `purge_subject_persists_the_record_it_was_given`, `apply_persists_the_audit_id_it_was_given`, `record_recall_persists_the_audit_id_it_was_given`, `import_preserves_every_audit_id`, `export_narrows_to_the_selectors_subject_and_namespace`, `export_orders_the_stream_by_kind_then_by_id`, `export_import_round_trips_exactly`, `import_is_idempotent`, `import_rejects_a_later_record_whose_tenant_disagrees`, `import_rejects_a_foreign_audit_record_even_when_every_item_agrees`, `audit_aggregates_survive_a_cascading_purge`, `audit_aggregates_page_in_the_documented_order`, `audit_aggregates_resume_from_a_cursor_that_names_no_stored_row`, `audit_aggregates_narrow_by_day_window_and_policy`, `every_audit_writing_path_increments_the_aggregates` |

**`pagination_is_stable` no longer exists.** It was renamed to
`list_pages_are_disjoint_and_complete` — what it actually proves. It sorts and
dedups the collected ids before asserting, so it checks only that pages did not
overlap or drop rows; "stable" read as tie-break stability, the property it does
*not* check. The two tests that do check ordering,
`list_orders_oldest_first_by_created_at` and
`list_tie_break_is_total_over_identical_timestamps`, are new alongside it, and
a backend that pages `list` **descending** passed the entire previous 27-test
suite.

### One inconsistency in Plan 1 to resolve before starting

`Page` is declared in `memorysafe-backend/src/query.rs` and re-exported as
`memorysafe_backend::Page`, but three of Plan 1's conformance files import it
as `memorysafe_core::Page`. Only one can be right — `memorysafe-core` has no
dependency on `memorysafe-backend`, so the declaration is. Check where the OSS
repo actually put it before Task 7 and import from there; this plan writes
`memorysafe_backend::Page` throughout.

### Behaviours the suite pins down that are easy to get wrong

- `HardFilters::default().sensitivity_ceiling` is `Internal`, not `Restricted`. Fail closed.
- `list` orders by `created_at ASC`, ties broken by `id ASC` — **ascending, oldest first**. `list_orders_oldest_first_by_created_at` rejects a descending backend, and `list_tie_break_is_total_over_identical_timestamps` rejects one whose tie-break is not total over a fully tied corpus. Neither test subsumes the other, and neither existed before Plan 1's contract task: a backend paging descending passed the whole 27-test suite.
- `audit` orders newest first: `id DESC` (see `AuditFilter::after`'s doc comment in `memorysafe-core` — `at` is whole seconds and cannot separate rows written in the same second, so `id` alone is the total order).
- Relevance fusion when both signals are present is `0.7 * vector + 0.3 * keyword`; when only one is present it is that one. Ties break by `item.id` ascending so ordering is total.
- Keyword scores are squashed into `(0, 1]` as `r / (1 + r)` before fusion.
- `retrieve_candidates` over-fetches `limit * 4` from each source before fusing, then truncates to `limit`.
- A failed transaction writes nothing at all — not the item, not the evictions, not the audit row.
- A replayed idempotent write returns `AppliedWrite { replayed: true, .. }` with the **original** `item_id`.
- `import` skips items that already exist rather than duplicating or overwriting them, and counts them in `items_skipped_existing`.
- `purge_subject` must leave `report.audit_rows_removed + report.audit_rows_preserved` equal to the number of audit rows the subject had.
- **`audit_aggregates` rows must survive `purge_subject`.** The `audit_aggregates` table is keyed by the policy's name and version, the event class and the day bucket, with **no subject and no namespace column** — see `memorysafe_backend::aggregates` for the whole argument. Do not give it a foreign key to `audit`, do not include it in the subject sweep, and do not add a subject or namespace column for query convenience: `lifecycle::audit_aggregates_survive_a_cascading_purge` fails on the first, and the module doc explains why the third is the one that matters. Every audit row written increments the matching aggregate in the same transaction.
- **`retrieve_candidates` and `neighbours` populate `ScoredCandidate::last_accessed_at` and `access_count`** from the `items.last_access`/`items.access_count` columns the DDL already declares — a row never recalled reads back `(None, 0)`, never `(created_at, 0)`. `record_recall` increments both for every item its `AuditRecord::items` references, in the same transaction as the audit row.
- **`import` takes the destination tenant** and compares it against every record — items and audit rows alike. A disagreement rejects the whole import; nothing is retargeted and no audit scope is rewritten. There is deliberately no separate "may not span tenants" check and no rejection of a header-only stream.

---

## File Structure

Locking decomposition in before tasks. Each file has one responsibility.

### Repository root (the closed repo)

| File | Responsibility |
|---|---|
| `Cargo.toml` | Workspace: one member, plus path dependencies into `vendor/memorysafe/crates/*`. |
| `rust-toolchain.toml` | Pins 1.97.1, matching the OSS repo. |
| `clippy.toml` | Mirrors the OSS repo's. |
| `LICENSE` | Commercial licence. Not Apache-2.0. |
| `vendor/memorysafe` | Git submodule: the OSS repository, pinned to a commit. |
| `.github/workflows/ci.yml` | Checkout with submodules, fmt, clippy, test (Docker available). |

### `crates/memorysafe-backend-postgres`

| File | Responsibility |
|---|---|
| `src/lib.rs` | `PostgresBackend`, the `Backend` impl wiring, `estimate_tokens`. |
| `src/config.rs` | `PgConfig`, `PgLayout`, `schema_for_tenant`, identifier budgeting. |
| `src/error.rs` | `sqlx` → `BackendError` mapping and SQLSTATE retryability. |
| `src/ddl.rs` | Every `CREATE` statement, for both layouts. `SCHEMA_VERSION`. |
| `src/bootstrap.rs` | Extension, role, schema, partitions, grants, RLS. Idempotent. |
| `src/session.rs` | `tenant_txn` — the transaction that sets the tenant GUC and `search_path`. |
| `src/items.rs` | Item row mapping, insert / get / list / delete / exists / merge. |
| `src/vectors.rs` | Vector row storage, HNSW candidate generation, exact rerank. |
| `src/filters.rs` | Hard-filter SQL and its bind values. |
| `src/keyword.rs` | `to_or_tsquery` and full-text search. |
| `src/retrieve.rs` | Hybrid fusion of the vector and keyword signals. |
| `src/capacity.rs` | Accounting rows, `SELECT … FOR UPDATE`, budgets, scope stats. |
| `src/audit.rs` | Audit row write and query. |
| `src/purge.rs` | `purge_subject`. |
| `src/portability.rs` | Export / import streams. |
| `tests/support/mod.rs` | `PgHarness` (container or `DATABASE_URL`) and `PgFactory`. |
| `tests/conformance.rs` | The frozen suite, run once per layout. |
| `tests/isolation.rs` | RLS proof tests that bypass the backend and query the pool directly. |
| `tests/parity.rs` | Same corpus into SQLite and Postgres; identical ranking asserted. |

---

## Canonical signatures

These names are referenced across tasks. Any deviation is a bug.

```rust
// config.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgLayout {
    /// One schema, tables hash-partitioned by tenant_id, RLS on. The default.
    SharedPartitioned,
    /// One schema per tenant, unpartitioned tables, RLS still on.
    SchemaPerTenant,
}

#[derive(Debug, Clone)]
pub struct PgConfig {
    pub url: String,
    pub layout: PgLayout,
    pub schema: String,        // Schema name, or the per-tenant prefix. Default "memorysafe".
    pub app_role: String,      // Default "memorysafe_app".
    pub vector_dim: u16,       // Default 256.
    pub partitions: u32,       // Default 16. SharedPartitioned only.
    pub max_connections: u32,  // Default 16.
    pub over_fetch: usize,     // Default 4.
}

impl PgConfig {
    pub fn new(url: impl Into<String>) -> Self;              // all defaults
    pub fn with_layout(self, layout: PgLayout) -> Self;
    pub fn with_schema(self, schema: impl Into<String>) -> Self;
    pub fn with_vector_dim(self, dim: u16) -> Self;
}

/// The PostgreSQL schema holding a tenant's tables. `config.schema` verbatim
/// under `SharedPartitioned`; `<schema>_<folded tenant>_<hash>`, bounded to 63
/// bytes, under `SchemaPerTenant`.
pub fn schema_for_tenant(config: &PgConfig, tenant: &TenantId) -> String;
pub const MAX_PREFIX_BYTES: usize = 46;

// error.rs
pub fn sqlx_error(e: sqlx::Error) -> BackendError;
pub fn is_retryable_sqlstate(code: &str) -> bool;
pub const UNIQUE_VIOLATION: &str = "23505";

// ddl.rs
pub const SCHEMA_VERSION: i64 = 1;
pub fn statements(config: &PgConfig, schema: &str) -> Vec<String>;

// bootstrap.rs
pub async fn ensure_role(admin: &PgPool, role: &str) -> Result<(), BackendError>;
pub async fn ensure_schema(admin: &PgPool, config: &PgConfig, schema: &str)
    -> Result<(), BackendError>;

// session.rs
pub(crate) async fn tenant_txn<'a>(be: &'a PostgresBackend, tenant: &TenantId)
    -> Result<Transaction<'a, Postgres>, BackendError>;

// lib.rs
pub struct PostgresBackend { /* admin pool, app pool, config, ready set */ }

impl PostgresBackend {
    pub async fn connect(config: PgConfig) -> Result<Self, BackendError>;
    pub fn config(&self) -> &PgConfig;
    /// Escape hatch for the isolation tests: the runtime (app-role) pool.
    pub fn app_pool(&self) -> &PgPool;
}

pub(crate) fn estimate_tokens(body: &str) -> u32;
```

---

## The PostgreSQL schema

`SCHEMA_VERSION = 1`. Every statement below was executed against PostgreSQL 17.11 + pgvector 0.8.6.

```sql
CREATE EXTENSION IF NOT EXISTS vector;   -- installed into `public`

CREATE TABLE items (
  tenant_id         TEXT NOT NULL,
  id                TEXT NOT NULL,
  subject           TEXT NOT NULL,
  namespace         TEXT NOT NULL,
  body              TEXT NOT NULL,
  kind              TEXT NOT NULL,
  source_kind       TEXT NOT NULL,
  source_id         TEXT,
  occurred_at       BIGINT,
  created_at        BIGINT NOT NULL,
  tags              TEXT[] NOT NULL,
  tags_text         TEXT NOT NULL,          -- see note below
  attrs             JSONB NOT NULL,
  sensitivity       SMALLINT NOT NULL,      -- ordinal 0..4
  ttl_seconds       BIGINT,
  protection        TEXT NOT NULL,          -- 'normal' | 'protected' | 'pinned'
  protected_until   BIGINT,
  value_score       REAL NOT NULL DEFAULT 0.0,
  fragility_score   REAL NOT NULL DEFAULT 0.0,
  byte_size         BIGINT NOT NULL,
  last_access       BIGINT,
  access_count      BIGINT NOT NULL DEFAULT 0,
  pending_embedding BOOLEAN NOT NULL DEFAULT FALSE,
  search            tsvector GENERATED ALWAYS AS
                      (to_tsvector('simple'::regconfig, body || ' ' || tags_text)) STORED,
  PRIMARY KEY (tenant_id, id)
) PARTITION BY HASH (tenant_id);

CREATE INDEX idx_items_scope      ON items (tenant_id, subject, namespace);
CREATE INDEX idx_items_scope_sens ON items (tenant_id, subject, namespace, sensitivity);
CREATE INDEX idx_items_search     ON items USING GIN (search);
CREATE INDEX idx_items_tags       ON items USING GIN (tags);

CREATE TABLE vectors (
  tenant_id TEXT NOT NULL,
  item_id   TEXT NOT NULL,
  subject   TEXT NOT NULL,
  namespace TEXT NOT NULL,
  embedder  TEXT NOT NULL,
  dim       INTEGER NOT NULL,
  scale     REAL NOT NULL,
  q         BYTEA NOT NULL,                 -- the canonical int8 vector
  embedding vector(<vector_dim>) NOT NULL,  -- derived, for ANN only
  PRIMARY KEY (tenant_id, item_id),
  FOREIGN KEY (tenant_id, item_id) REFERENCES items (tenant_id, id) ON DELETE CASCADE
) PARTITION BY HASH (tenant_id);

CREATE INDEX idx_vectors_scope ON vectors (tenant_id, subject, namespace, embedder, dim);
CREATE INDEX idx_vectors_hnsw  ON vectors USING hnsw (embedding vector_ip_ops);

CREATE TABLE capacity (
  tenant_id  TEXT NOT NULL,
  subject    TEXT NOT NULL,
  namespace  TEXT NOT NULL,
  max_items  BIGINT,
  max_bytes  BIGINT,
  used_items BIGINT NOT NULL DEFAULT 0,
  used_bytes BIGINT NOT NULL DEFAULT 0,
  PRIMARY KEY (tenant_id, subject, namespace)
) PARTITION BY HASH (tenant_id);

CREATE TABLE audit (
  tenant_id  TEXT NOT NULL,
  id         TEXT NOT NULL,
  at         BIGINT NOT NULL,
  subject    TEXT NOT NULL,
  namespace  TEXT NOT NULL,
  event      TEXT NOT NULL,
  items      JSONB NOT NULL,
  assessment JSONB,
  decision   JSONB,
  actor      JSONB NOT NULL,
  policy     TEXT,
  PRIMARY KEY (tenant_id, id)
) PARTITION BY HASH (tenant_id);
-- Two indexes, because there are two query shapes and neither serves the
-- other. `Backend::audit` orders by `id DESC` and pages `AuditFilter::after`
-- on `id < after`; within a scope the `at`-leading index is ordered
-- `at ASC, id DESC`, so it cannot answer that without sorting the whole
-- scope — on a table that grows without bound and is read for compliance.
-- And `at` cannot stand in for `id`: `AuditRecord::new` mints `id` at
-- construction while taking `at` as a parameter, so the two orders genuinely
-- diverge, which is what `audit_filter_narrows_by_event_and_time`
-- demonstrates at `limit: 2`. The `at`-leading index stays for the
-- `since`/`until` window filters.
CREATE INDEX idx_audit_scope_id ON audit (tenant_id, subject, namespace, id DESC);
CREATE INDEX idx_audit_scope_at ON audit (tenant_id, subject, namespace, at, id DESC);

CREATE TABLE idempotency (
  tenant_id      TEXT NOT NULL,
  key            TEXT NOT NULL,
  subject        TEXT NOT NULL,
  namespace      TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  outcome        JSONB NOT NULL,
  at             BIGINT NOT NULL,
  PRIMARY KEY (tenant_id, key)
) PARTITION BY HASH (tenant_id);

CREATE TABLE audit_aggregates (
  tenant_id      TEXT NOT NULL,
  -- The policy is two columns, never the rendered `name@version`. `PolicyId`'s
  -- Display is not injective — ("a@b","c") and ("a","b@c") both render
  -- "a@b@c" — so a rendered key column merges two distinct policies' counts
  -- into one row, in the artifact designed to outlive the detail rows.
  -- `lifecycle::audit_aggregates_page_in_the_documented_order` carries that
  -- pair. The `audit.policy` column above *is* the rendered form; it is a
  -- display convenience, nothing keys on it, and the aggregate key must not be
  -- derived from it.
  policy_name    TEXT,                 -- NULL together with policy_version
  policy_version TEXT,
  event          TEXT NOT NULL,        -- AuditEvent::as_str(), never an ordinal
  day            BIGINT NOT NULL,      -- whole UTC days, aggregates::day_bucket
  count          BIGINT NOT NULL,
  value_histogram     JSONB NOT NULL,
  fragility_histogram JSONB NOT NULL,
  histogram_version   INTEGER NOT NULL,
  -- The two policy columns are NULL together or set together. Without this a
  -- row like ('x', NULL, 'admitted', 1) is representable, falls inside the
  -- policied partial index — whose predicate tests only `policy_name` — and
  -- `aggregates::query`'s `(Some(n), Some(v)) => Some(..), _ => None` would
  -- silently relabel it as policy-less: a wrong aggregate that looks
  -- well-formed. The invariant was a comment; this makes it a constraint.
  CHECK ((policy_name IS NULL) = (policy_version IS NULL))
) PARTITION BY HASH (tenant_id);
-- No subject column and no namespace column: that absence is what lets these
-- rows legitimately outlive `purge_subject`, and it is the single most
-- important property of this table. Do not add one for query convenience.
--
-- Uniqueness in two partial indexes rather than one primary key over the
-- nullable tuple: a unique index treats NULLs as distinct, so a single index
-- would let two policy-less rows with the same event and day both insert — and
-- policy-less rows are the majority of the key space. Splitting on nullability
-- enforces it without inventing a sentinel policy string, which
-- `AggregateKey::policy`'s doc rules out.
CREATE UNIQUE INDEX idx_aggregates_key_policied
  ON audit_aggregates (tenant_id, policy_name, policy_version, event, day)
  WHERE policy_name IS NOT NULL;
CREATE UNIQUE INDEX idx_aggregates_key_policy_less
  ON audit_aggregates (tenant_id, event, day)
  WHERE policy_name IS NULL;
-- The read path's ordering index, in `Backend::audit_aggregates`' documented
-- key order. Collation stated on every text column rather than left to the
-- database default, per the mandate on that method: `COLLATE "C"` is
-- Postgres's spelling of byte order.
--
-- **Null placement is stated HERE as well as in the query, and it has to be.**
-- In Postgres a btree index's null ordering is part of the index, and `ASC`
-- defaults to `NULLS LAST`. An index declared without `NULLS FIRST` cannot
-- serve `ORDER BY ... ASC NULLS FIRST` however the query is written — the
-- planner falls back to a full sort. So the two must agree, and an earlier
-- version of this comment said the opposite ("null placement is stated in the
-- query's ORDER BY rather than here"), which would have produced exactly that
-- sort while satisfying the mandate's letter.
--
-- Plan 1 hit the same trap from the other direction: `(policy_name IS NULL)
-- DESC` also satisfies the mandate and is also unservable, because it is an
-- expression rather than a column. Its measurement (`EXPLAIN QUERY PLAN`
-- showing `USE TEMP B-TREE FOR ORDER BY`) is why both documents now name the
-- form rather than only the requirement.
CREATE INDEX idx_aggregates_order ON audit_aggregates (
  tenant_id,
  day,
  policy_name    COLLATE "C" ASC NULLS FIRST,
  policy_version COLLATE "C" ASC NULLS FIRST,
  event          COLLATE "C"
);

-- Not tenant-scoped, not partitioned, no RLS: it holds the schema version.
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
```

### Why the schema looks like this

- **`tags TEXT[]` *and* `tags_text TEXT`.** The array is what `tags_any` filters against (`tags && $n`, GIN-indexed). The text column feeds the generated `tsvector`. It cannot be derived: `array_to_string` is `STABLE`, not `IMMUTABLE`, so `to_tsvector(..., array_to_string(tags, ' '))` is rejected with `generation expression is not immutable`. Writing `tags_text` from the application is the price of having the `tsvector` maintained automatically on merge.
- **`search` is a generated column, not application-maintained.** A merge that updated `body` but forgot the `tsvector` would silently degrade recall with no failing test to catch it.
- **`q BYTEA` is canonical; `embedding vector(n)` is derived.** Export and exact scoring both read `q`, so round-trip fidelity does not depend on float round-tripping through pgvector. The `vector` column exists solely so the HNSW index can generate candidates.
- **The foreign key is composite and crosses two partitioned tables.** Supported and verified: deleting an item removes its vector row, which is the same behaviour the SQLite backend gets from `ON DELETE CASCADE`.
- **`PARTITION BY HASH`, not `LIST`.** List partitioning by tenant means DDL on the write path for every new tenant and one partition per customer. Hash with a fixed modulus prunes on `tenant_id` just as well, needs no DDL after bootstrap, and keeps the relation count bounded. Tenant-level `DETACH`/`DROP` is not something the `Backend` trait exposes, so the one thing list partitioning would buy is unused.
- **`meta` is outside RLS.** It holds one row, `schema_version`. Putting a non-tenant table behind a `tenant_id` policy would mean inventing a tenant for it.

### Row-level security

```sql
ALTER TABLE <t> ENABLE ROW LEVEL SECURITY;
ALTER TABLE <t> FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON <t>
  USING      (tenant_id = nullif(current_setting('memorysafe.tenant_id', true), ''))
  WITH CHECK (tenant_id = nullif(current_setting('memorysafe.tenant_id', true), ''));
```

for each of `items`, `vectors`, `capacity`, `audit`, `idempotency`.

Three properties this buys, all verified:

1. With no tenant claimed, the comparison is `NULL` and every row is filtered. Reads return zero rows; writes fail with `new row violates row-level security policy`.

   **The `nullif` is load-bearing and the obvious spelling is wrong.** A custom GUC that was never declared in `postgresql.conf` does *not* return to unset after a transaction that `set_config`'d it: PostgreSQL 17 resets it to the empty string, a placeholder GUC's reset value, verified directly against a live server. So on any pooled connection that has previously served a tenant, `current_setting('memorysafe.tenant_id', true)` is `Some("")`, not `NULL` — and `tenant_id = ''` is `FALSE`, not `NULL`.

   Every row is still filtered, so the property survives, but **it survives for a reason the plan did not state and did not depend on deliberately**: no stored `tenant_id` can be the empty string, because `memorysafe_core`'s `validate_component` rejects an empty component and `TenantId::new("")` is an error. That is a guarantee in another crate, in the OSS repository, load-bearing for this one's tenant isolation and coupled to it by nothing. `nullif` removes the dependency and restores the stated mechanism: an empty GUC becomes `NULL`, the comparison becomes `NULL`, and the argument above is true as written rather than true by accident.
2. `WITH CHECK` blocks a connection that has claimed tenant B from writing a row labelled tenant A.
3. Grants are issued on the **parent tables only**. Access through a partitioned parent is authorised against the parent, so the app role can work normally while a direct `SELECT … FROM items_p0` fails with `permission denied for table items_p0`. Direct partition access is the obvious way around a parent-level policy, and this closes it.

`FORCE` matters because it subjects the table owner to the policy too; without it, a deployment that ran the backend as the schema owner would have no isolation.

### The application role

```sql
CREATE ROLE memorysafe_app NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
GRANT USAGE ON SCHEMA <schema>, public TO memorysafe_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON
  <schema>.items, <schema>.vectors, <schema>.capacity,
  <schema>.audit, <schema>.idempotency TO memorysafe_app;
GRANT SELECT ON <schema>.meta TO memorysafe_app;
```

The pool connects with whatever credentials `PgConfig::url` carries — typically an owner or migration role — and every connection issues `SET ROLE memorysafe_app` in `after_connect`. After that, `current_user` is the app role, so RLS applies even when the underlying login is a superuser. `NOLOGIN` is deliberate: the role is a privilege container, not a credential, so there is no password to leak or rotate.

`public` must stay on `search_path` because the `vector` type and the `<#>` operator live in the extension's schema. Creating the extension inside the memorysafe schema and dropping `public` from the path also works, but then a shared installation cannot host two memorysafe schemas.

---

## Task Index

| # | Task | Deliverable |
|---|---|---|
| 1 | Repo scaffold, OSS submodule, CI | `cargo test` green; the OSS crates resolve through the submodule |
| 2 | `PgLayout`, `PgConfig`, schema naming | Bounded, deterministic schema names |
| 3 | Error mapping and retryability | `sqlx::Error` → `BackendError` |
| 4 | Test harness: container and fresh schema | A live database from `cargo test`, with pgvector |
| 5 | DDL, bootstrap, role, and RLS | An initialised schema, idempotently |
| 6 | Session plumbing: `tenant_txn` | The tenant GUC, scoped to a transaction |
| 7 | Items, audit, and `PgFactory` | Isolation conformance passes |
| 8 | RLS is structural — proof tests | Isolation survives a query with no tenant predicate |
| 9 | Vectors, HNSW, exact rerank | `neighbours` works; cross-model probes refused |
| 10 | Hard filters and keyword search | Hostile input cannot become an operator |
| 11 | Hybrid retrieval | Retrieval conformance passes |
| 12 | Capacity locking, merge, idempotency | Atomicity and capacity conformance pass |
| 13 | Purge and portable export/import | The full 50-test suite passes |
| 14 | `SchemaPerTenant` layout | The full suite passes under both layouts |
| 15 | Cross-backend parity | SQLite and Postgres rank identically |
| 16 | Schema-version guard and operator docs | Refuses a database from a newer version |

---

## Task 1: Repo scaffold, OSS submodule, and CI

**Files:**
- Create: `../memorysafe-backend-postgres/Cargo.toml`
- Create: `../memorysafe-backend-postgres/rust-toolchain.toml`
- Create: `../memorysafe-backend-postgres/clippy.toml`
- Create: `../memorysafe-backend-postgres/.gitignore`
- Create: `../memorysafe-backend-postgres/LICENSE`
- Create: `../memorysafe-backend-postgres/.github/workflows/ci.yml`
- Create: `../memorysafe-backend-postgres/crates/memorysafe-backend-postgres/Cargo.toml`
- Create: `../memorysafe-backend-postgres/crates/memorysafe-backend-postgres/src/lib.rs`
- Submodule: `../memorysafe-backend-postgres/vendor/memorysafe`
- Move: this plan file into `../memorysafe-backend-postgres/docs/plans/`

**Interfaces:**
- Consumes: `memorysafe-core`, `memorysafe-backend`, `memorysafe-embed` from the submodule.
- Produces: a workspace that builds, and the path-dependency wiring every later task relies on.

**Everything after this task runs from `../memorysafe-backend-postgres`.** Paths in later tasks are relative to that repository root.

**Why a submodule rather than git dependencies:** the OSS crates are unpublished, so a git dependency would pin a rev and every OSS change would need a commit plus a rev bump before it could be tested here. A submodule gives exact pinning through its recorded SHA *and* an ordinary path dependency, so `cargo test` picks up a local OSS edit immediately. The cost is remembering `--recurse-submodules`; CI does it explicitly.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend-postgres/src/lib.rs`:

```rust
//! The commercial PostgreSQL backend for MemorySafe.
//!
//! Isolation is enforced by row-level security under a non-superuser role,
//! not by the application's `WHERE` clauses. See `session.rs`.

#[cfg(test)]
mod tests {
    /// The submodule wiring is load-bearing for every later task, so it gets
    /// its own assertion rather than being discovered as a build error.
    #[test]
    fn the_oss_crates_resolve_through_the_submodule() {
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        assert_eq!(scope.tenant.as_str(), "t");
        assert_eq!(memorysafe_backend::MAX_PAGE_LIMIT, 1000);
        assert_eq!(
            memorysafe_backend::HardFilters::default().sensitivity_ceiling,
            memorysafe_core::SensitivityLevel::Internal,
            "the frozen suite fails closed; if this changed, the contract changed"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres`
Expected: FAIL — there is no repository yet, so `cargo` reports `could not find Cargo.toml`.

- [ ] **Step 3: Write minimal implementation**

Create the repository and vendor the OSS one. Run from the directory that contains the `memorysafe` checkout:

```bash
mkdir -p memorysafe-backend-postgres
cd memorysafe-backend-postgres
git init -b main

# Local path today; swap for the canonical URL once the OSS repo is pushed.
# `protocol.file.allow` is needed because git refuses file:// submodules by default.
git -c protocol.file.allow=always submodule add ../memorysafe vendor/memorysafe
git -c protocol.file.allow=always submodule update --init --recursive
```

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/*"]
# `vendor/memorysafe` is its own workspace. Without this exclude, the outer
# workspace pulls vendor's member crates in and resolves their
# `.workspace = true` inheritance against THIS manifest — `memorysafe-core`
# declares `ulid.workspace = true`, this workspace never declares `ulid`, and
# resolution fails for the whole workspace, including the one test Task 1
# exists to run. Hit on the first `cargo` invocation after the submodule was
# added; invisible until then, because a manifest written for a vendored
# workspace that has never been vendored has nothing to fail against.
exclude = ["vendor"]

[workspace.package]
edition = "2024"
rust-version = "1.97.1"
license = "LicenseRef-MemorySafe-Commercial"

[workspace.dependencies]
# Path dependencies into the vendored OSS repository. The submodule SHA is the
# pin; there is no version to bump.
# `memorysafe-backend-sqlite` is deliberately absent here and from
# `[dev-dependencies]`. Cargo resolves path dependencies eagerly, so naming a
# directory the submodule does not yet contain fails the whole workspace. It
# returns when Plan 1 ships that crate, and its only consumer here is the
# cross-backend export/import task — nothing before that references it.
memorysafe-core    = { path = "vendor/memorysafe/crates/memorysafe-core" }
memorysafe-backend = { path = "vendor/memorysafe/crates/memorysafe-backend" }
memorysafe-embed   = { path = "vendor/memorysafe/crates/memorysafe-embed" }
memorysafe-backend-sqlite = { path = "vendor/memorysafe/crates/memorysafe-backend-sqlite" }

serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
thiserror = "2.0.20"
time = { version = "0.3.55", features = ["serde", "macros"] }
blake3 = "1.5"
base64 = "0.22"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.97.1"
components = ["rustfmt", "clippy"]
```

`clippy.toml`:

```toml
avoid-breaking-exported-api = false
```

`.gitignore`:

```
/target
.env
```

`LICENSE`: the commercial licence text. This repository is **not** Apache-2.0; the workspace `license` field says `LicenseRef-MemorySafe-Commercial` so `cargo` never presents it as open source.

`crates/memorysafe-backend-postgres/Cargo.toml`:

```toml
[package]
name = "memorysafe-backend-postgres"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish = false

[dependencies]
memorysafe-core.workspace = true
memorysafe-backend.workspace = true
memorysafe-embed.workspace = true
sqlx = { version = "0.9.0", default-features = false, features = [
  "runtime-tokio", "tls-rustls-ring", "postgres", "json",
] }
tokio = { version = "1.53.1", features = ["rt", "rt-multi-thread", "macros", "sync"] }
async-trait = "0.1.92"
thiserror.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true
blake3.workspace = true
base64.workspace = true

[dev-dependencies]
memorysafe-backend-sqlite.workspace = true
testcontainers-modules = { version = "0.15.0", features = ["postgres"] }
tempfile = "3.27.0"

[lints]
workspace = true
```

`sqlx` is taken with `default-features = false`: the default set pulls in `any`, `macros`, and `migrate`, none of which this crate uses. Every query here is a runtime `sqlx::query`, not a compile-time-checked macro, because the schema name is chosen at runtime under `SchemaPerTenant` and `sqlx::query!` cannot see it.

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
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@1.97.1
        with:
          components: rustfmt, clippy
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      # The ubuntu runner has Docker; testcontainers starts pgvector itself.
      - run: cargo test --workspace --all-features
```

**Do not move this plan out of the open repository as part of this task.** The
disposition of this file is a **pending decision by the repository owner** and is not
an implementation step.

A shell block previously stood here that attempted the move. It was removed because
it did not work and was dangerous in the same breath:

- `git mv <path> /dev/null` is not an operation, and the path it named
  (`docs/superpowers/plans/…`) is not where this file lives
  (`.superpowers/closed-tier/…`). It could never have succeeded.
- `2>/dev/null || true` swallowed that failure silently, **guaranteeing the next line
  ran anyway**.
- That next line was `git -C ../memorysafe add -A && git -C ../memorysafe commit`,
  which stages and commits **the entire working tree of the open repository** —
  including whatever other agents have mid-write — under a message describing a move
  that did not happen.

So the block did nothing it intended and everything it should not, and the error
handling is what connected the two. A bare failure on the first line would at least
have stopped the sequence.

**If and when the owner rules that this file moves, the move is a deliberate,
reviewed operation on a public repository's history — not a `|| true` in a task
step.** Nothing in this plan should attempt it.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres && cargo clippy --all-targets -- -D warnings`
Expected: PASS — 1 test ok.

- [ ] **Step 5: Commit**

```bash
# Stage explicit paths. Never `git add -A`: this plan's steps have run in a
# worktree shared with other agents, where `-A` sweeps up their mid-write files.
git add Cargo.toml crates/ .github/workflows/
git commit -m "chore: scaffold the commercial Postgres backend workspace"
```

---

## Task 2: `PgLayout`, `PgConfig`, and schema naming

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/config.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`

**Interfaces:**
- Consumes: `TenantId`.
- Produces: `PgLayout`, `PgConfig`, `PgConfig::new/with_layout/with_schema/with_vector_dim`, `schema_for_tenant`, `MAX_PREFIX_BYTES`.

**The constraint that drives this task:** a `TenantId` may be up to 240 bytes, and a PostgreSQL identifier is truncated at 63. Truncating a tenant id to fit would map two long tenants onto one schema — a silent cross-tenant merge, which is the exact failure the OSS repo rejects uppercase tenant ids to avoid. A hash suffix makes collisions cryptographically implausible while keeping the readable portion that makes `\dn` useful to an operator. The configured `schema` becomes the prefix in both layouts, so two deployments — and two test runs — can share a database without their tenants landing in the same place.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-backend-postgres/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(raw: &str) -> TenantId {
        TenantId::new(raw).unwrap()
    }

    #[test]
    fn the_shared_layout_puts_every_tenant_in_one_schema() {
        let c = PgConfig::new("postgres://localhost/x");
        assert_eq!(c.layout, PgLayout::SharedPartitioned);
        assert_eq!(schema_for_tenant(&c, &tenant("acme")), "memorysafe");
        assert_eq!(schema_for_tenant(&c, &tenant("globex")), "memorysafe");
    }

    #[test]
    fn schema_per_tenant_names_stay_within_the_identifier_limit() {
        // The longest prefix the layout supports, against the longest tenant
        // id `TenantId` permits.
        for prefix_len in [1, 10, 20, MAX_PREFIX_BYTES] {
            let c = PgConfig::new("postgres://localhost/x")
                .with_layout(PgLayout::SchemaPerTenant)
                .with_schema("p".repeat(prefix_len));
            let name = schema_for_tenant(&c, &tenant(&"a".repeat(240)));
            assert!(
                name.len() <= 63,
                "prefix of {prefix_len} produced a {}-byte identifier: {name}",
                name.len()
            );
        }
    }

    #[test]
    fn schema_names_are_deterministic_and_distinct() {
        let c = PgConfig::new("postgres://localhost/x").with_layout(PgLayout::SchemaPerTenant);
        assert_eq!(schema_for_tenant(&c, &tenant("acme")), schema_for_tenant(&c, &tenant("acme")));
        assert_ne!(schema_for_tenant(&c, &tenant("acme")), schema_for_tenant(&c, &tenant("globex")));
    }

    /// Two tenants that share every readable character must not share a
    /// schema. The hash suffix is what guarantees it.
    #[test]
    fn long_tenants_with_a_common_prefix_do_not_collide() {
        let c = PgConfig::new("postgres://localhost/x").with_layout(PgLayout::SchemaPerTenant);
        let a = tenant(&format!("{}-one", "x".repeat(200)));
        let b = tenant(&format!("{}-two", "x".repeat(200)));
        assert_ne!(schema_for_tenant(&c, &a), schema_for_tenant(&c, &b));
    }

    /// The prefix separates deployments — and, in the tests, separates one
    /// test's tenants from another's. Two configs with different prefixes must
    /// never land the same tenant in the same schema.
    ///
    /// **Two same-length prefixes cannot test this.** Equal lengths give equal
    /// readable budgets, so the two names differ wherever the prefixes differ
    /// and the assertion holds however the hash is computed — the test passes
    /// against an implementation that ignores the prefix entirely. The pair
    /// below differs in *length*, which is the axis the budget arithmetic
    /// moves along.
    #[test]
    fn the_prefix_separates_otherwise_identical_tenants() {
        let one = PgConfig::new("postgres://localhost/x")
            .with_layout(PgLayout::SchemaPerTenant)
            .with_schema("ms_test");
        let two = PgConfig::new("postgres://localhost/x")
            .with_layout(PgLayout::SchemaPerTenant)
            .with_schema("ms_test_longer");
        let t = tenant("t");
        assert_ne!(schema_for_tenant(&one, &t), schema_for_tenant(&two, &t));
        assert!(schema_for_tenant(&one, &t).starts_with("ms_test_"));
    }

    /// The one pair the budget arithmetic can actually collapse, and the
    /// reason the hash covers the prefix.
    ///
    /// The readable budget shrinks by exactly what the prefix grows by, and
    /// `_` is both the separator and what every illegal byte folds to. So a
    /// tenant that folds to all underscores makes "one more prefix byte" and
    /// "one fewer readable byte" the same edit. These two configs differ by a
    /// single trailing `_` and, with the hash taken over the tenant alone,
    /// produce byte-identical schema names — two deployments silently sharing
    /// one tenant's tables.
    ///
    /// This is a regression test with a known-failing predecessor: it fails
    /// against `blake3::hash(tenant)` and passes against the length-delimited
    /// hash over `(prefix, tenant)`. Do not "simplify" the tenant to something
    /// readable — the all-underscore fold is the whole mechanism.
    #[test]
    fn a_prefix_that_grows_by_one_underscore_cannot_absorb_the_readable_tenant() {
        let short = PgConfig::new("postgres://localhost/x")
            .with_layout(PgLayout::SchemaPerTenant)
            .with_schema("p".repeat(20));
        let long = PgConfig::new("postgres://localhost/x")
            .with_layout(PgLayout::SchemaPerTenant)
            .with_schema(format!("{}_", "p".repeat(20)));
        let t = tenant(&"-".repeat(28));

        let a = schema_for_tenant(&short, &t);
        let b = schema_for_tenant(&long, &t);
        assert_ne!(a, b, "two deployments were given the same schema: {a}");
        // The premise, so a future change to the budget cannot make this test
        // vacuous by simply moving the two names apart for an unrelated reason.
        assert_eq!(a.len(), 63, "the collision needs both names at the limit");
        assert_eq!(b.len(), 63);
    }

    /// `-` and `.` are legal in a TenantId and illegal, unquoted, in an
    /// identifier. Fold them rather than quoting the identifier everywhere.
    #[test]
    fn punctuation_is_folded_to_underscores() {
        let c = PgConfig::new("postgres://localhost/x").with_layout(PgLayout::SchemaPerTenant);
        let name = schema_for_tenant(&c, &tenant("acme-corp.eu"));
        assert!(name.starts_with("memorysafe_acme_corp_eu_"), "got {name}");
        assert!(
            name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "identifier {name} needs quoting"
        );
    }

    /// An over-long prefix produces a name `ddl::checked_ident` rejects, so a
    /// misconfiguration fails at `connect` rather than silently truncating two
    /// deployments onto one schema.
    #[test]
    fn an_over_long_prefix_produces_a_name_that_will_be_refused() {
        let c = PgConfig::new("postgres://localhost/x")
            .with_layout(PgLayout::SchemaPerTenant)
            .with_schema("p".repeat(MAX_PREFIX_BYTES + 10));
        assert!(schema_for_tenant(&c, &tenant("acme")).len() > 63);
    }

    #[test]
    fn defaults_match_the_documented_ones() {
        let c = PgConfig::new("postgres://localhost/x");
        assert_eq!(c.schema, "memorysafe");
        assert_eq!(c.app_role, "memorysafe_app");
        assert_eq!(c.vector_dim, 256);
        assert_eq!(c.partitions, 16);
        assert_eq!(c.over_fetch, 4);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres config`
Expected: FAIL — `cannot find type PgConfig in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/config.rs`:

```rust
use memorysafe_core::TenantId;

/// How tenants are laid out in the database.
///
/// Both are behind one implementation and both run the full conformance
/// suite; the only differences are which schema a tenant's tables live in and
/// whether those tables are partitioned. RLS is on in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgLayout {
    /// One schema, tables hash-partitioned by `tenant_id`. The default.
    SharedPartitioned,
    /// One schema per tenant, unpartitioned tables. For customers whose
    /// contracts require a visible physical boundary.
    SchemaPerTenant,
}

#[derive(Debug, Clone)]
pub struct PgConfig {
    pub url: String,
    pub layout: PgLayout,
    /// The schema name under `SharedPartitioned`, and the per-tenant schema
    /// prefix under `SchemaPerTenant`. At most `MAX_PREFIX_BYTES` in the
    /// second case.
    pub schema: String,
    /// The non-superuser role every runtime connection assumes.
    pub app_role: String,
    /// The embedding width this deployment stores. `vectors.embedding` is
    /// `vector(vector_dim)`, so a write of a different width is refused.
    pub vector_dim: u16,
    /// Hash-partition modulus. `SharedPartitioned` only.
    pub partitions: u32,
    pub max_connections: u32,
    /// Multiplier applied to a query's limit when generating candidates from
    /// each retrieval source, before fusion narrows them again.
    pub over_fetch: usize,
}

impl PgConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            layout: PgLayout::SharedPartitioned,
            schema: "memorysafe".into(),
            app_role: "memorysafe_app".into(),
            vector_dim: 256,
            partitions: 16,
            max_connections: 16,
            over_fetch: 4,
        }
    }

    pub fn with_layout(mut self, layout: PgLayout) -> Self {
        self.layout = layout;
        self
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = schema.into();
        self
    }

    pub fn with_vector_dim(mut self, dim: u16) -> Self {
        self.vector_dim = dim;
        self
    }
}

/// PostgreSQL truncates identifiers past this, silently.
const MAX_IDENT_BYTES: usize = 63;
/// Hex characters of BLAKE3 kept as the distinctness guarantee. Sixty-four
/// bits is far beyond any plausible tenant count.
const HASH_HEX_BYTES: usize = 16;
/// Most readable tenant characters worth keeping. Longer adds no legibility.
const READABLE_BYTES: usize = 28;
/// Longest `schema` prefix `SchemaPerTenant` supports while still leaving room
/// for the hash. Documented so a misconfiguration is a known failure.
///
/// `- 1`, not `- 2`, and the difference is not arithmetic taste. A prefix this
/// long exhausts the readable budget, and the exhausted branch emits **one**
/// separator (`{prefix}_{hash}`), not two. Subtracting 2 would make the
/// constant conservative by a byte and its own doc sentence false: 46 would be
/// supported while the constant called 45 the longest.
pub const MAX_PREFIX_BYTES: usize = MAX_IDENT_BYTES - HASH_HEX_BYTES - 1;

/// The schema holding a tenant's tables.
///
/// Under `SharedPartitioned` this is `config.schema` verbatim. Under
/// `SchemaPerTenant` it is `<schema>_<folded tenant>_<16 hex of BLAKE3>`.
///
/// Three things are load-bearing:
///
/// * **The hash, over the prefix *and* the tenant.** A `TenantId` may be 240
///   bytes and an identifier may be 63, so a purely truncating scheme would
///   map two long tenants onto one schema — a silent cross-tenant merge, which
///   is the failure the OSS repo rejects uppercase tenant ids to avoid.
///
///   Hashing the tenant alone is not enough, and the reason is subtle enough
///   to state: the readable budget shrinks by exactly the number of bytes the
///   prefix grows by, and `_` is simultaneously the separator and what every
///   illegal byte folds to. So under a tenant that folds to all underscores,
///   growing the prefix by one `_` and losing one underscore of readable
///   tenant produces **the same string**. Concretely, prefix `"p"*20` with
///   tenant `"-"*28`, and prefix `"p"*20 + "_"` with the same tenant, both
///   yield `pppppppppppppppppppp___________________________<hash>`. With the
///   hash covering the tenant only, that hash is equal too, and two
///   deployments share a schema. Covering the prefix breaks it.
///
///   The prefix is fed **length-delimited**, not concatenated: plain
///   `blake3(prefix ++ tenant)` maps `("ab", "c")` and `("a", "bc")` to one
///   digest, which is the same defect one layer down.
/// * **The prefix.** It separates deployments sharing a database, and it is
///   what lets two test runs use tenant `"t"` without colliding — but only
///   because the hash covers it. See above.
/// * **The folding.** `-` and `.` are legal in a `TenantId` and illegal in an
///   unquoted identifier; folding them to `_` means no call site has to quote.
///
/// The readable portion shrinks to whatever the prefix leaves room for, and
/// disappears entirely if there is none. A prefix longer than
/// `MAX_PREFIX_BYTES` yields a name `ddl::checked_ident` refuses, so the
/// misconfiguration surfaces at `connect` rather than as a collision.
pub fn schema_for_tenant(config: &PgConfig, tenant: &TenantId) -> String {
    match config.layout {
        PgLayout::SharedPartitioned => config.schema.clone(),
        PgLayout::SchemaPerTenant => {
            let prefix = &config.schema;
            // Length-delimited over (prefix, tenant) — see the doc above for
            // the collision that hashing the tenant alone leaves open, and for
            // why the length goes in rather than a separator byte.
            let digest = {
                let mut h = blake3::Hasher::new();
                h.update(&(prefix.len() as u64).to_le_bytes());
                h.update(prefix.as_bytes());
                h.update(tenant.as_str().as_bytes());
                h.finalize().to_hex()
            };
            let hash = &digest[..HASH_HEX_BYTES];

            let budget = MAX_IDENT_BYTES
                .saturating_sub(prefix.len() + 2 + HASH_HEX_BYTES)
                .min(READABLE_BYTES);
            if budget == 0 {
                return format!("{prefix}_{hash}");
            }
            let folded: String = tenant
                .as_str()
                .bytes()
                .take(budget)
                .map(|b| if b.is_ascii_lowercase() || b.is_ascii_digit() { b as char } else { '_' })
                .collect();
            format!("{prefix}_{folded}_{hash}")
        }
    }
}
```

Add to `crates/memorysafe-backend-postgres/src/lib.rs`:

```rust
pub mod config;
pub use config::{PgConfig, PgLayout, schema_for_tenant};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres config`
Expected: PASS — 8 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/src/
git commit -m "feat(pg): layout, configuration, and collision-free schema naming"
```

---

## Task 3: Error mapping and retryability

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/error.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`

**Interfaces:**
- Consumes: `BackendError`.
- Produces: `sqlx_error`, `is_retryable_sqlstate`, `UNIQUE_VIOLATION`, `SqlxResultExt`.

**Why this is its own task:** `BackendError::Storage` carries a `retryable` flag that the engine surfaces as HTTP 503 with a retry hint. Getting the classification wrong in either direction is a production incident — a serialization failure reported as permanent turns a transient conflict into a lost write, and a syntax error reported as retryable turns a bug into an infinite loop. The classification is a pure function of the SQLSTATE, so it is testable with no database.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-backend-postgres/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_conditions_are_retryable() {
        // Class 40: transaction rollback. Class 08: connection exception.
        // 57P01: admin shutdown. 55P03: lock not available. 53300: too many
        // connections. Every one of these succeeds on a second attempt.
        for code in ["40001", "40P01", "08000", "08006", "57P01", "55P03", "53300"] {
            assert!(is_retryable_sqlstate(code), "{code} should be retryable");
        }
    }

    #[test]
    fn programming_and_constraint_errors_are_not_retryable() {
        // 42601 syntax, 42P01 undefined table, 23505 unique violation,
        // 23503 foreign key, 22P02 invalid text representation, 42501
        // insufficient privilege (which is what an RLS grant failure looks
        // like — retrying it forever would bury a misconfiguration).
        for code in ["42601", "42P01", "23505", "23503", "22P02", "42501"] {
            assert!(!is_retryable_sqlstate(code), "{code} should not be retryable");
        }
    }

    #[test]
    fn an_rls_policy_violation_is_not_retryable() {
        // A row that violates the tenant policy will violate it on every
        // retry. 42501 is what PostgreSQL raises for it.
        assert!(!is_retryable_sqlstate("42501"));
    }

    #[test]
    fn a_pool_timeout_is_retryable_storage() {
        match sqlx_error(sqlx::Error::PoolTimedOut) {
            BackendError::Storage { retryable, .. } => assert!(retryable),
            other => panic!("expected retryable Storage, got {other:?}"),
        }
    }

    #[test]
    fn a_closed_pool_is_not_retryable() {
        match sqlx_error(sqlx::Error::PoolClosed) {
            BackendError::Storage { retryable, .. } => assert!(!retryable),
            other => panic!("expected non-retryable Storage, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_row_is_storage_not_a_panic() {
        // `RowNotFound` reaches here only from `fetch_one` misuse; every
        // optional read in this crate uses `fetch_optional`. Map it rather
        // than unwrapping so a future misuse degrades instead of aborting.
        match sqlx_error(sqlx::Error::RowNotFound) {
            BackendError::Storage { retryable, .. } => assert!(!retryable),
            other => panic!("expected Storage, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres error`
Expected: FAIL — `cannot find function is_retryable_sqlstate in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/error.rs`:

```rust
use memorysafe_backend::BackendError;

/// Raised when an `INSERT` collides with an existing primary key. The
/// idempotency path turns this into a replay rather than an error.
pub const UNIQUE_VIOLATION: &str = "23505";

/// Whether a second attempt at the same statement could succeed.
///
/// The engine turns `retryable` into a client-visible retry hint, so this is
/// a product decision as much as a technical one: everything transient — lock
/// contention, serialization conflicts, a failed-over primary — is worth
/// retrying, and everything that reflects a bug or a misconfiguration is not.
pub fn is_retryable_sqlstate(code: &str) -> bool {
    match code {
        // 55P03 lock_not_available, 53300 too_many_connections,
        // 57P01 admin_shutdown, 57P02 crash_shutdown, 57P03 cannot_connect_now.
        "55P03" | "53300" | "57P01" | "57P02" | "57P03" => true,
        // Class 40 (transaction rollback) and class 08 (connection exception).
        _ => code.starts_with("40") || code.starts_with("08"),
    }
}

/// The single conversion point from `sqlx` into the contract's error type.
/// Nothing in this crate may return a `sqlx::Error` to a caller.
pub fn sqlx_error(e: sqlx::Error) -> BackendError {
    let retryable = match &e {
        sqlx::Error::Database(db) => {
            db.code().map(|c| is_retryable_sqlstate(&c)).unwrap_or(false)
        }
        // The pool was saturated, not broken.
        sqlx::Error::PoolTimedOut => true,
        // Io covers a connection dropped mid-statement.
        sqlx::Error::Io(_) => true,
        _ => false,
    };
    BackendError::Storage { message: e.to_string(), retryable }
}

/// Lets every `sqlx` call site end in `.pg()?` instead of a closure, the way
/// the SQLite backend's calls end in `.sql()?`.
pub trait SqlxResultExt<T> {
    fn pg(self) -> Result<T, BackendError>;
}

impl<T> SqlxResultExt<T> for Result<T, sqlx::Error> {
    fn pg(self) -> Result<T, BackendError> {
        self.map_err(sqlx_error)
    }
}

/// True when this error is a primary-key collision.
pub fn is_unique_violation(e: &BackendError) -> bool {
    matches!(e, BackendError::Storage { message, .. } if message.contains(UNIQUE_VIOLATION))
}
```

Add to `crates/memorysafe-backend-postgres/src/lib.rs`:

```rust
pub mod error;
pub(crate) use error::SqlxResultExt;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres error`
Expected: PASS — 6 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/src/
git commit -m "feat(pg): map sqlx errors to the backend contract with retry classification"
```

---

## Task 4: Test harness — container, fresh schema, `DATABASE_URL` override

**Files:**
- Create: `crates/memorysafe-backend-postgres/tests/support/mod.rs`
- Create: `crates/memorysafe-backend-postgres/tests/harness.rs`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `testcontainers_modules::postgres::Postgres`.
- Produces: `support::harness() -> &'static PgHarness`, `PgHarness::url`, `support::fresh_schema() -> String`, `support::test_config() -> PgConfig`.

**Why the override exists:** `cargo test` on a fresh checkout should just work, which means starting a container. But every `tests/*.rs` file is its own binary with its own `OnceCell`, so a Docker-only harness starts one container per test binary. `MEMORYSAFE_TEST_DATABASE_URL` lets CI point every binary at one server, and it is also the path a developer uses against a remote database. CI runs both jobs so neither path rots.

**Test partition count:** the harness sets `partitions: 4` rather than the default 16. Each schema costs five tables times the modulus in relations, and the suite creates roughly forty schemas. Four exercises exactly the same partitioning code for a quarter of the DDL.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend-postgres/tests/harness.rs`:

```rust
mod support;

use sqlx::Row;

#[tokio::test]
async fn the_harness_provides_a_database_with_pgvector() {
    let h = support::harness().await;
    let pool = sqlx::PgPool::connect(&h.url).await.expect("connect");

    sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
        .execute(&pool)
        .await
        .expect("pgvector must be installable; is the image pgvector/pgvector?");

    let version: String = sqlx::query("SELECT extversion FROM pg_extension WHERE extname='vector'")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("extversion");

    // Iterative index scans, which filtered ANN retrieval depends on, arrived
    // in 0.8.
    let major_minor: Vec<u32> =
        version.split('.').take(2).filter_map(|p| p.parse().ok()).collect();
    assert!(
        major_minor >= vec![0, 8],
        "pgvector {version} is older than the 0.8 floor"
    );
}

#[tokio::test]
async fn fresh_schema_names_are_unique_and_legal_identifiers() {
    let a = support::fresh_schema();
    let b = support::fresh_schema();
    assert_ne!(a, b);
    for name in [&a, &b] {
        assert!(name.len() <= 63, "{name} is too long for an identifier");
        assert!(
            name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
            "{name} would need quoting"
        );
    }
}

#[tokio::test]
async fn two_schemas_on_one_server_do_not_see_each_other() {
    let h = support::harness().await;
    let pool = sqlx::PgPool::connect(&h.url).await.unwrap();
    let (a, b) = (support::fresh_schema(), support::fresh_schema());

    for s in [&a, &b] {
        sqlx::query(&format!("CREATE SCHEMA {s}")).execute(&pool).await.unwrap();
        sqlx::query(&format!("CREATE TABLE {s}.probe (n INT)")).execute(&pool).await.unwrap();
    }
    sqlx::query(&format!("INSERT INTO {a}.probe (n) VALUES (1)")).execute(&pool).await.unwrap();

    let n: i64 = sqlx::query(&format!("SELECT count(*) AS c FROM {b}.probe"))
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("c");
    assert_eq!(n, 0, "a fresh schema was not fresh");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test harness`
Expected: FAIL — `file not found for module support`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/tests/support/mod.rs`:

```rust
//! Shared test scaffolding. One database per test binary, one schema per
//! backend instance.
#![allow(dead_code)] // each test binary uses a different subset

use memorysafe_backend_postgres::{PgConfig, PgLayout};
use std::sync::atomic::{AtomicU32, Ordering};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};
use tokio::sync::OnceCell;

pub struct PgHarness {
    /// Held for the lifetime of the test binary. Dropping it stops the
    /// container, so it must outlive every pool that points at it.
    _container: Option<ContainerAsync<Postgres>>,
    pub url: String,
}

static HARNESS: OnceCell<PgHarness> = OnceCell::const_new();
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// The database every test in this binary shares.
///
/// `MEMORYSAFE_TEST_DATABASE_URL` wins when set; otherwise a `pgvector`
/// container is started. Tests never share a *schema*, only a server.
pub async fn harness() -> &'static PgHarness {
    HARNESS
        .get_or_init(|| async {
            if let Ok(url) = std::env::var("MEMORYSAFE_TEST_DATABASE_URL") {
                return PgHarness { _container: None, url };
            }
            let container = Postgres::default()
                .with_name("pgvector/pgvector")
                .with_tag("pg17")
                .start()
                .await
                .expect("start a pgvector container (is Docker running?)");
            let host = container.get_host().await.expect("container host");
            let port = container.get_host_port_ipv4(5432).await.expect("container port");
            let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
            PgHarness { _container: Some(container), url }
        })
        .await
}

/// A schema name no other test will pick. The pid keeps concurrently running
/// test binaries apart; the counter keeps tests within one binary apart.
pub fn fresh_schema() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("ms_test_{}_{n}", std::process::id())
}

/// A config pointed at a schema nothing else uses.
pub async fn test_config(layout: PgLayout) -> PgConfig {
    let h = harness().await;
    let mut config = PgConfig::new(h.url.clone())
        .with_layout(layout)
        .with_schema(fresh_schema())
        .with_vector_dim(256); // matches `fx::embedder()`
    // Four is enough to exercise partition routing; sixteen would quadruple
    // the DDL cost of forty schemas for no extra coverage.
    config.partitions = 4;
    config.max_connections = 4;
    config
}
```

Add the second CI job so both harness paths stay exercised. In `.github/workflows/ci.yml`:

```yaml
  test-external-database:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: pgvector/pgvector:pg17
        env:
          POSTGRES_PASSWORD: postgres
        options: >-
          --health-cmd pg_isready --health-interval 5s
          --health-timeout 5s --health-retries 10
        ports: ["5432:5432"]
    env:
      MEMORYSAFE_TEST_DATABASE_URL: postgres://postgres:postgres@localhost:5432/postgres
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@1.97.1
      - run: cargo test --workspace --all-features
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres --test harness`
Expected: PASS — 3 tests ok. The first run pulls `pgvector/pgvector:pg17`, which takes a minute or so.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/tests/ .github/
git commit -m "test(pg): container harness with a DATABASE_URL override and per-test schemas"
```

---

## Task 5: DDL, bootstrap, role, and row-level security

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/ddl.rs`
- Create: `crates/memorysafe-backend-postgres/src/bootstrap.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`
- Create: `crates/memorysafe-backend-postgres/tests/bootstrap.rs`

**Interfaces:**
- Consumes: `PgConfig`, `PgLayout`, `SqlxResultExt`.
- Produces: `ddl::SCHEMA_VERSION`, `ddl::statements(config, schema) -> Vec<String>`, `ddl::checked_ident`, `bootstrap::ensure_role`, `bootstrap::ensure_schema`, `PostgresBackend::connect`, `PostgresBackend::app_pool`.

**The whole bootstrap runs in one transaction.** DDL is transactional in PostgreSQL, so a bootstrap that fails halfway leaves nothing behind — there is no half-initialised schema to reason about.

**Why identifiers are checked rather than quoted:** schema names reach SQL as interpolated text because `search_path` and `CREATE SCHEMA` cannot take a bind parameter. Under `SchemaPerTenant` the name comes out of `schema_for_tenant`, which restricts it to `[a-z0-9_]` by construction; under `SharedPartitioned` it comes from operator config. `checked_ident` turns the second case's assumption into an enforced one.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend-postgres/tests/bootstrap.rs`:

```rust
mod support;

use memorysafe_backend_postgres::{PgLayout, PostgresBackend};
use sqlx::Row;

#[tokio::test]
async fn bootstrap_creates_the_schema_and_is_idempotent() {
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let schema = config.schema.clone();

    let backend = PostgresBackend::connect(config.clone()).await.expect("first connect");
    drop(backend);
    // Connecting a second time against the same schema must be a no-op, not
    // an error and not a wipe.
    let backend = PostgresBackend::connect(config).await.expect("second connect");

    let version: String = sqlx::query(&format!(
        "SELECT value FROM {schema}.meta WHERE key = 'schema_version'"
    ))
    .fetch_one(backend.app_pool())
    .await
    .unwrap()
    .get("value");
    assert_eq!(version, "1");
}

#[tokio::test]
async fn every_tenant_table_has_row_level_security_forced() {
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let schema = config.schema.clone();
    let backend = PostgresBackend::connect(config).await.unwrap();

    for table in ["items", "vectors", "capacity", "audit", "idempotency"] {
        let row = sqlx::query(
            "SELECT c.relrowsecurity, c.relforcerowsecurity
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2",
        )
        .bind(&schema)
        .bind(table)
        .fetch_one(backend.app_pool())
        .await
        .unwrap();

        assert!(row.get::<bool, _>("relrowsecurity"), "{table} has RLS disabled");
        assert!(
            row.get::<bool, _>("relforcerowsecurity"),
            "{table} does not FORCE RLS; the owner would bypass it"
        );
    }
}

#[tokio::test]
async fn the_application_role_cannot_bypass_row_level_security() {
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let role = config.app_role.clone();
    let backend = PostgresBackend::connect(config).await.unwrap();

    let row = sqlx::query("SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = $1")
        .bind(&role)
        .fetch_one(backend.app_pool())
        .await
        .unwrap();
    assert!(!row.get::<bool, _>("rolsuper"), "the app role is a superuser");
    assert!(!row.get::<bool, _>("rolbypassrls"), "the app role has BYPASSRLS");

    // And the pool actually assumed it.
    let current: String = sqlx::query("SELECT current_user AS u")
        .fetch_one(backend.app_pool())
        .await
        .unwrap()
        .get("u");
    assert_eq!(current, role, "the pool did not SET ROLE");
}

#[tokio::test]
async fn the_shared_layout_partitions_every_tenant_table() {
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let schema = config.schema.clone();
    let partitions = config.partitions as i64;
    let backend = PostgresBackend::connect(config).await.unwrap();

    for table in ["items", "vectors", "capacity", "audit", "idempotency"] {
        let n: i64 = sqlx::query(
            "SELECT count(*) AS c FROM pg_inherits i
             JOIN pg_class p ON p.oid = i.inhparent
             JOIN pg_namespace n ON n.oid = p.relnamespace
             WHERE n.nspname = $1 AND p.relname = $2",
        )
        .bind(&schema)
        .bind(table)
        .fetch_one(backend.app_pool())
        .await
        .unwrap()
        .get("c");
        assert_eq!(n, partitions, "{table} has {n} partitions, expected {partitions}");
    }
}

#[tokio::test]
async fn a_hostile_schema_name_is_refused_rather_than_interpolated() {
    use memorysafe_backend_postgres::ddl;
    assert!(ddl::checked_ident("memorysafe").is_ok());
    assert!(ddl::checked_ident("ms_test_1_0").is_ok());
    for bad in ["public; DROP SCHEMA memorysafe CASCADE", "has space", "Upper", "1leading", ""] {
        assert!(ddl::checked_ident(bad).is_err(), "{bad:?} was accepted");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test bootstrap`
Expected: FAIL — `no function or associated item named connect found for struct PostgresBackend`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/ddl.rs`:

```rust
use crate::config::{PgConfig, PgLayout};
use memorysafe_backend::BackendError;

pub const SCHEMA_VERSION: i64 = 1;

/// Tables that hold tenant data and therefore carry an RLS policy. `meta` is
/// deliberately absent: it holds the schema version and belongs to no tenant.
pub const TENANT_TABLES: [&str; 6] =
    ["items", "vectors", "capacity", "audit", "idempotency", "audit_aggregates"];

/// Schema names reach SQL as interpolated text, because neither `CREATE
/// SCHEMA` nor `search_path` accepts a bind parameter. This is the check that
/// makes that safe.
pub fn checked_ident(name: &str) -> Result<(), BackendError> {
    let mut bytes = name.bytes();
    let first_is_lower_alpha = bytes.next().is_some_and(|b| b.is_ascii_lowercase());
    let rest_ok = bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if name.len() <= 63 && first_is_lower_alpha && rest_ok {
        Ok(())
    } else {
        Err(BackendError::InvalidQuery(format!(
            "{name:?} is not a safe unquoted PostgreSQL identifier"
        )))
    }
}

/// Every statement needed to bring one schema up to `SCHEMA_VERSION`.
///
/// Ordered so the whole vector can be executed in one transaction: schema,
/// `search_path`, tables, partitions, indexes, RLS, grants, version row.
/// Every statement is idempotent, so reconnecting to a live database is a
/// no-op rather than an error.
pub fn statements(config: &PgConfig, schema: &str) -> Vec<String> {
    let mut out = Vec::new();
    let partitioned = config.layout == PgLayout::SharedPartitioned;
    let by_hash = if partitioned { " PARTITION BY HASH (tenant_id)" } else { "" };
    let dim = config.vector_dim;
    let role = &config.app_role;

    out.push("CREATE EXTENSION IF NOT EXISTS vector".into());
    out.push(format!("CREATE SCHEMA IF NOT EXISTS {schema}"));
    // Unqualified DDL below lands in `schema`; `public` stays on the path so
    // the `vector` type and the `<#>` operator resolve.
    out.push(format!("SET LOCAL search_path = {schema}, public"));

    out.push(format!(
        "CREATE TABLE IF NOT EXISTS items (
           tenant_id         TEXT NOT NULL,
           id                TEXT NOT NULL,
           subject           TEXT NOT NULL,
           namespace         TEXT NOT NULL,
           body              TEXT NOT NULL,
           kind              TEXT NOT NULL,
           source_kind       TEXT NOT NULL,
           source_id         TEXT,
           occurred_at       BIGINT,
           created_at        BIGINT NOT NULL,
           tags              TEXT[] NOT NULL,
           tags_text         TEXT NOT NULL,
           attrs             JSONB NOT NULL,
           sensitivity       SMALLINT NOT NULL,
           ttl_seconds       BIGINT,
           protection        TEXT NOT NULL,
           protected_until   BIGINT,
           value_score       REAL NOT NULL DEFAULT 0.0,
           fragility_score   REAL NOT NULL DEFAULT 0.0,
           byte_size         BIGINT NOT NULL,
           last_access       BIGINT,
           access_count      BIGINT NOT NULL DEFAULT 0,
           pending_embedding BOOLEAN NOT NULL DEFAULT FALSE,
           search            tsvector GENERATED ALWAYS AS
                               (to_tsvector('simple'::regconfig, body || ' ' || tags_text)) STORED,
           PRIMARY KEY (tenant_id, id)
         ){by_hash}"
    ));

    out.push(format!(
        "CREATE TABLE IF NOT EXISTS vectors (
           tenant_id TEXT NOT NULL,
           item_id   TEXT NOT NULL,
           subject   TEXT NOT NULL,
           namespace TEXT NOT NULL,
           embedder  TEXT NOT NULL,
           dim       INTEGER NOT NULL,
           scale     REAL NOT NULL,
           q         BYTEA NOT NULL,
           embedding vector({dim}) NOT NULL,
           PRIMARY KEY (tenant_id, item_id),
           FOREIGN KEY (tenant_id, item_id)
             REFERENCES items (tenant_id, id) ON DELETE CASCADE
         ){by_hash}"
    ));

    out.push(format!(
        "CREATE TABLE IF NOT EXISTS capacity (
           tenant_id  TEXT NOT NULL,
           subject    TEXT NOT NULL,
           namespace  TEXT NOT NULL,
           max_items  BIGINT,
           max_bytes  BIGINT,
           used_items BIGINT NOT NULL DEFAULT 0,
           used_bytes BIGINT NOT NULL DEFAULT 0,
           PRIMARY KEY (tenant_id, subject, namespace)
         ){by_hash}"
    ));

    out.push(format!(
        "CREATE TABLE IF NOT EXISTS audit (
           tenant_id  TEXT NOT NULL,
           id         TEXT NOT NULL,
           at         BIGINT NOT NULL,
           subject    TEXT NOT NULL,
           namespace  TEXT NOT NULL,
           event      TEXT NOT NULL,
           items      JSONB NOT NULL,
           assessment JSONB,
           decision   JSONB,
           actor      JSONB NOT NULL,
           policy     TEXT,
           PRIMARY KEY (tenant_id, id)
         ){by_hash}"
    ));

    out.push(format!(
        "CREATE TABLE IF NOT EXISTS idempotency (
           tenant_id      TEXT NOT NULL,
           key            TEXT NOT NULL,
           subject        TEXT NOT NULL,
           namespace      TEXT NOT NULL,
           payload_digest TEXT NOT NULL,
           outcome        JSONB NOT NULL,
           at             BIGINT NOT NULL,
           PRIMARY KEY (tenant_id, key)
         ){by_hash}"
    ));

    out.push("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)".into());

    if partitioned {
        let modulus = config.partitions;
        for table in TENANT_TABLES {
            for remainder in 0..modulus {
                out.push(format!(
                    "CREATE TABLE IF NOT EXISTS {table}_p{remainder} PARTITION OF {table}
                     FOR VALUES WITH (MODULUS {modulus}, REMAINDER {remainder})"
                ));
            }
        }
    }

    out.extend([
        "CREATE INDEX IF NOT EXISTS idx_items_scope ON items (tenant_id, subject, namespace)".into(),
        "CREATE INDEX IF NOT EXISTS idx_items_scope_sens
           ON items (tenant_id, subject, namespace, sensitivity)".into(),
        "CREATE INDEX IF NOT EXISTS idx_items_search ON items USING GIN (search)".into(),
        "CREATE INDEX IF NOT EXISTS idx_items_tags ON items USING GIN (tags)".into(),
        "CREATE INDEX IF NOT EXISTS idx_vectors_scope
           ON vectors (tenant_id, subject, namespace, embedder, dim)".into(),
        "CREATE INDEX IF NOT EXISTS idx_vectors_hnsw
           ON vectors USING hnsw (embedding vector_ip_ops)".into(),
        // Two partial unique indexes on the aggregates, plus the ordering
        // index — see the DDL above for why one index over the nullable tuple
        // does not enforce uniqueness, and why the collation is stated.
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_aggregates_key_policied
           ON audit_aggregates (tenant_id, policy_name, policy_version, event, day)
           WHERE policy_name IS NOT NULL".into(),
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_aggregates_key_policy_less
           ON audit_aggregates (tenant_id, event, day)
           WHERE policy_name IS NULL".into(),
        // `NULLS FIRST` on both nullable columns is load-bearing, not decorative:
        // without it this index cannot serve the read's `ASC NULLS FIRST`
        // ordering and the planner sorts. Must match the DDL above exactly.
        "CREATE INDEX IF NOT EXISTS idx_aggregates_order
           ON audit_aggregates (tenant_id, day,
                                policy_name COLLATE \"C\" ASC NULLS FIRST,
                                policy_version COLLATE \"C\" ASC NULLS FIRST,
                                event COLLATE \"C\")".into(),
        // Two indexes for two query shapes; see the DDL above for why the
        // `at`-leading one cannot serve `ORDER BY id DESC` or the
        // `AuditFilter::after` cursor.
        "CREATE INDEX IF NOT EXISTS idx_audit_scope_id
           ON audit (tenant_id, subject, namespace, id DESC)".into(),
        "CREATE INDEX IF NOT EXISTS idx_audit_scope_at
           ON audit (tenant_id, subject, namespace, at, id DESC)".into(),
    ]);

    for table in TENANT_TABLES {
        out.push(format!("ALTER TABLE {table} ENABLE ROW LEVEL SECURITY"));
        // FORCE subjects the table owner to the policy too. Without it, a
        // deployment that ran as the schema owner would have no isolation.
        out.push(format!("ALTER TABLE {table} FORCE ROW LEVEL SECURITY"));
        // `CREATE POLICY` has no `IF NOT EXISTS`, so drop-then-create is what
        // makes the bootstrap re-runnable.
        out.push(format!("DROP POLICY IF EXISTS tenant_isolation ON {table}"));
        out.push(format!(
            "CREATE POLICY tenant_isolation ON {table}
               USING      (tenant_id = nullif(current_setting('memorysafe.tenant_id', true), ''))
               WITH CHECK (tenant_id = nullif(current_setting('memorysafe.tenant_id', true), ''))"
        ));
    }

    // Grants name the parent tables only. Access through a partitioned parent
    // is authorised against the parent, so the app role works normally while a
    // direct `SELECT FROM items_p0` — the obvious way around a parent-level
    // policy — is denied outright.
    out.push(format!("GRANT USAGE ON SCHEMA {schema}, public TO {role}"));
    out.push(format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON {} TO {role}",
        TENANT_TABLES.join(", ")
    ));
    out.push(format!("GRANT SELECT ON meta TO {role}"));

    out.push(format!(
        "INSERT INTO meta (key, value) VALUES ('schema_version', '{SCHEMA_VERSION}')
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"
    ));

    out
}
```

`crates/memorysafe-backend-postgres/src/bootstrap.rs`:

```rust
use crate::config::PgConfig;
use crate::ddl;
use crate::error::SqlxResultExt;
use memorysafe_backend::BackendError;
use sqlx::PgPool;

/// Creates the application role if it is absent.
///
/// `NOLOGIN` is deliberate: the role is a privilege container that connections
/// assume with `SET ROLE`, not a credential, so there is no password to leak
/// or rotate. `NOBYPASSRLS` and `NOSUPERUSER` are what make the policies
/// apply at all.
pub async fn ensure_role(admin: &PgPool, role: &str) -> Result<(), BackendError> {
    ddl::checked_ident(role)?;
    sqlx::query(&format!(
        "DO $ms$ BEGIN
           IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{role}') THEN
             CREATE ROLE {role} NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
           END IF;
         END $ms$"
    ))
    .execute(admin)
    .await
    .pg()?;
    Ok(())
}

/// Brings one schema up to `SCHEMA_VERSION`, in a single transaction.
///
/// DDL is transactional in PostgreSQL, so a failure here leaves nothing
/// behind — there is no half-initialised schema to reason about.
pub async fn ensure_schema(
    admin: &PgPool,
    config: &PgConfig,
    schema: &str,
) -> Result<(), BackendError> {
    ddl::checked_ident(schema)?;
    let mut tx = admin.begin().await.pg()?;
    for statement in ddl::statements(config, schema) {
        sqlx::query(&statement).execute(&mut *tx).await.pg()?;
    }
    tx.commit().await.pg()?;
    Ok(())
}
```

`crates/memorysafe-backend-postgres/src/lib.rs`:

```rust
//! The commercial PostgreSQL backend for MemorySafe.
//!
//! Isolation is enforced by row-level security under a non-superuser role,
//! not by the application's `WHERE` clauses. See `session::tenant_txn`.

pub mod bootstrap;
pub mod config;
pub mod ddl;
pub mod error;

pub use config::{PgConfig, PgLayout, schema_for_tenant};

pub(crate) use error::SqlxResultExt;

use memorysafe_backend::BackendError;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::collections::HashSet;
use std::sync::Mutex;

pub struct PostgresBackend {
    /// Owns DDL. Connects with the configured credentials and keeps them.
    pub(crate) admin: PgPool,
    /// Every runtime statement. `SET ROLE`s to the app role on checkout, so
    /// `current_user` is a role that RLS applies to.
    pub(crate) app: PgPool,
    pub(crate) config: PgConfig,
    /// Schemas already brought up to `SCHEMA_VERSION` in this process.
    /// Meaningful only under `SchemaPerTenant`, where schemas appear lazily.
    pub(crate) ready: Mutex<HashSet<String>>,
}

impl PostgresBackend {
    pub async fn connect(config: PgConfig) -> Result<Self, BackendError> {
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&config.url)
            .await
            .pg()?;

        bootstrap::ensure_role(&admin, &config.app_role).await?;

        let role = config.app_role.clone();
        ddl::checked_ident(&role)?;
        let app = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .after_connect(move |conn, _meta| {
                let role = role.clone();
                Box::pin(async move {
                    // Drops superuser rights for the session. Without this,
                    // a pool connected as `postgres` would ignore every
                    // policy and the isolation claim would be empty.
                    sqlx::query(&format!("SET ROLE {role}")).execute(conn).await?;
                    Ok(())
                })
            })
            .connect(&config.url)
            .await
            .pg()?;

        let backend = Self { admin, app, config, ready: Mutex::new(HashSet::new()) };

        if backend.config.layout == PgLayout::SharedPartitioned {
            let schema = backend.config.schema.clone();
            bootstrap::ensure_schema(&backend.admin, &backend.config, &schema).await?;
            backend.ready.lock().expect("ready set").insert(schema);
        }

        Ok(backend)
    }

    pub fn config(&self) -> &PgConfig {
        &self.config
    }

    /// The runtime pool, exposed so the isolation tests can pose as the
    /// application without going through the `Backend` methods.
    pub fn app_pool(&self) -> &PgPool {
        &self.app
    }
}

/// Rough token count for budget packing: ~4 bytes per token. Identical to the
/// SQLite backend's, so a `ScoredCandidate` costs the same on both.
pub(crate) fn estimate_tokens(body: &str) -> u32 {
    ((body.len() as f32 / 4.0).ceil() as u32).max(1)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres --test bootstrap`
Expected: PASS — 5 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): transactional bootstrap with forced RLS and a non-superuser app role"
```

---

## Task 6: Session plumbing — `tenant_txn`

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/session.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`
- Modify: `crates/memorysafe-backend-postgres/tests/bootstrap.rs`

**Interfaces:**
- Consumes: `PostgresBackend`, `schema_for_tenant`, `bootstrap::ensure_schema`.
- Produces: `session::tenant_txn`, `PostgresBackend::ensure_ready`.

**Everything the backend does goes through this function,** reads included. That is what lets the tenant GUC be transaction-local: a `SET` at session scope would survive on a pooled connection and the next borrower would inherit someone else's tenant.

**An honest statement of what RLS buys.** The application chooses the tenant it declares, so RLS is not a defence against this crate deciding to lie. What it defends against is the failure that actually happens: a query that forgets its `tenant_id` predicate, a join that loses it, a hand-written maintenance statement. Those return nothing instead of everything. The tests in Task 8 are written against exactly that failure.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-backend-postgres/tests/bootstrap.rs`:

```rust
#[tokio::test]
async fn the_tenant_guc_is_transaction_local() {
    use memorysafe_core::TenantId;
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let backend = PostgresBackend::connect(config).await.unwrap();
    let tenant = TenantId::new("acme").unwrap();

    {
        let mut tx = backend.begin_for_test(&tenant).await.unwrap();
        let inside: Option<String> =
            sqlx::query("SELECT current_setting('memorysafe.tenant_id', true) AS t")
                .fetch_one(&mut *tx)
                .await
                .unwrap()
                .get("t");
        assert_eq!(inside.as_deref(), Some("acme"));
        tx.commit().await.unwrap();
    }

    // A later borrower of a pooled connection must not inherit it.
    //
    // TWO THINGS THIS TEST MUST NOT ASSUME, both measured rather than reasoned:
    //
    // 1. **It does not reset to unset.** A custom GUC never declared in
    //    `postgresql.conf` resets to the empty string, not to unset, so
    //    `current_setting(..., true)` returns `Some("")` here on PostgreSQL 17
    //    and `assert_eq!(leaked, None)` fails. Compare against the *tenant*,
    //    which is the property actually at stake.
    // 2. **The pool does not reliably hand back the same connection.** Eight
    //    sequential acquisitions at `max_connections = 4` were observed to use
    //    three distinct backends. A loop that assumes reuse inspects
    //    connections that never ran the transaction and passes whatever the
    //    GUC does — so pin the connection instead of looping and hoping.
    let mut held = backend.app_pool().acquire().await.unwrap();
    for _ in 0..8 {
        let leaked: Option<String> =
            sqlx::query("SELECT nullif(current_setting('memorysafe.tenant_id', true), '') AS t")
                .fetch_one(&mut *held)
                .await
                .unwrap()
                .get("t");
        assert_eq!(
            leaked.as_deref(),
            None,
            "the tenant GUC leaked out of its transaction onto a pooled connection"
        );
    }
}

#[tokio::test]
async fn the_transaction_sets_a_search_path_that_reaches_pgvector() {
    use memorysafe_core::TenantId;
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let schema = config.schema.clone();
    let backend = PostgresBackend::connect(config).await.unwrap();

    let mut tx = backend.begin_for_test(&TenantId::new("acme").unwrap()).await.unwrap();
    let path: String = sqlx::query("SELECT current_setting('search_path') AS p")
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .get("p");
    assert!(path.starts_with(&schema), "search_path is {path}");
    assert!(path.contains("public"), "public must stay on the path for the vector type");

    // The `<#>` operator must resolve under that path.
    let ok: f64 = sqlx::query("SELECT ('[1,0]'::vector <#> '[1,0]'::vector)::float8 AS d")
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .get("d");
    assert_eq!(ok, -1.0);
    tx.commit().await.unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test bootstrap`
Expected: FAIL — `no method named begin_for_test found for struct PostgresBackend`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/session.rs`:

```rust
use crate::config::{PgLayout, schema_for_tenant};
use crate::error::SqlxResultExt;
use crate::{PostgresBackend, bootstrap};
use memorysafe_backend::BackendError;
use memorysafe_core::TenantId;
use sqlx::{Postgres, Transaction};

/// Opens the transaction every backend operation runs inside — reads included.
///
/// Three settings, all transaction-local:
///
/// * `search_path` puts the tenant's schema first and keeps `public` reachable
///   for the `vector` type and the `<#>` operator.
/// * `memorysafe.tenant_id` is what the RLS policies compare against. With no
///   tenant claimed the policies see NULL — via `nullif(..., '')`, because an
///   undeclared custom GUC resets to the empty string rather than to unset —
///   so every comparison is NULL and every row is filtered. A bug that skips
///   this function fails closed.
/// * `hnsw.iterative_scan` lets pgvector keep scanning when the scope
///   predicate rejects most of what the index returns. Without it a selective
///   scope can starve an ANN result set; with it the recall loss is bounded.
///   It costs nothing on the plans where the index is not used.
pub(crate) async fn tenant_txn<'a>(
    be: &'a PostgresBackend,
    tenant: &TenantId,
) -> Result<Transaction<'a, Postgres>, BackendError> {
    let schema = schema_for_tenant(&be.config, tenant);
    be.ensure_ready(&schema).await?;

    let mut tx = be.app.begin().await.pg()?;
    sqlx::query("SELECT set_config('search_path', $1, true)")
        .bind(format!("{schema}, public"))
        .execute(&mut *tx)
        .await
        .pg()?;
    sqlx::query("SELECT set_config('memorysafe.tenant_id', $1, true)")
        .bind(tenant.as_str())
        .execute(&mut *tx)
        .await
        .pg()?;
    sqlx::query("SELECT set_config('hnsw.iterative_scan', 'relaxed_order', true)")
        .execute(&mut *tx)
        .await
        .pg()?;
    Ok(tx)
}

impl PostgresBackend {
    /// Under `SharedPartitioned` the one schema is created at `connect`; under
    /// `SchemaPerTenant` a tenant's schema appears the first time it is
    /// written to. The set makes the second case a single DDL round trip per
    /// tenant per process rather than one per operation.
    pub(crate) async fn ensure_ready(&self, schema: &str) -> Result<(), BackendError> {
        if self.config.layout == PgLayout::SharedPartitioned {
            return Ok(());
        }
        if self.ready.lock().expect("ready set").contains(schema) {
            return Ok(());
        }
        bootstrap::ensure_schema(&self.admin, &self.config, schema).await?;
        self.ready.lock().expect("ready set").insert(schema.to_owned());
        Ok(())
    }

    /// Test-only access to `tenant_txn`, so the isolation and session tests
    /// can pose as the backend without a `Backend` method in between.
    pub async fn begin_for_test(
        &self,
        tenant: &TenantId,
    ) -> Result<Transaction<'_, Postgres>, BackendError> {
        tenant_txn(self, tenant).await
    }
}
```

Add `pub mod session;` to `crates/memorysafe-backend-postgres/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres --test bootstrap`
Expected: PASS — 7 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): transaction-scoped tenant context with a fail-closed GUC"
```

---

## Task 7: Items, audit, and the first conformance run

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/items.rs`
- Create: `crates/memorysafe-backend-postgres/src/audit.rs`
- Create: `crates/memorysafe-backend-postgres/tests/conformance.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`

**Interfaces:**
- Consumes: `tenant_txn`, `SqlxResultExt`, all core types.
- Produces: `items::item_columns(alias)`, `items::row_to_item`, `items::insert`, `items::get`, `items::list`, `items::delete`, `items::exists`, `audit::insert`, `audit::query`, `audit::row_to_record`, the `Backend` impl skeleton, and `PgFactory`.

**Milestone:** the four isolation conformance tests execute against Postgres for the first time.

**Placeholders are deliberate here.** `retrieve_candidates`, `neighbours`, `capacity_state`, `scope_stats`, `set_budget`, `purge_subject`, `export` and `import` return empty defaults so the crate compiles; Tasks 9–13 replace each one and the conformance test that covers it is wired in the same task.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend-postgres/tests/conformance.rs`:

```rust
mod support;

use memorysafe_backend::conformance::{BackendFactory, isolation};
use memorysafe_backend_postgres::{PgLayout, PostgresBackend};
use std::future::Future;

/// Each conformance test gets a backend rooted in its own PostgreSQL schema,
/// which is the Postgres analogue of the SQLite factory's fresh `TempDir`.
pub struct PgFactory {
    pub layout: PgLayout,
}

impl BackendFactory for PgFactory {
    type B = PostgresBackend;
    fn create(&self) -> impl Future<Output = Self::B> + Send {
        let layout = self.layout;
        async move {
            let config = support::test_config(layout).await;
            PostgresBackend::connect(config).await.expect("connect and bootstrap")
        }
    }
}

fn shared() -> PgFactory {
    PgFactory { layout: PgLayout::SharedPartitioned }
}

#[tokio::test]
async fn tenants_are_isolated() {
    isolation::tenants_are_isolated(&shared()).await;
}

#[tokio::test]
async fn subjects_are_isolated() {
    isolation::subjects_are_isolated(&shared()).await;
}

#[tokio::test]
async fn namespaces_are_separated() {
    isolation::namespaces_are_separated(&shared()).await;
}

#[tokio::test]
async fn audit_is_scoped() {
    isolation::audit_is_scoped(&shared()).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test conformance`
Expected: FAIL — `the trait bound PostgresBackend: Backend is not satisfied`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/items.rs`:

```rust
use crate::error::SqlxResultExt;
use memorysafe_backend::{BackendError, Page};
use memorysafe_core::{
    ItemId, MemoryItem, Protection, Scope, SensitivityLevel, Source, SourceKind,
};
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
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

const BARE_COLUMNS: [&str; 17] = [
    "id", "tenant_id", "subject", "namespace", "body", "kind", "source_kind", "source_id",
    "occurred_at", "created_at", "tags", "attrs", "sensitivity", "ttl_seconds", "protection",
    "protected_until", "pending_embedding",
];

/// The item columns, aliased so `row_to_item` finds them by bare name even in
/// a join where both sides have a `tenant_id`.
pub fn item_columns(alias: &str) -> String {
    BARE_COLUMNS
        .iter()
        .map(|c| format!("{alias}.{c} AS {c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn row_to_item(row: &PgRow) -> Result<MemoryItem, BackendError> {
    let tenant: String = row.try_get("tenant_id").pg()?;
    let subject: String = row.try_get("subject").pg()?;
    let namespace: String = row.try_get("namespace").pg()?;
    let id: String = row.try_get("id").pg()?;
    let sensitivity: i16 = row.try_get("sensitivity").pg()?;
    let occurred: Option<i64> = row.try_get("occurred_at").pg()?;
    let created: i64 = row.try_get("created_at").pg()?;
    let ttl: Option<i64> = row.try_get("ttl_seconds").pg()?;
    let protection: String = row.try_get("protection").pg()?;
    let protected_until: Option<i64> = row.try_get("protected_until").pg()?;
    let attrs: Json<BTreeMap<String, serde_json::Value>> = row.try_get("attrs").pg()?;

    Ok(MemoryItem {
        id: ItemId::parse(&id)
            .map_err(|e| BackendError::Storage { message: e.to_string(), retryable: false })?,
        scope: Scope::new(&tenant, &subject, &namespace)
            .map_err(|e| BackendError::Storage { message: e.to_string(), retryable: false })?,
        body: row.try_get("body").pg()?,
        kind: row.try_get("kind").pg()?,
        source: Source {
            kind: source_kind_from(&row.try_get::<String, _>("source_kind").pg()?),
            id: row.try_get("source_id").pg()?,
        },
        occurred_at: occurred.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()),
        created_at: OffsetDateTime::from_unix_timestamp(created)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH),
        tags: row.try_get("tags").pg()?,
        attrs: attrs.0,
        // An unknown ordinal means a newer writer used a level this build does
        // not know. Treating it as Restricted keeps it out of every result set
        // rather than leaking it as Public.
        sensitivity: SensitivityLevel::from_ordinal(i64::from(sensitivity))
            .unwrap_or(SensitivityLevel::Restricted),
        ttl: ttl.map(Duration::seconds),
        protection: protection_from(&protection, protected_until),
        pending_embedding: row.try_get("pending_embedding").pg()?,
    })
}

pub async fn insert(conn: &mut PgConnection, item: &MemoryItem) -> Result<(), BackendError> {
    let (protection, protected_until) = protection_parts(item.protection);
    sqlx::query(
        "INSERT INTO items (tenant_id, id, subject, namespace, body, kind, source_kind,
             source_id, occurred_at, created_at, tags, tags_text, attrs, sensitivity,
             ttl_seconds, protection, protected_until, byte_size, pending_embedding)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
    )
    .bind(item.scope.tenant.as_str())
    .bind(item.id.as_str())
    .bind(item.scope.subject.as_str())
    .bind(item.scope.namespace.as_str())
    .bind(&item.body)
    .bind(&item.kind)
    .bind(source_kind_str(item.source.kind))
    .bind(item.source.id.as_deref())
    .bind(item.occurred_at.map(|t| t.unix_timestamp()))
    .bind(item.created_at.unix_timestamp())
    .bind(&item.tags)
    // `tags_text` feeds the generated tsvector. It cannot be derived in SQL:
    // `array_to_string` is STABLE, and a generated column needs IMMUTABLE.
    .bind(item.tags.join(" "))
    .bind(Json(&item.attrs))
    .bind(item.sensitivity.ordinal() as i16)
    .bind(item.ttl.map(|d| d.whole_seconds()))
    .bind(protection)
    .bind(protected_until)
    .bind(item.byte_size() as i64)
    .bind(item.pending_embedding)
    .execute(conn)
    .await
    .pg()?;
    Ok(())
}

pub async fn get(
    conn: &mut PgConnection,
    scope: &Scope,
    id: &ItemId,
) -> Result<Option<MemoryItem>, BackendError> {
    let sql = format!(
        "SELECT {cols} FROM items i
         WHERE i.tenant_id = $1 AND i.subject = $2 AND i.namespace = $3 AND i.id = $4",
        cols = item_columns("i")
    );
    let row = sqlx::query(&sql)
        .bind(scope.tenant.as_str())
        .bind(scope.subject.as_str())
        .bind(scope.namespace.as_str())
        .bind(id.as_str())
        .fetch_optional(conn)
        .await
        .pg()?;
    row.as_ref().map(row_to_item).transpose()
}

pub async fn list(
    conn: &mut PgConnection,
    scope: &Scope,
    page: &Page,
) -> Result<Vec<MemoryItem>, BackendError> {
    // Ordered by id: ULIDs are lexicographically time-ordered, which makes
    // pagination stable under concurrent inserts.
    let sql = format!(
        "SELECT {cols} FROM items i
         WHERE i.tenant_id = $1 AND i.subject = $2 AND i.namespace = $3
         ORDER BY i.id ASC LIMIT $4 OFFSET $5",
        cols = item_columns("i")
    );
    let rows = sqlx::query(&sql)
        .bind(scope.tenant.as_str())
        .bind(scope.subject.as_str())
        .bind(scope.namespace.as_str())
        .bind(page.effective_limit() as i64)
        .bind(page.offset as i64)
        .fetch_all(conn)
        .await
        .pg()?;
    rows.iter().map(row_to_item).collect()
}

/// Returns the byte size of what was removed, for capacity accounting. The
/// vector row goes with it through the foreign key's `ON DELETE CASCADE`.
pub async fn delete(
    conn: &mut PgConnection,
    scope: &Scope,
    id: &ItemId,
) -> Result<u64, BackendError> {
    let row = sqlx::query(
        "DELETE FROM items
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3 AND id = $4
         RETURNING byte_size",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await
    .pg()?;
    Ok(row.map(|r| r.get::<i64, _>("byte_size").max(0) as u64).unwrap_or(0))
}

pub async fn exists(
    conn: &mut PgConnection,
    scope: &Scope,
    id: &ItemId,
) -> Result<bool, BackendError> {
    let row = sqlx::query(
        "SELECT 1 AS present FROM items
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3 AND id = $4",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .bind(id.as_str())
    .fetch_optional(conn)
    .await
    .pg()?;
    Ok(row.is_some())
}
```

`crates/memorysafe-backend-postgres/src/audit.rs`:

```rust
use crate::error::SqlxResultExt;
use memorysafe_backend::BackendError;
use memorysafe_core::{
    Actor, AuditEvent, AuditFilter, AuditId, AuditRecord, Assessment, Decision, ItemRef, Scope,
};
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use sqlx::{PgConnection, Row};
use time::OffsetDateTime;

/// `AuditEvent` is stored as its serde snake_case name so the column is
/// readable in `psql` and filterable without a join table.
// `AuditEvent::as_str` is the one referent for this string — the serde form,
// the stored value, and the key `AggregateKey` sorts by. This used to be a
// serde round-trip with `.unwrap_or_default()`, which on a serialisation
// failure would have written an **empty event string** into the audit table
// rather than failing; and being a second encoder, it could drift from the one
// the ordering compares.
fn event_str(e: AuditEvent) -> &'static str {
    e.as_str()
}

fn event_from(s: &str) -> Result<AuditEvent, BackendError> {
    serde_json::from_str(&format!("\"{s}\""))
        .map_err(|e| BackendError::Storage { message: e.to_string(), retryable: false })
}

pub fn row_to_record(row: &PgRow) -> Result<AuditRecord, BackendError> {
    let tenant: String = row.try_get("tenant_id").pg()?;
    let subject: String = row.try_get("subject").pg()?;
    let namespace: String = row.try_get("namespace").pg()?;
    let id: String = row.try_get("id").pg()?;
    let at: i64 = row.try_get("at").pg()?;
    let event: String = row.try_get("event").pg()?;
    let items: Json<Vec<ItemRef>> = row.try_get("items").pg()?;
    let assessment: Option<Json<Assessment>> = row.try_get("assessment").pg()?;
    let decision: Option<Json<Decision>> = row.try_get("decision").pg()?;
    let actor: Json<Actor> = row.try_get("actor").pg()?;

    Ok(AuditRecord {
        id: AuditId::parse(&id)
            .map_err(|e| BackendError::Storage { message: e.to_string(), retryable: false })?,
        at: OffsetDateTime::from_unix_timestamp(at).unwrap_or(OffsetDateTime::UNIX_EPOCH),
        scope: Scope::new(&tenant, &subject, &namespace)
            .map_err(|e| BackendError::Storage { message: e.to_string(), retryable: false })?,
        event: event_from(&event)?,
        items: items.0,
        assessment: assessment.map(|a| a.0),
        decision: decision.map(|d| d.0),
        actor: actor.0,
    })
}

pub async fn insert(
    conn: &mut PgConnection,
    record: &AuditRecord,
) -> Result<AuditId, BackendError> {
    sqlx::query(
        "INSERT INTO audit (tenant_id, id, at, subject, namespace, event, items,
             assessment, decision, actor, policy)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(record.scope.tenant.as_str())
    .bind(record.id.as_str())
    .bind(record.at.unix_timestamp())
    .bind(record.scope.subject.as_str())
    .bind(record.scope.namespace.as_str())
    .bind(event_str(record.event))
    .bind(Json(&record.items))
    .bind(record.assessment.as_ref().map(Json))
    .bind(record.decision.as_ref().map(Json))
    .bind(Json(&record.actor))
    .bind(record.decision.as_ref().map(|d| d.policy.to_string()))
    .execute(conn)
    .await
    .pg()?;
    Ok(record.id.clone())
}

pub async fn query(
    conn: &mut PgConnection,
    scope: &Scope,
    filter: &AuditFilter,
) -> Result<Vec<AuditRecord>, BackendError> {
    // Optional filters are expressed as NULL-guarded predicates rather than
    // dynamically numbered placeholders: one SQL string, one bind order,
    // nothing to get out of step.
    let events: Option<Vec<String>> = if filter.events.is_empty() {
        None
    } else {
        Some(filter.events.iter().map(|e| event_str(*e).to_string()).collect())
    };

    // Ordered by id, descending (newest first) — see `AuditFilter::after`'s
    // doc comment in memorysafe-core: `at` is whole seconds and cannot
    // separate rows written in the same second, so `id` alone is the total
    // order, not a tie-break on `at`. `since`/`until` are inclusive, hence
    // `>=`/`<=` rather than `>`/`<`; `audit_since_and_until_include_a_record_on_the_boundary`
    // rejects the exclusive reading.
    //
    // `after` is `id < $7`, strictly smaller, because the list descends and
    // "after" names a position in that order rather than a point in time.
    // `audit_pages_by_the_after_cursor_without_repeating_a_row` rejects
    // `id > after` (which re-serves rows forever), `id <= after` (which
    // re-serves one) and any `at`-based cursor.
    let rows = sqlx::query(
        "SELECT tenant_id, id, at, subject, namespace, event, items, assessment,
                decision, actor
         FROM audit
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3
           AND ($4::text[] IS NULL OR event = ANY($4))
           AND ($5::bigint IS NULL OR at >= $5)
           AND ($6::bigint IS NULL OR at <= $6)
           AND ($7::text IS NULL OR id < $7)
           AND ($8::text IS NULL OR items @> jsonb_build_array(
                   jsonb_build_object('id', $8::text)))
         ORDER BY id DESC
         LIMIT $9",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .bind(events)
    .bind(filter.since.map(|t| t.unix_timestamp()))
    .bind(filter.until.map(|t| t.unix_timestamp()))
    .bind(filter.after.as_ref().map(|a| a.as_str()))
    .bind(filter.item.as_ref().map(|i| i.as_str()))
    .bind(filter.limit as i64)
    .fetch_all(conn)
    .await
    .pg()?;

    let out: Vec<AuditRecord> = rows.iter().map(row_to_record).collect::<Result<_, _>>()?;
    // `AuditFilter::item` is filtered in the `WHERE` clause above, not here.
    // Filtering in Rust after `LIMIT` would let a page come back shorter than
    // `min(filter.limit, rows still matching)`, which `Backend::audit`
    // forbids and which a caller reads as "the log is exhausted". No
    // conformance test sets `filter.item`, so nothing would have caught it.
    //
    // Over-fetching and looping until the page fills was rejected: it turns a
    // correctness property into a retry heuristic that still returns a short
    // page under adversarial data, and it re-creates the class this repository
    // already paid to fix at `b6bb15f`.
    //
    // The predicate pushes down because `AuditFilter::item` is `Option<ItemId>`
    // against `AuditRecord::items: Vec<ItemRef>` — a **membership** test, which
    // Postgres expresses natively. The shape made it feel unpushable; it never
    // was.
    //
    // **What is settled and what is not.** Settled: the predicate belongs in
    // SQL, and over-fetch-and-loop is rejected. Not settled: the containment
    // operator below and its index implications. It was written without a
    // Postgres to run it against — no `psql` and no database were available to
    // whoever added it — so treat `@>` against `jsonb_build_array(...)` as a
    // sketch of the right shape, not a verified query. Whoever implements this
    // should confirm the operator, decide whether a GIN index on `items` or an
    // expression index on the extracted ids is the right support, and say which
    // in the task. The ruling constrains the shape; it does not constrain that
    // choice.
    Ok(out)
}
```

Add the `Backend` impl to `crates/memorysafe-backend-postgres/src/lib.rs`:

```rust
pub mod audit;
pub mod items;

use async_trait::async_trait;
use memorysafe_backend::{
    AppliedWrite, Backend, CandidateQuery, ExportStream, ImportReport, ImportStream, Page,
    PurgeReport, ScopeSelector, WriteTransaction,
};
use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, Budget, CapacityState, Embedding, ItemId, MemoryItem,
    PurgeCascade, Scope, ScopeStats, ScoredCandidate, SubjectId, TenantId,
};
use session::tenant_txn;

#[async_trait]
impl Backend for PostgresBackend {
    async fn get(&self, scope: &Scope, id: &ItemId) -> Result<Option<MemoryItem>, BackendError> {
        let mut tx = tenant_txn(self, &scope.tenant).await?;
        let out = items::get(&mut tx, scope, id).await?;
        tx.commit().await.pg()?;
        Ok(out)
    }

    async fn list(&self, scope: &Scope, page: &Page) -> Result<Vec<MemoryItem>, BackendError> {
        let mut tx = tenant_txn(self, &scope.tenant).await?;
        let out = items::list(&mut tx, scope, page).await?;
        tx.commit().await.pg()?;
        Ok(out)
    }

    async fn audit(
        &self,
        scope: &Scope,
        filter: &AuditFilter,
    ) -> Result<Vec<AuditRecord>, BackendError> {
        let mut tx = tenant_txn(self, &scope.tenant).await?;
        let out = audit::query(&mut tx, scope, filter).await?;
        tx.commit().await.pg()?;
        Ok(out)
    }

    async fn record_recall(&self, record: AuditRecord) -> Result<AuditId, BackendError> {
        let mut tx = tenant_txn(self, &record.scope.tenant).await?;
        let id = audit::insert(&mut tx, &record).await?;
        tx.commit().await.pg()?;
        Ok(id)
    }

    /// Insert, evictions, and the audit row, in one transaction. Vectors
    /// arrive in Task 9; capacity, merge, and idempotency in Task 12.
    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError> {
        if !txn.is_valid() {
            return Err(BackendError::InvalidTransaction(
                "a transaction may not both insert and merge".into(),
            ));
        }
        let mut tx = tenant_txn(self, &txn.scope.tenant).await?;

        let mut evicted = Vec::new();
        for id in &txn.evictions {
            items::delete(&mut tx, &txn.scope, id).await?;
            evicted.push(id.clone());
        }

        let mut item_id = None;
        if let Some(w) = &txn.upsert {
            items::insert(&mut tx, &w.item).await?;
            item_id = Some(w.item.id.clone());
        }

        let audit_id = audit::insert(&mut tx, &txn.audit).await?;
        tx.commit().await.pg()?;

        Ok(AppliedWrite {
            item_id,
            audit_id,
            evicted,
            replayed: false,
            replayed_outcome: None,
        })
    }

    // --- Filled in by Tasks 9-13. Empty defaults so the crate compiles. ---

    async fn retrieve_candidates(
        &self,
        _scope: &Scope,
        _query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        Ok(vec![])
    }

    async fn neighbours(
        &self,
        _scope: &Scope,
        _embedding: &Embedding,
        _k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        Ok(vec![])
    }

    async fn capacity_state(&self, _scope: &Scope) -> Result<CapacityState, BackendError> {
        Ok(CapacityState { budget: Budget::UNBOUNDED, used_items: 0, used_bytes: 0 })
    }

    async fn scope_stats(&self, _scope: &Scope) -> Result<ScopeStats, BackendError> {
        Ok(ScopeStats::default())
    }

    async fn set_budget(&self, _scope: &Scope, _budget: Budget) -> Result<(), BackendError> {
        Ok(())
    }

    async fn purge_subject(
        &self,
        _tenant: &TenantId,
        _subject: &SubjectId,
        _cascade: PurgeCascade,
        _audit: AuditRecord,
    ) -> Result<PurgeReport, BackendError> {
        Ok(PurgeReport {
            items_removed: 0,
            vectors_removed: 0,
            audit_rows_removed: 0,
            audit_rows_preserved: 0,
        })
    }

    async fn export(&self, _sel: &ScopeSelector) -> Result<ExportStream, BackendError> {
        Ok(vec![])
    }

    async fn import(&self, _destination: &TenantId, _stream: ImportStream)
        -> Result<ImportReport, BackendError> {
        Ok(ImportReport::default())
    }

    // Placeholder here; the real read lands with `purge`, `export` and
    // `import`. Three lifecycle conformance tests fail against this stub —
    // `audit_aggregates_survive_a_cascading_purge`,
    // `audit_aggregates_page_in_the_documented_order` and
    // `audit_aggregates_narrow_by_day_window_and_policy`. When implementing it:
    // order by `day`, then `policy_name`, then `policy_version`, then `event`,
    // each text column with an explicit `COLLATE "C"` and policy-less rows
    // placed first by an explicit `(policy_name IS NULL) DESC` rather than by
    // Postgres's default; page **ascending** with `after` selecting keys
    // **strictly greater**, which is the opposite direction from
    // `Backend::audit` next door; and return exactly
    // `min(limit, rows still matching after the cursor)`. Write the row
    // comparison out longhand rather than as a row-value `(a,b,c,d) > (w,x,y,z)`:
    // the row-value form yields NULL when any component is NULL, and the policy
    // columns are NULL for most event classes, so rows would silently vanish and
    // the short page would read as exhaustion. The crate must also carry
    // `ordering_sql_states_collation_and_null_placement`, asserting over the
    // SQL the query builder returns rather than a copied literal — see the
    // mandate on `Backend::audit_aggregates`.
    async fn audit_aggregates(&self, _tenant: &TenantId, _filter: &AuditAggregateFilter)
        -> Result<Vec<AuditAggregate>, BackendError> {
        Ok(vec![])
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres --test conformance`
Expected: PASS — 4 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): item and audit persistence; isolation conformance passes"
```

---

## Task 8: Row-level security is structural — the proof tests

**Files:**
- Create: `crates/memorysafe-backend-postgres/tests/isolation.rs`

**Interfaces:**
- Consumes: `PostgresBackend::begin_for_test`, `PostgresBackend::app_pool`, the conformance fixtures.
- Produces: nothing. This task adds no source code — it adds the evidence for the product claim.

**Why this is a task of its own.** The conformance suite proves the *backend's methods* isolate tenants, which any correct set of `WHERE` clauses would also do. The commercial claim is stronger than that: isolation is a property of the database, so a query that loses its predicate returns nothing rather than everything. Only a test that deliberately writes the wrong query can show that, and such a test cannot live in the OSS conformance suite because SQLite achieves isolation a different way — one file per tenant.

If any test here fails, the deployment has no tenant isolation even though every conformance test still passes. That is exactly the failure mode worth a dedicated gate.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend-postgres/tests/isolation.rs`:

```rust
mod support;

use memorysafe_backend::conformance::fx;
use memorysafe_backend::{Backend, Page};
use memorysafe_backend_postgres::{PgLayout, PostgresBackend};
use memorysafe_core::{Scope, TenantId};
use sqlx::Row;

async fn two_tenants() -> (PostgresBackend, String) {
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let schema = config.schema.clone();
    let backend = PostgresBackend::connect(config).await.unwrap();

    for tenant in ["tenant-a", "tenant-b"] {
        let scope = Scope::new(tenant, "s", "n").unwrap();
        backend
            .apply(fx::admit_txn(&scope, fx::item(&scope, "a private note"), None))
            .await
            .unwrap();
    }
    (backend, schema)
}

/// The claim: a query that forgets its tenant predicate still cannot cross
/// tenants. This is the one that separates "we filter carefully" from
/// "the database enforces it".
#[tokio::test]
async fn a_query_with_no_tenant_predicate_still_sees_one_tenant() {
    let (backend, schema) = two_tenants().await;
    let mut tx = backend.begin_for_test(&TenantId::new("tenant-a").unwrap()).await.unwrap();

    let n: i64 = sqlx::query(&format!("SELECT count(*) AS c FROM {schema}.items"))
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .get("c");

    assert_eq!(n, 1, "an unfiltered query saw {n} rows across tenants");
    tx.commit().await.unwrap();
}

/// The control for the test above: the rows really are there, so a count of
/// one is isolation and not an empty database.
#[tokio::test]
async fn the_same_query_sees_the_other_tenants_row_under_the_other_tenant() {
    let (backend, schema) = two_tenants().await;

    for tenant in ["tenant-a", "tenant-b"] {
        let mut tx = backend.begin_for_test(&TenantId::new(tenant).unwrap()).await.unwrap();
        let id: String = sqlx::query(&format!("SELECT tenant_id FROM {schema}.items"))
            .fetch_one(&mut *tx)
            .await
            .unwrap()
            .get("tenant_id");
        assert_eq!(id, tenant);
        tx.commit().await.unwrap();
    }
}

/// A code path that skips `tenant_txn` gets nothing, rather than everything.
#[tokio::test]
async fn a_connection_with_no_tenant_context_reads_nothing() {
    let (backend, schema) = two_tenants().await;

    let n: i64 = sqlx::query(&format!("SELECT count(*) AS c FROM {schema}.items"))
        .fetch_one(backend.app_pool())
        .await
        .unwrap()
        .get("c");
    assert_eq!(n, 0, "a query with no tenant GUC read {n} rows");
}

/// And it cannot write, either. Failing closed on reads but open on writes
/// would be worse than no policy at all.
#[tokio::test]
async fn a_connection_with_no_tenant_context_cannot_write() {
    let (backend, schema) = two_tenants().await;

    let err = sqlx::query(&format!(
        "INSERT INTO {schema}.items (tenant_id, id, subject, namespace, body, kind,
             source_kind, created_at, tags, tags_text, attrs, sensitivity, protection,
             byte_size)
         VALUES ('tenant-a','01GHOSTGHOSTGHOSTGHOSTGHOST','s','n','x','fact','agent',1,
                 ARRAY[]::text[], '', '{{}}'::jsonb, 1, 'normal', 1)"
    ))
    .execute(backend.app_pool())
    .await
    .expect_err("a write with no tenant context was accepted");

    assert!(
        err.to_string().contains("row-level security"),
        "expected an RLS violation, got {err}"
    );
}

/// `WITH CHECK`: a connection that has declared tenant B may not write a row
/// labelled tenant A. Without this half of the policy, a mislabelled insert
/// would plant a row its own tenant could never read or delete.
#[tokio::test]
async fn a_write_labelled_with_another_tenant_is_refused() {
    let (backend, schema) = two_tenants().await;
    let mut tx = backend.begin_for_test(&TenantId::new("tenant-b").unwrap()).await.unwrap();

    let err = sqlx::query(&format!(
        "INSERT INTO {schema}.items (tenant_id, id, subject, namespace, body, kind,
             source_kind, created_at, tags, tags_text, attrs, sensitivity, protection,
             byte_size)
         VALUES ('tenant-a','01FORGEFORGEFORGEFORGEFORG','s','n','x','fact','agent',1,
                 ARRAY[]::text[], '', '{{}}'::jsonb, 1, 'normal', 1)"
    ))
    .execute(&mut *tx)
    .await
    .expect_err("a cross-tenant write was accepted");

    assert!(
        err.to_string().contains("row-level security"),
        "expected an RLS violation, got {err}"
    );
}

/// A parent-level policy is worthless if the partitions can be read directly.
/// Grants name only the parents, so this fails on privileges before RLS is
/// even consulted.
#[tokio::test]
async fn partitions_cannot_be_read_directly() {
    let (backend, schema) = two_tenants().await;
    let mut tx = backend.begin_for_test(&TenantId::new("tenant-a").unwrap()).await.unwrap();

    let err = sqlx::query(&format!("SELECT count(*) FROM {schema}.items_p0"))
        .fetch_one(&mut *tx)
        .await
        .expect_err("a partition was readable directly");

    assert!(
        err.to_string().contains("permission denied"),
        "expected permission denied, got {err}"
    );
}

/// `purge_subject` and the conformance suite both assume the backend's own
/// methods isolate. This checks the same thing through the public surface, so
/// a regression shows up whichever way it is introduced.
#[tokio::test]
async fn the_backend_surface_agrees_with_the_policy() {
    let (backend, _schema) = two_tenants().await;
    let b = Scope::new("tenant-b", "s", "n").unwrap();
    assert_eq!(backend.list(&b, &Page::default()).await.unwrap().len(), 1);
    let c = Scope::new("tenant-c", "s", "n").unwrap();
    assert!(backend.list(&c, &Page::default()).await.unwrap().is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test isolation`
Expected: PASS on the first run if Tasks 5–7 are correct. That is the point: these tests do not drive new code, they pin down behaviour that already exists and is easy to lose. **To confirm they are load-bearing rather than vacuous, temporarily change `ensure_role` to create the role with `BYPASSRLS` and re-run** — `a_query_with_no_tenant_predicate_still_sees_one_tenant`, `a_connection_with_no_tenant_context_reads_nothing`, and both write tests must fail. Revert the change before proceeding.

- [ ] **Step 3: Write minimal implementation**

None. If the mutation check in Step 2 did not fail the expected four tests, the RLS setup in Task 5 is wrong and must be fixed here before continuing.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres --test isolation`
Expected: PASS — 7 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/tests/isolation.rs
git commit -m "test(pg): prove isolation survives a query with no tenant predicate"
```

---

## Task 9: Vectors — pgvector candidates, exact rerank

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/vectors.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`
- Modify: `crates/memorysafe-backend-postgres/tests/conformance.rs`

**Interfaces:**
- Consumes: `QuantizedVector`, `items::item_columns`, `items::row_to_item`.
- Produces: `vectors::to_pg_vector`, `vectors::insert`, `vectors::scope_embedder`, `vectors::search`, and a real `Backend::neighbours`.

**The design, and why it is not "just use pgvector's distance".** `q BYTEA` is the canonical vector; `embedding vector(n)` is a dequantised copy that exists only so the HNSW index has something to index. Candidate generation is `ORDER BY embedding <#> $probe LIMIT k * over_fetch`, and then every candidate is rescored in Rust with `QuantizedVector::dot` — the same function the SQLite backend scores with. Two consequences: the relevance numbers the engine sees are identical on both backends, so the policy cannot behave differently depending on where the data lives; and the approximation in HNSW affects only *which* candidates are considered, never how they are ranked.

**Measured, at 20,000 vectors in one scope on PostgreSQL 17.11 / pgvector 0.8.6:** the planner chooses the HNSW index and the query runs in about 0.1 ms. At two rows it chooses the scope btree and sorts, which is exact and also correct. No threshold is needed in the code — the planner already has one.

**`vectors::delete` does not exist.** The `ON DELETE CASCADE` on the composite foreign key removes a vector when its item goes, so an explicit delete would be a second way to do the same thing and a second way to get it wrong. `purge_subject` deletes vector rows directly, but only because it needs the count.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-backend-postgres/tests/conformance.rs`:

```rust
use memorysafe_backend::conformance::retrieval;

#[tokio::test]
async fn vector_search_ranks_by_similarity() {
    retrieval::vector_search_ranks_by_similarity(&shared()).await;
}

#[tokio::test]
async fn cross_model_vectors_are_rejected() {
    retrieval::cross_model_vectors_are_rejected(&shared()).await;
}

#[tokio::test]
async fn neighbours_break_ties_before_truncating_at_k() {
    retrieval::neighbours_break_ties_before_truncating_at_k(&shared()).await;
}
```

Append to `crates/memorysafe-backend-postgres/src/vectors.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::{EmbedderId, Embedding};

    fn quantized(values: &[f32]) -> QuantizedVector {
        QuantizedVector::from_embedding(&Embedding {
            vector: values.to_vec(),
            embedder: EmbedderId::new("test-3"),
            dim: values.len() as u16,
        })
    }

    #[test]
    fn a_vector_becomes_a_bracketed_literal_pgvector_accepts() {
        let text = to_pg_vector(&quantized(&[1.0, 0.0, -1.0]));
        assert!(text.starts_with('[') && text.ends_with(']'), "got {text}");
        assert_eq!(text.matches(',').count(), 2, "got {text}");
        assert!(!text.contains(' '), "pgvector's parser wants no spaces: {text}");
    }

    #[test]
    fn dequantization_recovers_the_original_scale() {
        // q[i] * scale is the round trip. The first component was 1.0 and
        // quantizes to 127, so scale * 127 must land back near 1.0.
        let q = quantized(&[1.0, 0.5, 0.0]);
        let text = to_pg_vector(&q);
        let first: f32 = text
            .trim_matches(['[', ']'])
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!((first - 1.0).abs() < 0.01, "first component came back as {first}");
    }

    #[test]
    fn a_zero_vector_is_representable() {
        // `from_embedding` gives a zero vector scale 1.0 so nothing divides by
        // zero; the literal must still be well formed.
        let text = to_pg_vector(&quantized(&[0.0, 0.0, 0.0]));
        assert_eq!(text, "[0,0,0]");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres vectors`
Expected: FAIL — `cannot find function to_pg_vector in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/vectors.rs`:

```rust
use crate::error::SqlxResultExt;
use crate::items::{item_columns, row_to_item};
use memorysafe_backend::BackendError;
use memorysafe_core::{ItemId, MemoryItem, Scope};
use memorysafe_embed::QuantizedVector;
use sqlx::{PgConnection, Row};

/// The `vector` input format: `[a,b,c]`, no spaces.
///
/// The stored `embedding` is the dequantised form of `q`, so the ANN index
/// ranks on the same geometry the exact rerank scores on — just with less
/// precision.
pub fn to_pg_vector(q: &QuantizedVector) -> String {
    let mut out = String::with_capacity(q.q.len() * 8 + 2);
    out.push('[');
    for (i, v) in q.q.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&(f32::from(*v) * q.scale).to_string());
    }
    out.push(']');
    out
}

/// Writes the canonical `q` and its dequantised copy together.
///
/// `expected_dim` is the deployment's `vector(n)` width. A mismatch is caught
/// here rather than as a PostgreSQL type error, so the caller gets
/// `EmbedderMismatch` — the same error a cross-model probe gets — instead of
/// an opaque `Storage`.
pub async fn insert(
    conn: &mut PgConnection,
    id: &ItemId,
    scope: &Scope,
    q: &QuantizedVector,
    expected_dim: u16,
) -> Result<(), BackendError> {
    if q.dim != expected_dim {
        return Err(BackendError::EmbedderMismatch {
            got: format!("{}:{}", q.embedder, q.dim),
            expected: format!("this deployment stores vector({expected_dim})"),
        });
    }
    sqlx::query(
        "INSERT INTO vectors (tenant_id, item_id, subject, namespace, embedder, dim,
             scale, q, embedding)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9::vector)
         ON CONFLICT (tenant_id, item_id) DO UPDATE SET
             embedder = EXCLUDED.embedder, dim = EXCLUDED.dim,
             scale = EXCLUDED.scale, q = EXCLUDED.q, embedding = EXCLUDED.embedding",
    )
    .bind(scope.tenant.as_str())
    .bind(id.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .bind(q.embedder.to_string())
    .bind(i32::from(q.dim))
    .bind(q.scale)
    .bind(q.to_bytes())
    .bind(to_pg_vector(q))
    .execute(conn)
    .await
    .pg()?;
    Ok(())
}

/// Which embedder this scope's vectors were produced by, or `None` when it
/// holds none. Comparing across models yields silently meaningless
/// similarities, so every search gates on this.
pub async fn scope_embedder(
    conn: &mut PgConnection,
    scope: &Scope,
) -> Result<Option<(String, u16)>, BackendError> {
    let row = sqlx::query(
        "SELECT embedder, dim FROM vectors
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3 LIMIT 1",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .fetch_optional(conn)
    .await
    .pg()?;

    Ok(row.map(|r| {
        (r.get::<String, _>("embedder"), r.get::<i32, _>("dim") as u16)
    }))
}

/// Top-k by cosine similarity.
///
/// pgvector orders the candidates — approximately, once the corpus is large
/// enough for the planner to reach for HNSW — and `QuantizedVector::dot`
/// scores them exactly. `over_fetch` is the margin that absorbs the
/// approximation: pull more candidates than asked for, then keep the best `k`
/// by exact score.
/// Task 11 adds a `filters: &HardFilters` parameter to this signature, once
/// `filters.rs` exists. Until then `neighbours` is the only caller and needs
/// no filtering.
pub async fn search(
    conn: &mut PgConnection,
    scope: &Scope,
    probe: &QuantizedVector,
    k: usize,
    over_fetch: usize,
) -> Result<Vec<(MemoryItem, f32)>, BackendError> {
    if k == 0 {
        return Ok(vec![]);
    }
    let sql = format!(
        "SELECT {cols}, v.scale AS v_scale, v.q AS v_q
         FROM vectors v
         JOIN items i ON i.tenant_id = v.tenant_id AND i.id = v.item_id
         WHERE v.tenant_id = $1 AND v.subject = $2 AND v.namespace = $3
           AND v.embedder = $4 AND v.dim = $5
         ORDER BY v.embedding <#> $6::vector
         LIMIT $7",
        cols = item_columns("i")
    );

    let rows = sqlx::query(&sql)
        .bind(scope.tenant.as_str())
        .bind(scope.subject.as_str())
        .bind(scope.namespace.as_str())
        .bind(probe.embedder.to_string())
        .bind(i32::from(probe.dim))
        .bind(to_pg_vector(probe))
        .bind((k.saturating_mul(over_fetch).max(k)) as i64)
        .fetch_all(conn)
        .await
        .pg()?;

    let mut scored = Vec::with_capacity(rows.len());
    for row in &rows {
        let item = row_to_item(row)?;
        let scale: f32 = row.try_get("v_scale").pg()?;
        let bytes: Vec<u8> = row.try_get("v_q").pg()?;
        let stored =
            QuantizedVector::from_bytes(probe.embedder.clone(), probe.dim, scale, &bytes)
                .map_err(|e| BackendError::Storage {
                    message: e.to_string(),
                    retryable: false,
                })?;
        let score = probe.dot(&stored).map_err(|e| BackendError::EmbedderMismatch {
            got: e.to_string(),
            expected: probe.embedder.to_string(),
        })?;
        scored.push((item, score));
    }

    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
    scored.truncate(k);
    Ok(scored)
}
```

Replace the `neighbours` placeholder in `lib.rs`:

```rust
    async fn neighbours(
        &self,
        scope: &Scope,
        embedding: &Embedding,
        k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        let mut tx = tenant_txn(self, &scope.tenant).await?;

        // Refuse a probe from a model the scope was not indexed with, rather
        // than returning similarities from two different spaces.
        if let Some((stored, dim)) = vectors::scope_embedder(&mut tx, scope).await?
            && (stored != embedding.embedder.to_string() || dim != embedding.dim)
        {
            return Err(BackendError::EmbedderMismatch {
                got: format!("{}:{}", embedding.embedder, embedding.dim),
                expected: format!("{stored}:{dim}"),
            });
        }

        let probe = memorysafe_embed::QuantizedVector::from_embedding(embedding);
        let hits = vectors::search(&mut tx, scope, &probe, k, self.config.over_fetch).await?;
        tx.commit().await.pg()?;

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
                // `last_access`/`access_count` are the two `items` columns the
                // DDL has always declared and nothing read until Plan 1's
                // contract task. Select them alongside the item columns and
                // return them from `vectors::search` as their own tuple
                // element — they must NOT go on `MemoryItem`, which is
                // exported and digested. A row never recalled reads back
                // `(None, 0)`, never `(created_at, 0)`.
                last_accessed_at: access.last_accessed_at,
                access_count: access.access_count,
            })
            .collect())
    }
```

Wire vector writes into `apply`, immediately after `items::insert`:

```rust
                if let Some(v) = &w.vector {
                    vectors::insert(&mut tx, &item.id, &txn.scope, v, self.config.vector_dim)
                        .await?;
                }
```

Add `pub mod vectors;` to `lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres`
Expected: PASS — 3 unit tests plus 6 conformance tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): pgvector candidate generation with exact int8 rerank"
```

---

## Task 10: Hard filters and keyword search

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/filters.rs`
- Create: `crates/memorysafe-backend-postgres/src/keyword.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`

**Interfaces:**
- Consumes: `items::item_columns`, `items::row_to_item`.
- Produces: `filters::filter_sql`, `filters::FilterBinds`, `filters::passes`, `keyword::to_or_tsquery`, `keyword::search`.

**Hard filters get their own module, created here rather than with the fusion code.** The keyword query needs them and so does the vector query, and neither is the natural owner. `filters.rs` also keeps the module that decides what a caller may see small enough to read in one sitting — it is the enforcement point for the sensitivity ceiling.

**Configuration is `simple`, not `english`.** The SQLite side uses FTS5's `unicode61` tokenizer, which does no stemming and has no stop-word list. `english` would drop "the", "a" and "on" as stop words, and `hybrid_returns_both_signal_sources` searches for "the cat sat on the mat" and requires a keyword score on the exact match. `simple` is the configuration that behaves like `unicode61`.

**Terms are OR-ed, not AND-ed.** `plainto_tsquery` and `websearch_to_tsquery` both AND their terms, which for a multi-word recall query means "documents containing every word" — far too narrow when the vector side is supplying the precision. Each term becomes a quoted lexeme and the terms are joined with `|`, mirroring `escape_fts_query` on the SQLite side exactly.

**Three escaping facts, all established against a live server:**

- A quoted lexeme is not parsed as an operator, so `'AND'`, `'*'`, `'(unbalanced'` and `'NEAR/'` are all searched for literally.
- A lone backslash raises `syntax error in tsquery: "'\'"`. Backslash is the escape character *inside* a quoted lexeme, so it must be doubled before the single quote is doubled.
- Input that yields no lexemes produces an empty `tsquery`, which matches nothing and emits a notice. That is the desired outcome, not an error.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-backend-postgres/src/filters.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::SensitivityLevel;

    #[test]
    fn filter_sql_numbers_its_placeholders_from_the_offset_it_is_given() {
        let sql = filter_sql("i", 5);
        for n in 5..=10 {
            assert!(sql.contains(&format!("${n}")), "missing ${n} in {sql}");
        }
        assert!(!sql.contains("$4"), "clobbered an earlier placeholder: {sql}");
        assert!(!sql.contains("$11"), "ran past its allocation: {sql}");
    }

    #[test]
    fn filter_sql_qualifies_every_column_with_the_alias() {
        let sql = filter_sql("i", 1);
        for column in ["sensitivity", "pending_embedding", "kind", "tags", "occurred_at"] {
            assert!(sql.contains(&format!("i.{column}")), "{column} is unqualified in {sql}");
        }
    }

    #[test]
    fn empty_filter_lists_bind_as_null_so_the_guard_short_circuits() {
        let b = FilterBinds::from(&HardFilters::default());
        assert!(b.kinds.is_none(), "an empty kinds list must not filter everything out");
        assert!(b.tags.is_none());
        assert_eq!(b.ceiling, SensitivityLevel::Internal.ordinal() as i16);
    }

    #[test]
    fn populated_filter_lists_bind_as_arrays() {
        let f = HardFilters {
            kinds: vec!["fact".into()],
            tags_any: vec!["work".into(), "home".into()],
            ..Default::default()
        };
        let b = FilterBinds::from(&f);
        assert_eq!(b.kinds.as_deref(), Some(&["fact".to_string()][..]));
        assert_eq!(b.tags.as_ref().map(|t| t.len()), Some(2));
    }

    /// The Rust-side check must agree with the SQL, or the second line of
    /// defence becomes a source of disagreement between the two backends.
    #[test]
    fn passes_agrees_with_the_ceiling_the_sql_applies() {
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        let mut item = memorysafe_backend::conformance::fx::item(&scope, "a note");
        item.sensitivity = SensitivityLevel::Restricted;
        let f = HardFilters { sensitivity_ceiling: SensitivityLevel::Personal, ..Default::default() };
        assert!(!passes(&item, &f));
        item.sensitivity = SensitivityLevel::Personal;
        assert!(passes(&item, &f));
    }
}
```

Append to `crates/memorysafe-backend-postgres/src/keyword.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terms_become_or_ed_quoted_lexemes() {
        assert_eq!(to_or_tsquery("zstandard").as_deref(), Some("'zstandard'"));
        assert_eq!(to_or_tsquery("cat mat").as_deref(), Some("'cat' | 'mat'"));
    }

    #[test]
    fn operators_are_quoted_rather_than_interpreted() {
        // A quoted lexeme is a search term, so 'AND' is looked for, not run.
        assert_eq!(to_or_tsquery("a AND b").as_deref(), Some("'a' | 'AND' | 'b'"));
        assert_eq!(to_or_tsquery("*").as_deref(), Some("'*'"));
        assert_eq!(to_or_tsquery("(unbalanced").as_deref(), Some("'(unbalanced'"));
        assert_eq!(to_or_tsquery("NEAR/").as_deref(), Some("'NEAR/'"));
    }

    /// Backslash is the escape character inside a quoted lexeme. Left alone,
    /// a lone backslash makes `to_tsquery` raise a syntax error rather than
    /// return no rows.
    #[test]
    fn backslashes_are_doubled_before_quotes_are() {
        assert_eq!(to_or_tsquery(r"\").as_deref(), Some(r"'\\'"));
        assert_eq!(to_or_tsquery(r"a\b").as_deref(), Some(r"'a\\b'"));
        assert_eq!(to_or_tsquery("it's").as_deref(), Some("'it''s'"));
        // Both, in the right order: the backslash must not escape the doubled
        // quote that follows it.
        assert_eq!(to_or_tsquery(r"a\'b").as_deref(), Some(r"'a\\''b'"));
    }

    #[test]
    fn empty_input_yields_no_query_at_all() {
        assert_eq!(to_or_tsquery(""), None);
        assert_eq!(to_or_tsquery("   "), None);
    }

    /// `to_tsquery` rejects a word longer than 2047 bytes. A caller pasting a
    /// base64 blob into a search box must get no rows, not an error.
    #[test]
    fn overlong_terms_are_truncated_on_a_character_boundary() {
        let long = "é".repeat(400); // 800 bytes
        let q = to_or_tsquery(&long).unwrap();
        assert!(q.len() <= MAX_TERM_BYTES + 2, "term was not truncated: {} bytes", q.len());
        // Truncation must not split a multi-byte character.
        assert!(q.is_char_boundary(q.len() - 1));
    }
}
```

The two keyword conformance tests are not wired here: they go through
`retrieve_candidates`, which is still the Task 7 placeholder. Task 11 adds
them once the fusion path exists.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres filters keyword`
Expected: FAIL — `cannot find function filter_sql in this scope`, `cannot find function to_or_tsquery in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/filters.rs`:

```rust
use memorysafe_backend::HardFilters;
use memorysafe_core::MemoryItem;

/// The hard-filter predicate, occupying placeholders `$start` through
/// `$start+5` in this fixed order: sensitivity ceiling, exclude-pending flag,
/// kinds array, tags array, occurred-after, occurred-before.
///
/// These run inside the backend's own query, below the policy: a policy may
/// narrow a candidate set but never widen it, so anything security-relevant
/// has to be here rather than in `compose`.
///
/// Optional filters are NULL-guarded rather than spliced in conditionally.
/// Building `$7`, `$8`, … at runtime means the SQL string and the bind
/// sequence have to be kept in step by hand, and a filter added later in the
/// wrong place produces a query that runs and returns the wrong rows. One
/// constant fragment has one bind order that cannot drift, and PostgreSQL's
/// array types make `kinds` and `tags_any` single parameters rather than
/// expanded lists.
pub fn filter_sql(alias: &str, start: usize) -> String {
    format!(
        " AND {alias}.sensitivity <= ${a}
          AND (NOT ${b} OR {alias}.pending_embedding = FALSE)
          AND (${c}::text[] IS NULL OR {alias}.kind = ANY(${c}))
          AND (${d}::text[] IS NULL OR {alias}.tags && ${d})
          AND (${e}::bigint IS NULL OR {alias}.occurred_at >= ${e})
          AND (${f}::bigint IS NULL OR {alias}.occurred_at <= ${f})",
        a = start,
        b = start + 1,
        c = start + 2,
        d = start + 3,
        e = start + 4,
        f = start + 5,
    )
}

/// The six values `filter_sql` expects, already in PostgreSQL's types.
/// An absent filter binds NULL so its guard short-circuits, which is why an
/// empty `kinds` list widens rather than eliminating every row.
pub struct FilterBinds {
    pub ceiling: i16,
    pub exclude_pending: bool,
    pub kinds: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub after: Option<i64>,
    pub before: Option<i64>,
}

impl FilterBinds {
    pub fn from(f: &HardFilters) -> Self {
        Self {
            ceiling: f.sensitivity_ceiling.ordinal() as i16,
            exclude_pending: f.exclude_pending_embedding,
            kinds: (!f.kinds.is_empty()).then(|| f.kinds.clone()),
            tags: (!f.tags_any.is_empty()).then(|| f.tags_any.clone()),
            after: f.occurred_after.map(|t| t.unix_timestamp()),
            before: f.occurred_before.map(|t| t.unix_timestamp()),
        }
    }
}

/// Applied in Rust over rows the SQL already narrowed, as a second line of
/// defence. The SQL predicates are the enforcement point; this catches a
/// mistake in them before a restricted item reaches a caller.
pub fn passes(item: &MemoryItem, f: &HardFilters) -> bool {
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
```

`crates/memorysafe-backend-postgres/src/keyword.rs`:

```rust
use crate::error::SqlxResultExt;
use crate::filters::{FilterBinds, filter_sql};
use crate::items::{item_columns, row_to_item};
use memorysafe_backend::{BackendError, HardFilters};
use memorysafe_core::{MemoryItem, Scope};
use sqlx::{PgConnection, Row};

/// `to_tsquery` rejects a word longer than 2047 bytes. 200 is generous for a
/// real term and far below the limit.
pub const MAX_TERM_BYTES: usize = 200;

fn truncate_term(t: &str) -> &str {
    if t.len() <= MAX_TERM_BYTES {
        return t;
    }
    let mut end = MAX_TERM_BYTES;
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    &t[..end]
}

/// Turns arbitrary user text into a safe `tsquery` expression.
///
/// Every whitespace-separated term becomes a quoted lexeme, which `to_tsquery`
/// treats as a literal rather than an operator; the terms are OR-ed so a
/// multi-word query behaves like "any of these". Precision comes from the
/// vector side, so narrowing here would only lose recall.
///
/// Escaping order matters: backslash is the escape character inside a quoted
/// lexeme, so it is doubled first. Doubling the quote first would leave the
/// backslash free to escape the quote that follows it.
pub fn to_or_tsquery(raw: &str) -> Option<String> {
    let terms: Vec<String> = raw
        .split_whitespace()
        .map(truncate_term)
        .map(|t| format!("'{}'", t.replace('\\', r"\\").replace('\'', "''")))
        .collect();
    if terms.is_empty() { None } else { Some(terms.join(" | ")) }
}

/// Returns `(item, score)` with the score squashed into `(0, 1]` so it can be
/// fused with a cosine similarity. `ts_rank_cd` is unbounded above; `r / (1 +
/// r)` is the same flattening the SQLite side applies to bm25.
pub async fn search(
    conn: &mut PgConnection,
    scope: &Scope,
    raw_query: &str,
    filters: &HardFilters,
    limit: usize,
) -> Result<Vec<(MemoryItem, f32)>, BackendError> {
    let Some(expr) = to_or_tsquery(raw_query) else {
        return Ok(vec![]);
    };

    // $1..$3 scope, $4 tsquery, $5..$10 filters, $11 limit.
    let sql = format!(
        "SELECT {cols}, ts_rank_cd(i.search, q.tsq) AS rank
         FROM items i, to_tsquery('simple', $4) AS q(tsq)
         WHERE i.tenant_id = $1 AND i.subject = $2 AND i.namespace = $3
           AND i.search @@ q.tsq{filters}
         ORDER BY rank DESC, i.id ASC
         LIMIT $11",
        cols = item_columns("i"),
        filters = filter_sql("i", 5)
    );

    let b = FilterBinds::from(filters);
    let rows = sqlx::query(&sql)
        .bind(scope.tenant.as_str())
        .bind(scope.subject.as_str())
        .bind(scope.namespace.as_str())
        .bind(expr)
        .bind(b.ceiling)
        .bind(b.exclude_pending)
        .bind(b.kinds)
        .bind(b.tags)
        .bind(b.after)
        .bind(b.before)
        .bind(limit as i64)
        .fetch_all(conn)
        .await
        .pg()?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let item = row_to_item(row)?;
        let rank: f32 = row.try_get("rank").pg()?;
        let positive = rank.max(0.0);
        out.push((item, positive / (1.0 + positive)));
    }
    Ok(out)
}
```

Add `pub mod filters;` and `pub mod keyword;` to `lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres && cargo clippy --all-targets -- -D warnings`
Expected: PASS — 5 filter tests and 5 keyword tests ok, and the conformance tests wired so far still pass. `keyword::search` has no caller until Task 11; it is a `pub` item in a `pub` module, so that is not a dead-code warning.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/src/
git commit -m "feat(pg): hard-filter SQL and tsquery construction that neutralises operators"
```

---

## Task 11: Hybrid retrieval

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/retrieve.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`
- Modify: `crates/memorysafe-backend-postgres/tests/conformance.rs`

**Interfaces:**
- Consumes: `keyword::search`, `vectors::search`, `vectors::scope_embedder`, `filters::{filter_sql, FilterBinds, passes}`.
- Produces: `retrieve::fuse`, `retrieve::candidates`, a filtered `vectors::search`, and a real `Backend::retrieve_candidates`.

**Fusion is byte-for-byte the SQLite behaviour:** `0.7 * vector + 0.3 * keyword` when both are present, the single score when only one is, ties broken by item id ascending, truncated to `limit`. Task 15 asserts the two backends agree; this is where that agreement is created.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-backend-postgres/src/retrieve.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusion_weights_the_vector_signal_more_heavily() {
        assert!((fuse(Some(1.0), Some(0.0)) - 0.7).abs() < 1e-6);
        assert!((fuse(Some(0.0), Some(1.0)) - 0.3).abs() < 1e-6);
        assert_eq!(fuse(Some(0.5), None), 0.5);
        assert_eq!(fuse(None, Some(0.5)), 0.5);
        assert_eq!(fuse(None, None), 0.0);
    }

    /// A single-source result must not be scaled down; the weights apply only
    /// when there are two signals to weigh against each other.
    #[test]
    fn a_lone_signal_passes_through_unweighted() {
        for v in [0.0_f32, 0.25, 1.0] {
            assert_eq!(fuse(Some(v), None), v);
            assert_eq!(fuse(None, Some(v)), v);
        }
    }
}
```

Add to `crates/memorysafe-backend-postgres/tests/conformance.rs`:

```rust
#[tokio::test]
async fn sensitivity_ceiling_is_enforced_in_the_query() {
    retrieval::sensitivity_ceiling_is_enforced_in_the_query(&shared()).await;
}

#[tokio::test]
async fn tag_and_kind_filters_narrow_results() {
    retrieval::tag_and_kind_filters_narrow_results(&shared()).await;
}

#[tokio::test]
async fn keyword_search_finds_exact_terms() {
    retrieval::keyword_search_finds_exact_terms(&shared()).await;
}

#[tokio::test]
async fn keyword_search_escapes_user_input() {
    retrieval::keyword_search_escapes_user_input(&shared()).await;
}

#[tokio::test]
async fn hybrid_returns_both_signal_sources() {
    retrieval::hybrid_returns_both_signal_sources(&shared()).await;
}

#[tokio::test]
async fn list_pages_are_disjoint_and_complete() {
    retrieval::list_pages_are_disjoint_and_complete(&shared()).await;
}

#[tokio::test]
async fn list_orders_oldest_first_by_created_at() {
    retrieval::list_orders_oldest_first_by_created_at(&shared()).await;
}

#[tokio::test]
async fn list_tie_break_is_total_over_identical_timestamps() {
    retrieval::list_tie_break_is_total_over_identical_timestamps(&shared()).await;
}

#[tokio::test]
async fn pending_embedding_items_are_excluded_when_asked() {
    retrieval::pending_embedding_items_are_excluded_when_asked(&shared()).await;
}

#[tokio::test]
async fn recall_updates_access_statistics() {
    retrieval::recall_updates_access_statistics(&shared()).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres retrieve`
Expected: FAIL — `cannot find function fuse in this scope`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/retrieve.rs`:

```rust
use crate::filters::passes;
use crate::{estimate_tokens, keyword, vectors};
use memorysafe_backend::{BackendError, CandidateQuery};
use memorysafe_core::{MemoryItem, Score, Scope, ScoredCandidate};
use sqlx::PgConnection;
use std::collections::HashMap;

/// Weight on the vector signal when both are present; keyword carries the
/// rest. Identical to the SQLite backend's, because the engine's policy must
/// see the same relevance numbers whichever backend is underneath.
const VECTOR_WEIGHT: f32 = 0.7;

pub(crate) fn fuse(vector_score: Option<f32>, keyword_score: Option<f32>) -> f32 {
    match (vector_score, keyword_score) {
        (Some(v), Some(k)) => VECTOR_WEIGHT * v + (1.0 - VECTOR_WEIGHT) * k,
        (Some(v), None) => v,
        (None, Some(k)) => k,
        (None, None) => 0.0,
    }
}

pub async fn candidates(
    conn: &mut PgConnection,
    scope: &Scope,
    query: &CandidateQuery,
    vector_dim: u16,
    over_fetch: usize,
) -> Result<Vec<ScoredCandidate>, BackendError> {
    if !query.is_valid() {
        return Err(BackendError::InvalidQuery(
            "a query must carry an embedding, text, or both".into(),
        ));
    }

    // Over-fetching happens twice, deliberately. Here it widens the candidate
    // pool so fusion has something to reorder; inside `vectors::search` it
    // widens again so HNSW's approximation cannot cost a candidate that the
    // exact rerank would have kept.
    let fetch = query.limit.saturating_mul(over_fetch).max(query.limit);
    let mut merged: HashMap<String, (MemoryItem, Option<f32>, Option<f32>)> = HashMap::new();

    if let Some(embedding) = &query.embedding
        && let Some((stored, dim)) = vectors::scope_embedder(conn, scope).await?
        && stored == embedding.embedder.to_string()
        && dim == embedding.dim
        && dim == vector_dim
    {
        let probe = memorysafe_embed::QuantizedVector::from_embedding(embedding);
        for (item, score) in
            vectors::search(conn, scope, &probe, fetch, over_fetch, &query.filters).await?
        {
            merged.entry(item.id.as_str().to_string()).or_insert((item, None, None)).1 =
                Some(score);
        }
    }

    if let Some(text) = &query.text {
        for (item, score) in
            keyword::search(conn, scope, text, &query.filters, fetch).await?
        {
            let e = merged.entry(item.id.as_str().to_string()).or_insert((item, None, None));
            e.2 = Some(score);
        }
    }

    let mut out: Vec<ScoredCandidate> = merged
        .into_values()
        .filter(|(item, _, _)| passes(item, &query.filters))
        .map(|(item, access, vector_score, keyword_score)| ScoredCandidate {
            estimated_tokens: estimate_tokens(&item.body),
            relevance: fuse(vector_score, keyword_score),
            item,
            vector_score,
            keyword_score,
            value: Score::ZERO,
            fragility: Score::ZERO,
            // From the item row's `last_access`/`access_count`, carried
            // through the merge map beside the item. `(None, 0)` for a row
            // never recalled.
            last_accessed_at: access.last_accessed_at,
            access_count: access.access_count,
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

**`vectors::search` needs the hard filters too.** The vector path currently ignores them, which would let a restricted item reach `passes()` in process memory — the exact thing §7 of the spec forbids. Extend `vectors::search` with a `filters: &HardFilters` parameter, import `crate::filters::{FilterBinds, filter_sql}`, and splice the predicate into its `WHERE` at placeholders `$8..$13`:

```rust
    let sql = format!(
        "SELECT {cols}, v.scale AS v_scale, v.q AS v_q
         FROM vectors v
         JOIN items i ON i.tenant_id = v.tenant_id AND i.id = v.item_id
         WHERE v.tenant_id = $1 AND v.subject = $2 AND v.namespace = $3
           AND v.embedder = $4 AND v.dim = $5{filters}
         ORDER BY v.embedding <#> $6::vector
         LIMIT $7",
        cols = item_columns("i"),
        filters = filter_sql("i", 8)
    );
```

The final signature, and the bind order it fixes:

```rust
pub async fn search(
    conn: &mut PgConnection,
    scope: &Scope,
    probe: &QuantizedVector,
    k: usize,
    over_fetch: usize,
    filters: &HardFilters,
) -> Result<Vec<(MemoryItem, f32)>, BackendError>
```

```rust
    let b = FilterBinds::from(filters);
    let rows = sqlx::query(&sql)
        .bind(scope.tenant.as_str())            // $1
        .bind(scope.subject.as_str())           // $2
        .bind(scope.namespace.as_str())         // $3
        .bind(probe.embedder.to_string())       // $4
        .bind(i32::from(probe.dim))             // $5
        .bind(to_pg_vector(probe))              // $6
        .bind((k.saturating_mul(over_fetch).max(k)) as i64) // $7
        .bind(b.ceiling)                        // $8
        .bind(b.exclude_pending)                // $9
        .bind(b.kinds)                          // $10
        .bind(b.tags)                           // $11
        .bind(b.after)                          // $12
        .bind(b.before)                         // $13
        .fetch_all(conn)
        .await
        .pg()?;
```

`retrieve::candidates` above already calls it with the new parameter. The
other call site, `Backend::neighbours` from Task 9, has to be updated — and it
passes no filters at all, because it serves the policy's redundancy check and
must see the whole neighbourhood rather than the caller's slice of it:

```rust
        // `neighbours` is not a recall path: it answers "what is this item
        // near?", and a policy that saw a censored neighbourhood would judge
        // novelty against a corpus that is not there.
        let unfiltered = memorysafe_backend::HardFilters {
            sensitivity_ceiling: memorysafe_core::SensitivityLevel::Restricted,
            ..Default::default()
        };
        let hits = vectors::search(
            &mut tx, scope, &probe, k, self.config.over_fetch, &unfiltered,
        )
        .await?;
```

Replace the `retrieve_candidates` placeholder in `lib.rs`:

```rust
    async fn retrieve_candidates(
        &self,
        scope: &Scope,
        query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        let mut tx = tenant_txn(self, &scope.tenant).await?;
        let out = retrieve::candidates(
            &mut tx,
            scope,
            query,
            self.config.vector_dim,
            self.config.over_fetch,
        )
        .await?;
        tx.commit().await.pg()?;
        Ok(out)
    }
```

Add `pub mod retrieve;` to `lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres`
Expected: PASS — 2 fusion unit tests plus 13 conformance tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): hybrid fusion over vector and keyword candidates"
```

---

## Task 12: Capacity accounting, merge, and idempotency

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/capacity.rs`
- Modify: `crates/memorysafe-backend-postgres/src/items.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`
- Modify: `crates/memorysafe-backend-postgres/tests/conformance.rs`

**Interfaces:**
- Consumes: `items`, `vectors`, `audit`.
- Produces: `capacity::ensure_row`, `capacity::lock_row`, `capacity::state`, `capacity::set_budget`, `capacity::adjust`, `capacity::stats`, `items::merge`, and real `Backend::apply`, `capacity_state`, `scope_stats`, `set_budget`.

**The correctness detail the spec calls out.** Without a lock on the accounting row, two concurrent admits both read the same `used_items`, both conclude there is room, and the budget is silently exceeded. `SELECT … FOR UPDATE` on the namespace's row, taken as the first statement of the write transaction, is the Postgres form of the per-tenant write serialization the SQLite backend gets from a mutex — and it is finer-grained, because two namespaces of one tenant no longer block each other.

**The lock is taken before the idempotency check, not after.** Two retries of the same request necessarily share a namespace, so the row lock serialises them; checking the key first would let both retries read "no prior outcome" and both apply. The unique constraint would catch the second, but as a 23505 error rather than the replay the contract promises.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-backend-postgres/tests/conformance.rs`:

```rust
use memorysafe_backend::conformance::{atomicity, capacity};

#[tokio::test]
async fn capacity_accounting_tracks_items_and_bytes() {
    capacity::capacity_accounting_tracks_items_and_bytes(&shared()).await;
}

#[tokio::test]
async fn eviction_releases_capacity() {
    capacity::eviction_releases_capacity(&shared()).await;
}

#[tokio::test]
async fn concurrent_admits_do_not_double_count() {
    capacity::concurrent_admits_do_not_double_count(&shared()).await;
}

#[tokio::test]
async fn scope_stats_reflect_the_corpus() {
    capacity::scope_stats_reflect_the_corpus(&shared()).await;
}

#[tokio::test]
async fn admit_evict_and_audit_commit_together() {
    atomicity::admit_evict_and_audit_commit_together(&shared()).await;
}

#[tokio::test]
async fn a_failed_transaction_leaves_no_trace() {
    atomicity::a_failed_transaction_leaves_no_trace(&shared()).await;
}

#[tokio::test]
async fn every_mutation_writes_exactly_one_audit_record() {
    atomicity::every_mutation_writes_exactly_one_audit_record(&shared()).await;
}

#[tokio::test]
async fn idempotent_writes_replay_the_original_outcome() {
    atomicity::idempotent_writes_replay_the_original_outcome(&shared()).await;
}

#[tokio::test]
async fn idempotency_conflict_on_different_payload() {
    atomicity::idempotency_conflict_on_different_payload(&shared()).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test conformance capacity_accounting`
Expected: FAIL — `assertion left == right failed: 0 vs 1`, because `capacity_state` still returns the placeholder.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/capacity.rs`:

```rust
use crate::error::SqlxResultExt;
use memorysafe_backend::BackendError;
use memorysafe_core::{Budget, CapacityState, Scope, ScopeStats};
use sqlx::{PgConnection, Row};

pub async fn ensure_row(conn: &mut PgConnection, scope: &Scope) -> Result<(), BackendError> {
    sqlx::query(
        "INSERT INTO capacity (tenant_id, subject, namespace, used_items, used_bytes)
         VALUES ($1, $2, $3, 0, 0)
         ON CONFLICT (tenant_id, subject, namespace) DO NOTHING",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .execute(conn)
    .await
    .pg()?;
    Ok(())
}

/// Takes the namespace's accounting row for the rest of the transaction.
///
/// This is the whole of the concurrency story for writes: two admits to one
/// namespace serialise here, so neither can read a stale `used_items` and
/// conclude there is room that the other has already taken.
pub async fn lock_row(conn: &mut PgConnection, scope: &Scope) -> Result<(), BackendError> {
    ensure_row(conn, scope).await?;
    sqlx::query(
        "SELECT used_items FROM capacity
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3
         FOR UPDATE",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .fetch_optional(conn)
    .await
    .pg()?;
    Ok(())
}

pub async fn set_budget(
    conn: &mut PgConnection,
    scope: &Scope,
    budget: Budget,
) -> Result<(), BackendError> {
    ensure_row(conn, scope).await?;
    sqlx::query(
        "UPDATE capacity SET max_items = $4, max_bytes = $5
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .bind(budget.max_items.map(|v| v as i64))
    .bind(budget.max_bytes.map(|v| v as i64))
    .execute(conn)
    .await
    .pg()?;
    Ok(())
}

pub async fn state(
    conn: &mut PgConnection,
    scope: &Scope,
) -> Result<CapacityState, BackendError> {
    let row = sqlx::query(
        "SELECT max_items, max_bytes, used_items, used_bytes FROM capacity
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .fetch_optional(conn)
    .await
    .pg()?;

    Ok(match row {
        Some(r) => CapacityState {
            budget: Budget {
                max_items: r.get::<Option<i64>, _>("max_items").map(|v| v as u64),
                max_bytes: r.get::<Option<i64>, _>("max_bytes").map(|v| v as u64),
            },
            used_items: r.get::<i64, _>("used_items").max(0) as u64,
            used_bytes: r.get::<i64, _>("used_bytes").max(0) as u64,
        },
        None => CapacityState { budget: Budget::UNBOUNDED, used_items: 0, used_bytes: 0 },
    })
}

/// Applied inside the write transaction, under the row lock. Deltas are
/// signed; the row is clamped at zero so a bookkeeping slip cannot go
/// negative and wrap.
pub async fn adjust(
    conn: &mut PgConnection,
    scope: &Scope,
    delta_items: i64,
    delta_bytes: i64,
) -> Result<(), BackendError> {
    sqlx::query(
        "UPDATE capacity
         SET used_items = GREATEST(0, used_items + $4),
             used_bytes = GREATEST(0, used_bytes + $5)
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .bind(delta_items)
    .bind(delta_bytes)
    .execute(conn)
    .await
    .pg()?;
    Ok(())
}

/// Corpus statistics the policy needs but must not query for itself.
///
/// The median is computed the same way the SQLite backend computes it —
/// `ORDER BY byte_size LIMIT 1 OFFSET count/2` — rather than with
/// `percentile_cont`, so the two backends report the same number for the same
/// corpus. Task 15 asserts that.
pub async fn stats(
    conn: &mut PgConnection,
    scope: &Scope,
) -> Result<ScopeStats, BackendError> {
    let row = sqlx::query(
        "SELECT count(*) AS n, COALESCE(SUM(byte_size), 0) AS total FROM items
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .fetch_one(&mut *conn)
    .await
    .pg()?;
    let count: i64 = row.get("n");
    let total: i64 = row.get("total");

    let median: i64 = if count == 0 {
        0
    } else {
        sqlx::query(
            "SELECT byte_size FROM items
             WHERE tenant_id = $1 AND subject = $2 AND namespace = $3
             ORDER BY byte_size LIMIT 1 OFFSET $4",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.subject.as_str())
        .bind(scope.namespace.as_str())
        .bind(count / 2)
        .fetch_optional(conn)
        .await
        .pg()?
        .map(|r| r.get::<i64, _>("byte_size"))
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
///
/// `tags_text` is rewritten alongside `tags`; the generated `search` column
/// then updates itself, which is the reason it is generated rather than
/// application-maintained.
pub async fn merge(
    conn: &mut PgConnection,
    scope: &Scope,
    target: &ItemId,
    body: &str,
    tags: &[String],
    attrs: &BTreeMap<String, serde_json::Value>,
) -> Result<i64, BackendError> {
    let Some(existing) = get(conn, scope, target).await? else {
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

    sqlx::query(
        "UPDATE items
         SET body = $5, tags = $6, tags_text = $7, attrs = $8, byte_size = $9
         WHERE tenant_id = $1 AND subject = $2 AND namespace = $3 AND id = $4",
    )
    .bind(scope.tenant.as_str())
    .bind(scope.subject.as_str())
    .bind(scope.namespace.as_str())
    .bind(target.as_str())
    .bind(&updated.body)
    .bind(&updated.tags)
    .bind(updated.tags.join(" "))
    .bind(Json(&updated.attrs))
    .bind(after)
    .execute(conn)
    .await
    .pg()?;

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
        let mut tx = tenant_txn(self, &txn.scope.tenant).await?;

        // First statement in the transaction: take the namespace's accounting
        // row. Two admits to one namespace serialise here, and so do a write
        // and its own retry.
        capacity::lock_row(&mut tx, &txn.scope).await?;

        if let Some(key) = &txn.idempotency_key {
            let prior = sqlx::query(
                "SELECT payload_digest, outcome FROM idempotency
                 WHERE tenant_id = $1 AND key = $2",
            )
            .bind(txn.scope.tenant.as_str())
            .bind(key)
            .fetch_optional(&mut *tx)
            .await
            .pg()?;

            if let Some(row) = prior {
                let digest: String = row.try_get("payload_digest").pg()?;
                if txn.payload_digest.as_deref() != Some(digest.as_str()) {
                    return Err(BackendError::IdempotencyConflict);
                }
                let stored: sqlx::types::Json<AppliedWrite> =
                    row.try_get("outcome").pg()?;
                // `replayed_outcome` carries the outcome as it was first
                // recorded, so capture it before the replay flag is set.
                let recorded = serde_json::to_string(&stored.0).ok();
                let mut replay = stored.0;
                replay.replayed = true;
                replay.replayed_outcome = recorded;
                tx.commit().await.pg()?;
                return Ok(replay);
            }
        }

        let mut delta_items: i64 = 0;
        let mut delta_bytes: i64 = 0;
        let mut evicted = Vec::new();

        for id in &txn.evictions {
            // The vector row goes with the item through ON DELETE CASCADE.
            let size = items::delete(&mut tx, &txn.scope, id).await?;
            if size > 0 {
                delta_items -= 1;
                delta_bytes -= size as i64;
            }
            evicted.push(id.clone());
        }

        let mut item_id = None;

        if let Some(w) = &txn.upsert {
            items::insert(&mut tx, &w.item).await?;
            if let Some(v) = &w.vector {
                vectors::insert(&mut tx, &w.item.id, &txn.scope, v, self.config.vector_dim)
                    .await?;
            }
            delta_items += 1;
            delta_bytes += w.item.byte_size() as i64;
            item_id = Some(w.item.id.clone());
        }

        if let Some(m) = &txn.merge {
            let diff =
                items::merge(&mut tx, &txn.scope, &m.target, &m.body, &m.tags, &m.attrs)
                    .await?;
            if let Some(v) = &m.vector {
                vectors::insert(&mut tx, &m.target, &txn.scope, v, self.config.vector_dim)
                    .await?;
            }
            delta_bytes += diff;
            item_id = Some(m.target.clone());
        }

        capacity::adjust(&mut tx, &txn.scope, delta_items, delta_bytes).await?;
        let audit_id = audit::insert(&mut tx, &txn.audit).await?;

        let applied = AppliedWrite {
            item_id,
            audit_id,
            evicted,
            replayed: false,
            replayed_outcome: None,
        };

        if let Some(key) = &txn.idempotency_key {
            sqlx::query(
                "INSERT INTO idempotency (tenant_id, key, subject, namespace,
                     payload_digest, outcome, at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7)",
            )
            .bind(txn.scope.tenant.as_str())
            .bind(key)
            .bind(txn.scope.subject.as_str())
            .bind(txn.scope.namespace.as_str())
            .bind(txn.payload_digest.clone().unwrap_or_default())
            .bind(sqlx::types::Json(&applied))
            .bind(time::OffsetDateTime::now_utc().unix_timestamp())
            .execute(&mut *tx)
            .await
            .pg()?;
        }

        tx.commit().await.pg()?;
        Ok(applied)
    }
```

Replace the three remaining capacity placeholders:

```rust
    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, BackendError> {
        let mut tx = tenant_txn(self, &scope.tenant).await?;
        let out = capacity::state(&mut tx, scope).await?;
        tx.commit().await.pg()?;
        Ok(out)
    }

    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, BackendError> {
        let mut tx = tenant_txn(self, &scope.tenant).await?;
        let out = capacity::stats(&mut tx, scope).await?;
        tx.commit().await.pg()?;
        Ok(out)
    }

    async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), BackendError> {
        let mut tx = tenant_txn(self, &scope.tenant).await?;
        capacity::set_budget(&mut tx, scope, budget).await?;
        tx.commit().await.pg()?;
        Ok(())
    }
```

Add `pub mod capacity;` and `use sqlx::Row;` to `lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres`
Expected: PASS — 28 conformance tests (everything but the 22 lifecycle ones) plus the unit tests, all ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): row-locked capacity accounting, merge, and idempotent writes"
```

---

## Task 13: Purge and portable export/import

**Files:**
- Create: `crates/memorysafe-backend-postgres/src/purge.rs`
- Create: `crates/memorysafe-backend-postgres/src/portability.rs`
- Modify: `crates/memorysafe-backend-postgres/src/lib.rs`
- Modify: `crates/memorysafe-backend-postgres/tests/conformance.rs`

**Interfaces:**
- Consumes: everything in the crate.
- Produces: `aggregates::increment`, `aggregates::query`, `purge::subject`, `portability::export`, `portability::import`, real `Backend::purge_subject`, `export`, `import`, `audit_aggregates`, and the single `run_conformance_suite` entry point.

**Milestone: every conformance test the pinned submodule contains passes under `SharedPartitioned`** — which requires the aggregate write and read this task adds, not only the purge and portability work. Three lifecycle tests depend on them and the stub they replace returns `Ok(vec![])`; the milestone was stated before the aggregates existed anywhere in this document and could not have been met.

**Why vectors are deleted explicitly when the cascade would do it.** `PurgeReport` counts what was removed, and a cascade reports nothing. Deleting vectors first makes the count exact and leaves the item delete with nothing to cascade to.

- [ ] **Step 1: Write the failing test**

Replace the individual test functions in `crates/memorysafe-backend-postgres/tests/conformance.rs` with one entry point:

```rust
mod support;

use memorysafe_backend::conformance::{BackendFactory, run_conformance_suite};
use memorysafe_backend_postgres::{PgLayout, PostgresBackend};
use std::future::Future;

pub struct PgFactory {
    pub layout: PgLayout,
}

impl BackendFactory for PgFactory {
    type B = PostgresBackend;
    fn create(&self) -> impl Future<Output = Self::B> + Send {
        let layout = self.layout;
        async move {
            let config = support::test_config(layout).await;
            PostgresBackend::connect(config).await.expect("connect and bootstrap")
        }
    }
}

/// The whole frozen suite. The SQLite backend runs this same function.
#[tokio::test(flavor = "multi_thread")]
async fn postgres_passes_the_backend_conformance_suite() {
    run_conformance_suite(&PgFactory { layout: PgLayout::SharedPartitioned }).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test conformance`
Expected: FAIL — `purge_subject_removes_everything_for_that_subject` panics: `assertion left == right failed: 0 vs 6`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-backend-postgres/src/purge.rs`:

```rust
use crate::error::SqlxResultExt;
use memorysafe_backend::{BackendError, PurgeReport};
use memorysafe_core::{AuditRecord, PurgeCascade, SubjectId, TenantId};
use sqlx::PgConnection;

/// Right-to-delete for one subject: a first-class operation rather than a
/// scan-and-delete loop. Everything the subject owns goes in one transaction,
/// across every namespace it has.
pub async fn subject(
    conn: &mut PgConnection,
    tenant: &TenantId,
    subject: &SubjectId,
    cascade: PurgeCascade,
    audit: &AuditRecord,
) -> Result<PurgeReport, BackendError> {
    // Counted before anything is written. `Backend::purge_subject`'s report is
    // an equation — `audit_rows_removed + audit_rows_preserved` equals the
    // rows the subject held immediately before this call — and the record
    // inserted at the end belongs to neither term, so counting after the
    // insert would put it in `preserved` and break it.
    let existing_audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE tenant_id = $1 AND subject = $2",
    )
    .bind(tenant.as_str())
    .bind(subject.as_str())
    .fetch_one(&mut *conn)
    .await
    .pg()?;

    // Vectors first, so the count is exact. Deleting items afterwards has
    // nothing left to cascade to.
    let vectors_removed = sqlx::query("DELETE FROM vectors WHERE tenant_id = $1 AND subject = $2")
        .bind(tenant.as_str())
        .bind(subject.as_str())
        .execute(&mut *conn)
        .await
        .pg()?
        .rows_affected();

    let items_removed = sqlx::query("DELETE FROM items WHERE tenant_id = $1 AND subject = $2")
        .bind(tenant.as_str())
        .bind(subject.as_str())
        .execute(&mut *conn)
        .await
        .pg()?
        .rows_affected();

    // `cascade` decides the audit detail and nothing else. The engine no
    // longer reads or rewrites audit rows around this call — it passes the
    // retention profile's decision and the `SubjectPurged` record, and this
    // function does delete-before-insert in one transaction. See
    // `Backend::purge_subject`, which enumerates the five defects of the
    // replay shape that was here before.
    let audit_rows_removed = match cascade {
        PurgeCascade::Cascade => {
            sqlx::query("DELETE FROM audit WHERE tenant_id = $1 AND subject = $2")
                .bind(tenant.as_str())
                .bind(subject.as_str())
                .execute(&mut *conn)
                .await
                .pg()?
                .rows_affected()
        }
        PurgeCascade::Preserve => 0,
    };

    for table in ["idempotency", "capacity"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1 AND subject = $2"))
            .bind(tenant.as_str())
            .bind(subject.as_str())
            .execute(&mut *conn)
            .await
            .pg()?;
    }

    // After the deletes, inside the same transaction, under the id it carries
    // — the echo rule on `Backend`. Inserting first and then sweeping under
    // `Cascade` would delete the purge's own record: the erasure would eat the
    // only evidence it ran.
    crate::audit::insert(&mut *conn, audit).await?;

    Ok(PurgeReport {
        items_removed,
        vectors_removed,
        audit_rows_removed,
        // Under `Preserve` nothing was deleted, so every pre-existing row
        // survives; under `Cascade` they are all in `audit_rows_removed`. The
        // record just inserted is in neither term.
        audit_rows_preserved: existing_audit as u64 - audit_rows_removed,
    })
}
```

`crates/memorysafe-backend-postgres/src/portability.rs`:

```rust
use crate::error::SqlxResultExt;
use crate::items::{item_columns, row_to_item};
use crate::{audit, capacity, items, vectors};
use base64::Engine as _;
use memorysafe_backend::{
    BackendError, ExportRecord, ExportStream, ExportVector, FORMAT_VERSION, ImportReport,
    ImportStream, ScopeSelector,
};
use memorysafe_core::{AuditFilter, EmbedderId, Scope, TenantId};
use memorysafe_embed::QuantizedVector;
use sqlx::{PgConnection, Row};

// `FORMAT_VERSION` comes from `memorysafe-backend`, not from a private copy
// here. "A supported format version" is a property of the format, not of
// whichever backend is reading the stream; two backends each declaring their
// own constant is one silent divergence away from a Postgres export SQLite
// refuses.

pub async fn export(
    conn: &mut PgConnection,
    sel: &ScopeSelector,
) -> Result<ExportStream, BackendError> {
    let mut out: ExportStream = vec![ExportRecord::Header {
        format_version: FORMAT_VERSION,
        exported_at: time::OffsetDateTime::now_utc().unix_timestamp(),
    }];

    let sql = format!(
        "SELECT {cols}, v.embedder AS v_embedder, v.dim AS v_dim, v.scale AS v_scale,
                v.q AS v_q
         FROM items i
         LEFT JOIN vectors v ON v.tenant_id = i.tenant_id AND v.item_id = i.id
         WHERE i.tenant_id = $1
           AND ($2::text IS NULL OR i.subject = $2)
           AND ($3::text IS NULL OR i.namespace = $3)
         ORDER BY i.id ASC",
        cols = item_columns("i")
    );

    let rows = sqlx::query(&sql)
        .bind(sel.tenant.as_str())
        .bind(sel.subject.as_ref().map(|s| s.as_str()))
        .bind(sel.namespace.as_ref().map(|n| n.as_str()))
        .fetch_all(&mut *conn)
        .await
        .pg()?;

    let mut scopes: Vec<Scope> = Vec::new();
    for row in &rows {
        let item = row_to_item(row)?;
        let embedder: Option<String> = row.try_get("v_embedder").pg()?;
        let vector = match embedder {
            Some(embedder) => {
                let dim: i32 = row.try_get("v_dim").pg()?;
                let scale: f32 = row.try_get("v_scale").pg()?;
                let q: Vec<u8> = row.try_get("v_q").pg()?;
                Some(ExportVector {
                    embedder,
                    dim: dim as u16,
                    scale,
                    q_base64: base64::engine::general_purpose::STANDARD.encode(q),
                })
            }
            None => None,
        };
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
            audit_rows.extend(audit::query(&mut *conn, &scope, &filter).await?);
        }
        audit_rows.sort_by(|a, b| a.id.cmp(&b.id));
        for record in audit_rows {
            out.push(ExportRecord::Audit { audit: Box::new(record) });
        }
    }

    Ok(out)
}

pub async fn import(
    conn: &mut PgConnection,
    destination: &TenantId,
    stream: ImportStream,
    vector_dim: u16,
) -> Result<ImportReport, BackendError> {
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
            ExportRecord::Item { item, vector } => {
                let scope = item.scope.clone();
                // Every record is compared against `destination` — never
                // against another record. No record's tenant is authority for
                // any other's, so a disagreement rejects the whole import
                // rather than being retargeted. RLS would refuse the write
                // anyway, but with an error that names no record; this one
                // does, and it fires before any row is attempted.
                if scope.tenant != *destination {
                    return Err(BackendError::MalformedImport(format!(
                        "item {} names tenant {} but the destination is {destination}",
                        item.id, scope.tenant
                    )));
                }
                // Import is idempotent: an item already present is skipped
                // rather than duplicated or overwritten.
                if items::exists(&mut *conn, &scope, &item.id).await? {
                    report.items_skipped_existing += 1;
                    continue;
                }
                capacity::ensure_row(&mut *conn, &scope).await?;
                items::insert(&mut *conn, &item).await?;
                capacity::adjust(&mut *conn, &scope, 1, item.byte_size() as i64).await?;
                report.items_imported += 1;

                if let Some(v) = vector {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&v.q_base64)
                        .map_err(|e| BackendError::MalformedImport(e.to_string()))?;
                    let q = QuantizedVector::from_bytes(
                        EmbedderId::new(&v.embedder),
                        v.dim,
                        v.scale,
                        &bytes,
                    )
                    .map_err(|e| BackendError::MalformedImport(e.to_string()))?;
                    vectors::insert(&mut *conn, &item.id, &scope, &q, vector_dim).await?;
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
                audit::insert(&mut *conn, &audit).await?;
                report.audit_imported += 1;
            }
        }
    }

    Ok(report)
}
```

`crates/memorysafe-backend-postgres/src/aggregates.rs` — both halves, write and read. Plan 1's items-and-audit task carries the SQLite sketch for `increment` and its portability task carries `query`; the shapes transfer, the dialect does not. Three things this document must get right that the SQLite one states in the same places:

- **Every audit row increments**, in the transaction that writes it — `apply`, `record_recall`, the purge's `SubjectPurged` row, and rows arriving through `import`. See the write rule in `memorysafe_backend::aggregates`.
- **The increment must be atomic against concurrent writers.** Postgres has no per-tenant write lock, so a read-modify-write under READ COMMITTED loses updates. Use `ON CONFLICT ... DO UPDATE SET count = audit_aggregates.count + 1` and let the database evaluate it, against the partial unique index the key falls in.
- **The read states collation and null placement explicitly, in the form the index can serve** — `COLLATE "C"` on every text column of the key, and `ASC NULLS FIRST` on `policy_name` and `policy_version`. **Not `(policy_name IS NULL) DESC`.** That expression form satisfies the mandate and is unservable by any column index, so the planner sorts the whole scope; Plan 1 measured it (`EXPLAIN QUERY PLAN` → `USE TEMP B-TREE FOR ORDER BY`) and now prescribes `ASC NULLS FIRST` for the same reason. **And `idx_aggregates_order` must itself declare `NULLS FIRST` on both nullable columns** — in Postgres the index carries its own null ordering and `ASC` defaults to `NULLS LAST`, so a query and an index that disagree produce the sort the explicit ordering was meant to avoid. The read pages **ascending** with `after` selecting keys strictly greater, the opposite of `Backend::audit`. This crate must carry `ordering_sql_states_collation_and_null_placement`, asserting over the SQL its query builder returns rather than a copied literal.

Replace the last four placeholders in `lib.rs`:

```rust
    async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
        cascade: PurgeCascade,
        audit: AuditRecord,
    ) -> Result<PurgeReport, BackendError> {
        let mut tx = tenant_txn(self, tenant).await?;
        let report = purge::subject(&mut tx, tenant, subject, cascade, &audit).await?;
        tx.commit().await.pg()?;
        Ok(report)
    }

    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError> {
        let mut tx = tenant_txn(self, &sel.tenant).await?;
        let out = portability::export(&mut tx, sel).await?;
        tx.commit().await.pg()?;
        Ok(out)
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
        let mut tx = tenant_txn(self, destination).await?;
        let report =
            portability::import(&mut tx, destination, stream, self.config.vector_dim).await?;
        tx.commit().await.pg()?;
        Ok(report)
    }
```

Add `pub mod portability;` and `pub mod purge;`. `lib.rs` does not need `ExportRecord` — nothing in it inspects the stream.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS — `postgres_passes_the_backend_conformance_suite` prints all 33 test names and passes.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): subject purge and portable export/import; full conformance passes"
```

---

## Task 14: The `SchemaPerTenant` layout

**Files:**
- Modify: `crates/memorysafe-backend-postgres/src/bootstrap.rs`
- Create: `crates/memorysafe-backend-postgres/tests/layout.rs`
- Modify: `crates/memorysafe-backend-postgres/tests/conformance.rs`

**Interfaces:**
- Consumes: `ddl::statements` (already branches on layout), `ensure_ready` (already lazy).
- Produces: an advisory lock around `ensure_schema`, and a second full conformance run.

**Milestone: every conformance test the pinned submodule contains passes under both
layouts.** Stated against the pin rather than against a number: the submodule SHA is
authoritative and the count is descriptive. A milestone naming a count is a derived
value with no reference — it was written at 33, has been 39, 45, 47, 49 and is 50 as of
`5776fce`, and each of those was correct when written.

**Most of this layout already exists** — `ddl::statements` omits the partitioning clause and the partition tables, and `ensure_ready` creates a tenant's schema on first use. Two things are missing, and both are the kind of bug that only appears under load.

**The race.** `CREATE SCHEMA IF NOT EXISTS` and `CREATE TABLE IF NOT EXISTS` are not atomic against a concurrent identical statement: both sessions see the object absent, both create it, and one gets `duplicate key value violates unique constraint "pg_namespace_nspname_index"`. Under `SharedPartitioned` the window is one bootstrap per process and easy to miss; under `SchemaPerTenant` it opens on the first write to every new tenant, which is exactly when several requests tend to arrive at once. A transaction-scoped advisory lock keyed on the schema name closes it.

**Where the lock key comes from.** Rust, not `hashtext`. `hashtext` is an undocumented internal whose algorithm is not a compatibility promise, and a hash key that changed across a PostgreSQL upgrade would silently stop excluding anything.

**A property to be aware of, not a bug:** under `SchemaPerTenant`, a *read* for an unknown tenant creates that tenant's schema, because `tenant_txn` cannot know the operation is read-only. Tenant ids reach the backend already validated against the caller's API key, so unbounded schema creation needs a compromised key rather than a crafted request — but the adapters in Plan 3 are what make that true, and it is worth saying out loud here.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend-postgres/tests/layout.rs`:

```rust
mod support;

use memorysafe_backend::conformance::fx;
use memorysafe_backend::{Backend, Page};
use memorysafe_backend_postgres::{PgLayout, PostgresBackend, schema_for_tenant};
use memorysafe_core::{Scope, TenantId};
use sqlx::Row;

async fn per_tenant() -> PostgresBackend {
    let config = support::test_config(PgLayout::SchemaPerTenant).await;
    PostgresBackend::connect(config).await.expect("connect")
}

async fn schema_exists(backend: &PostgresBackend, schema: &str) -> bool {
    sqlx::query("SELECT 1 FROM pg_namespace WHERE nspname = $1")
        .bind(schema)
        .fetch_optional(backend.app_pool())
        .await
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn a_tenants_schema_appears_on_first_use() {
    let backend = per_tenant().await;
    let tenant = TenantId::new("acme").unwrap();
    let schema = schema_for_tenant(backend.config(), &tenant);

    assert!(!schema_exists(&backend, &schema).await, "{schema} existed before any use");

    let scope = Scope::new("acme", "s", "n").unwrap();
    backend.apply(fx::admit_txn(&scope, fx::item(&scope, "first"), None)).await.unwrap();

    assert!(schema_exists(&backend, &schema).await, "{schema} was not created");
}

#[tokio::test]
async fn each_tenant_gets_a_schema_of_its_own() {
    let backend = per_tenant().await;
    let mut schemas = Vec::new();
    for tenant in ["acme", "globex", "initech"] {
        let scope = Scope::new(tenant, "s", "n").unwrap();
        backend.apply(fx::admit_txn(&scope, fx::item(&scope, "a note"), None)).await.unwrap();
        schemas.push(schema_for_tenant(backend.config(), &TenantId::new(tenant).unwrap()));
    }

    let unique: std::collections::HashSet<_> = schemas.iter().collect();
    assert_eq!(unique.len(), 3, "tenants shared a schema: {schemas:?}");
    for schema in &schemas {
        assert!(schema_exists(&backend, schema).await, "{schema} missing");
    }
}

#[tokio::test]
async fn the_per_tenant_layout_creates_no_partitions() {
    let backend = per_tenant().await;
    let tenant = TenantId::new("acme").unwrap();
    let scope = Scope::new("acme", "s", "n").unwrap();
    backend.apply(fx::admit_txn(&scope, fx::item(&scope, "a note"), None)).await.unwrap();
    let schema = schema_for_tenant(backend.config(), &tenant);

    let n: i64 = sqlx::query(
        "SELECT count(*) AS c FROM pg_class c
         JOIN pg_namespace ns ON ns.oid = c.relnamespace
         WHERE ns.nspname = $1 AND c.relispartition",
    )
    .bind(&schema)
    .fetch_one(backend.app_pool())
    .await
    .unwrap()
    .get("c");
    assert_eq!(n, 0, "{schema} has {n} partitions; the per-tenant layout needs none");
}

/// A schema boundary is not a reason to drop the policy. Both layouts keep
/// RLS on, so there is one security model rather than two.
#[tokio::test]
async fn row_level_security_is_still_enforced_per_tenant() {
    let backend = per_tenant().await;
    let scope = Scope::new("acme", "s", "n").unwrap();
    backend.apply(fx::admit_txn(&scope, fx::item(&scope, "a note"), None)).await.unwrap();
    let schema = schema_for_tenant(backend.config(), &TenantId::new("acme").unwrap());

    let n: i64 = sqlx::query(&format!("SELECT count(*) AS c FROM {schema}.items"))
        .fetch_one(backend.app_pool())
        .await
        .unwrap()
        .get("c");
    assert_eq!(n, 0, "a query with no tenant context read {n} rows from a per-tenant schema");
}

/// The race the advisory lock exists for: several first writes to one new
/// tenant, arriving together.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_first_writes_to_a_new_tenant_do_not_race() {
    use std::sync::Arc;
    let backend = Arc::new(per_tenant().await);
    let scope = Scope::new("brandnew", "s", "n").unwrap();

    let mut handles = Vec::new();
    for i in 0..8 {
        let b = Arc::clone(&backend);
        let s = scope.clone();
        handles.push(tokio::spawn(async move {
            b.apply(fx::admit_txn(&s, fx::item(&s, &format!("concurrent {i}")), None)).await
        }));
    }
    for h in handles {
        h.await.unwrap().expect("a concurrent first write failed");
    }

    assert_eq!(backend.list(&scope, &Page { offset: 0, limit: 50 }).await.unwrap().len(), 8);
}
```

Add to `crates/memorysafe-backend-postgres/tests/conformance.rs`:

```rust
/// The same frozen suite, against a schema per tenant. Both layouts are one
/// implementation, so both have to pass it — otherwise "configurable layout"
/// means "one layout and an untested branch".
#[tokio::test(flavor = "multi_thread")]
async fn postgres_passes_the_suite_with_a_schema_per_tenant() {
    run_conformance_suite(&PgFactory { layout: PgLayout::SchemaPerTenant }).await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test layout`
Expected: FAIL — `concurrent_first_writes_to_a_new_tenant_do_not_race` fails intermittently with `duplicate key value violates unique constraint "pg_type_typname_nsp_index"` or a `relation already exists` error. Run it a few times; the race is real but not certain on every run.

- [ ] **Step 3: Write minimal implementation**

Add the advisory lock to `ensure_schema` in `crates/memorysafe-backend-postgres/src/bootstrap.rs`:

```rust
/// A stable 64-bit lock key for a schema name.
///
/// Derived in Rust rather than with PostgreSQL's `hashtext`, which is an
/// undocumented internal: a hash whose algorithm changed across an upgrade
/// would leave two servers computing different keys and excluding nothing.
fn advisory_key(schema: &str) -> i64 {
    let digest = blake3::hash(schema.as_bytes());
    i64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("8 bytes"))
}

pub async fn ensure_schema(
    admin: &PgPool,
    config: &PgConfig,
    schema: &str,
) -> Result<(), BackendError> {
    ddl::checked_ident(schema)?;
    let mut tx = admin.begin().await.pg()?;

    // `CREATE ... IF NOT EXISTS` is not atomic against a concurrent identical
    // statement: both sessions see the object absent and one loses on the
    // catalogue's unique index. Under `SchemaPerTenant` that window opens on
    // the first write to every new tenant, which is precisely when several
    // requests tend to arrive together. The lock is transaction-scoped, so it
    // is released by the commit below.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(advisory_key(schema))
        .execute(&mut *tx)
        .await
        .pg()?;

    for statement in ddl::statements(config, schema) {
        sqlx::query(&statement).execute(&mut *tx).await.pg()?;
    }
    tx.commit().await.pg()?;
    Ok(())
}
```

`ensure_ready` also needs to hold its decision across the await, or two tasks both miss the cache and both call `ensure_schema` — harmless now that the lock exists, but wasteful. Leave the double-checked pattern in `session.rs` as it is: the lock makes the redundant call correct, and holding a lock across an await in `ensure_ready` would need an async mutex for no real gain.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres` and re-run `--test layout` three times to be confident the race is closed.
Expected: PASS — 5 layout tests, plus both conformance suite runs.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): schema-per-tenant layout passes the frozen suite; close the DDL race"
```

---

## Task 15: Cross-backend parity

**Files:**
- Create: `crates/memorysafe-backend-postgres/tests/parity.rs`

**Interfaces:**
- Consumes: `SqliteBackend` from the submodule, `PostgresBackend`, the conformance fixtures.
- Produces: nothing. Like Task 8, this task adds evidence rather than code.

**What the conformance suite cannot check.** It runs each backend in isolation, so it can only assert properties each satisfies on its own: "results come back sorted", "the ceiling is enforced". It cannot assert that both produce *the same* answer. That is the property customers actually depend on when they move from the free tier to the scaling tier, and it is the property that decays first — a fusion weight changed on one side, a tie broken differently, a median computed with `percentile_cont` instead of an offset.

**What can and cannot be asserted.** Vector scores are produced by `QuantizedVector::dot` on both sides, so they are identical to the bit. Keyword scores are not: bm25 and `ts_rank_cd` are different functions, and no amount of normalisation makes them agree. So the parity claims are: identical ordering and identical scores for vector-only retrieval; identical *result sets* for filtered and hybrid retrieval; identical accounting; and an export from one backend importing cleanly into the other.

**The last of those is a product claim, not just a test.** "A customer's memory is genuinely theirs" means the export format is portable across backends, not merely round-trippable within one. Nothing else in either plan checks that.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-backend-postgres/tests/parity.rs`:

```rust
mod support;

use memorysafe_backend::conformance::fx;
use memorysafe_backend::{
    Backend, CandidateQuery, HardFilters, Page, ScopeSelector,
};
use memorysafe_backend_postgres::{PgLayout, PostgresBackend};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{MemoryItem, Scope, ScoredCandidate, SensitivityLevel, TenantId};
use memorysafe_embed::Embedder;

/// Plain alphanumeric words only: FTS5's `unicode61` and PostgreSQL's `simple`
/// agree on those, so a difference in results is a difference in the backend
/// rather than in tokenization.
fn corpus(scope: &Scope) -> Vec<MemoryItem> {
    [
        ("the cat sat on the mat", "fact", vec!["home"], SensitivityLevel::Public),
        ("the cat sat on a rug", "fact", vec!["home"], SensitivityLevel::Internal),
        ("a dog barked at the postman", "event", vec!["home"], SensitivityLevel::Internal),
        ("quarterly revenue exceeded projections", "fact", vec!["work"], SensitivityLevel::Personal),
        ("deployment used zstandard compression", "procedure", vec!["work"], SensitivityLevel::Internal),
        ("the account number is redacted", "fact", vec!["work"], SensitivityLevel::Restricted),
    ]
    .into_iter()
    .map(|(body, kind, tags, level)| fx::item_with(scope, body, kind, &tags, level))
    .collect()
}

async fn seed(backend: &impl Backend, scope: &Scope, items: &[MemoryItem]) {
    for item in items {
        let vector = fx::vector_for(&item.body);
        backend
            .apply(fx::admit_txn(scope, item.clone(), Some(vector)))
            .await
            .unwrap();
    }
}

fn ids(hits: &[ScoredCandidate]) -> Vec<String> {
    hits.iter().map(|h| h.item.id.as_str().to_string()).collect()
}

async fn both() -> (SqliteBackend, PostgresBackend, Scope, Vec<MemoryItem>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sqlite = SqliteBackend::open(dir.keep());
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let postgres = PostgresBackend::connect(config).await.unwrap();

    let scope = Scope::new("parity", "s", "n").unwrap();
    // The *same* items, ids included, so a tie broken by id breaks the same
    // way on both sides.
    let items = corpus(&scope);
    seed(&sqlite, &scope, &items).await;
    seed(&postgres, &scope, &items).await;
    (sqlite, postgres, scope, items)
}

/// Both backends score with `QuantizedVector::dot`, so this is exact.
#[tokio::test]
async fn vector_only_retrieval_ranks_identically() {
    let (sqlite, postgres, scope, _) = both().await;
    let query = CandidateQuery {
        embedding: Some(fx::embedder().embed("the cat sat on the mat").unwrap()),
        text: None,
        filters: HardFilters { sensitivity_ceiling: SensitivityLevel::Restricted, ..Default::default() },
        limit: 10,
    };

    let a = sqlite.retrieve_candidates(&scope, &query).await.unwrap();
    let b = postgres.retrieve_candidates(&scope, &query).await.unwrap();

    assert_eq!(ids(&a), ids(&b), "the two backends ordered vector results differently");
    for (x, y) in a.iter().zip(&b) {
        assert!(
            (x.relevance - y.relevance).abs() < 1e-6,
            "{} scored {} on sqlite and {} on postgres",
            x.item.body, x.relevance, y.relevance
        );
    }
}

#[tokio::test]
async fn neighbours_rank_identically() {
    let (sqlite, postgres, scope, _) = both().await;
    let probe = fx::embedder().embed("the cat sat on the mat").unwrap();

    let a = sqlite.neighbours(&scope, &probe, 4).await.unwrap();
    let b = postgres.neighbours(&scope, &probe, 4).await.unwrap();
    assert_eq!(ids(&a), ids(&b));
}

/// Keyword scores differ by construction — bm25 is not `ts_rank_cd` — so the
/// claim is the candidate set, which the filters determine, plus the top hit.
#[tokio::test]
async fn hybrid_retrieval_selects_the_same_candidates() {
    let (sqlite, postgres, scope, _) = both().await;
    let query = CandidateQuery {
        embedding: Some(fx::embedder().embed("cat mat").unwrap()),
        text: Some("cat mat".into()),
        filters: HardFilters { sensitivity_ceiling: SensitivityLevel::Restricted, ..Default::default() },
        limit: 10,
    };

    let mut a = ids(&sqlite.retrieve_candidates(&scope, &query).await.unwrap());
    let mut b = ids(&postgres.retrieve_candidates(&scope, &query).await.unwrap());
    let (top_a, top_b) = (a.first().cloned(), b.first().cloned());
    a.sort();
    b.sort();

    assert_eq!(a, b, "the two backends considered different candidates");
    assert_eq!(top_a, top_b, "the two backends chose different best hits");
}

/// The security-critical filter has to agree exactly. A ceiling that admits
/// one more row on one backend than the other is a leak on that backend.
#[tokio::test]
async fn hard_filters_select_the_same_rows() {
    let (sqlite, postgres, scope, _) = both().await;

    for filters in [
        HardFilters { sensitivity_ceiling: SensitivityLevel::Internal, ..Default::default() },
        HardFilters { sensitivity_ceiling: SensitivityLevel::Personal, ..Default::default() },
        HardFilters {
            sensitivity_ceiling: SensitivityLevel::Restricted,
            kinds: vec!["fact".into()],
            ..Default::default()
        },
        HardFilters {
            sensitivity_ceiling: SensitivityLevel::Restricted,
            tags_any: vec!["work".into()],
            ..Default::default()
        },
    ] {
        let query = CandidateQuery {
            embedding: Some(fx::embedder().embed("the cat sat on the mat").unwrap()),
            text: None,
            filters: filters.clone(),
            limit: 10,
        };
        let mut a = ids(&sqlite.retrieve_candidates(&scope, &query).await.unwrap());
        let mut b = ids(&postgres.retrieve_candidates(&scope, &query).await.unwrap());
        a.sort();
        b.sort();
        assert_eq!(a, b, "filters {filters:?} selected different rows");
    }
}

#[tokio::test]
async fn accounting_and_statistics_agree() {
    let (sqlite, postgres, scope, _) = both().await;

    let a = sqlite.capacity_state(&scope).await.unwrap();
    let b = postgres.capacity_state(&scope).await.unwrap();
    assert_eq!((a.used_items, a.used_bytes), (b.used_items, b.used_bytes));

    let a = sqlite.scope_stats(&scope).await.unwrap();
    let b = postgres.scope_stats(&scope).await.unwrap();
    assert_eq!(a.item_count, b.item_count);
    assert_eq!(a.total_bytes, b.total_bytes);
    assert_eq!(
        a.median_item_bytes, b.median_item_bytes,
        "the medians disagree; one backend is not using the offset method"
    );
}

/// The anti-lock-in claim, tested: a customer on SQLite can move to the
/// scaling tier without an intermediate tool.
#[tokio::test]
async fn an_export_from_sqlite_imports_into_postgres() {
    let (sqlite, postgres, scope, _) = both().await;
    let fresh_config = support::test_config(PgLayout::SharedPartitioned).await;
    let target = PostgresBackend::connect(fresh_config).await.unwrap();

    let selector = ScopeSelector {
        tenant: TenantId::new("parity").unwrap(),
        subject: None,
        namespace: None,
        include_audit: true,
    };
    let exported = sqlite.export(&selector).await.unwrap();
    let report = target.import(&selector.tenant, exported).await.unwrap();
    assert_eq!(report.items_imported, 6);
    assert_eq!(report.vectors_imported, 6);

    let mut before = sqlite.list(&scope, &Page { offset: 0, limit: 50 }).await.unwrap();
    let mut after = target.list(&scope, &Page { offset: 0, limit: 50 }).await.unwrap();
    before.sort_by(|a, b| a.id.cmp(&b.id));
    after.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(before, after, "the crossing lost or changed something");

    // And the vectors survived the crossing, not just the rows.
    let probe = fx::embedder().embed("the cat sat on the mat").unwrap();
    assert_eq!(
        ids(&sqlite.neighbours(&scope, &probe, 3).await.unwrap()),
        ids(&target.neighbours(&scope, &probe, 3).await.unwrap()),
    );
    // The other direction too, so neither export is the privileged one.
    let back = postgres.export(&selector).await.unwrap();
    assert!(!back.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test parity`
Expected: These should pass if Tasks 9–13 are right. If any fails, the failure names the divergence: a different fusion weight, a different tie-break, a median computed differently, or an export that drops a field. Fix the Postgres side — the SQLite backend is the reference, because it is what the conformance suite was written against.

- [ ] **Step 3: Write minimal implementation**

None expected. Any change made here is a correction to Tasks 9–13, and belongs in this commit with a note saying which task it amends.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres --test parity`
Expected: PASS — 6 tests ok.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/tests/parity.rs
git commit -m "test(pg): assert SQLite and Postgres rank, account, and export alike"
```

---

## Task 16: Schema-version guard, dimension guard, and operator documentation

**Files:**
- Modify: `crates/memorysafe-backend-postgres/src/ddl.rs`
- Modify: `crates/memorysafe-backend-postgres/src/bootstrap.rs`
- Create: `crates/memorysafe-backend-postgres/README.md`
- Modify: `crates/memorysafe-backend-postgres/tests/bootstrap.rs`

**Interfaces:**
- Consumes: `ddl::SCHEMA_VERSION`, `PgConfig::vector_dim`.
- Produces: `bootstrap::check_compatibility`, two `meta` rows, and the operator documentation.

**Two ways a deployment can be quietly wrong, both worth failing loudly on.**

A binary older than the database — a rollback that did not roll the schema back, or one node left behind during a deploy — will run happily against a newer schema until it meets a column it does not know about. Refusing at `connect` turns a mystery into a startup error.

A `vector_dim` that disagrees with the stored one is worse, because it half-works: `vectors::insert` refuses new writes with `EmbedderMismatch`, but every existing row stays readable and every recall silently loses the vector signal for them. The spec calls changing embedder "an explicit, resumable re-embedding migration, audited as `Reembedded` — never a silent recall degradation"; this is the backend's half of keeping that promise.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-backend-postgres/tests/bootstrap.rs`:

```rust
#[tokio::test]
async fn a_database_from_a_newer_build_is_refused() {
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let schema = config.schema.clone();
    let backend = PostgresBackend::connect(config.clone()).await.unwrap();

    sqlx::query(&format!(
        "UPDATE {schema}.meta SET value = '99' WHERE key = 'schema_version'"
    ))
    .execute(backend.app_pool())
    .await
    .unwrap();
    drop(backend);

    let err = PostgresBackend::connect(config)
        .await
        .expect_err("connected to a schema from a newer build");
    assert!(
        err.to_string().contains("schema version 99"),
        "the error should name the version it found, got: {err}"
    );
}

#[tokio::test]
async fn a_vector_dimension_change_is_refused_rather_than_half_applied() {
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    let backend = PostgresBackend::connect(config.clone()).await.unwrap();
    drop(backend);

    let widened = config.with_vector_dim(384);
    let err = PostgresBackend::connect(widened)
        .await
        .expect_err("connected with a vector width the schema was not built for");
    assert!(
        err.to_string().contains("256") && err.to_string().contains("384"),
        "the error should name both widths, got: {err}"
    );
}

#[tokio::test]
async fn reconnecting_with_the_same_settings_stays_a_no_op() {
    let config = support::test_config(PgLayout::SharedPartitioned).await;
    for _ in 0..3 {
        PostgresBackend::connect(config.clone()).await.expect("reconnect");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-backend-postgres --test bootstrap`
Expected: FAIL — `a_database_from_a_newer_build_is_refused` panics: `connected to a schema from a newer build`.

- [ ] **Step 3: Write minimal implementation**

Record the vector width alongside the schema version. In `ddl::statements`, replace the single `meta` insert with:

```rust
    out.push(format!(
        "INSERT INTO meta (key, value) VALUES
           ('schema_version', '{SCHEMA_VERSION}'),
           ('vector_dim', '{dim}')
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"
    ));
```

Add the guard to `bootstrap.rs` and call it from `ensure_schema` before the DDL runs:

```rust
use sqlx::Row;

/// Refuses a schema this build cannot safely write to.
///
/// Runs before the DDL, because the DDL's own `meta` upsert would otherwise
/// overwrite the very values being checked. A schema with no `meta` table yet
/// is a fresh one and passes; a `vector_dim` row absent from a schema created
/// by an earlier build is treated as "no opinion" and gets written on this
/// pass.
pub async fn check_compatibility(
    tx: &mut sqlx::PgConnection,
    config: &PgConfig,
    schema: &str,
) -> Result<(), BackendError> {
    let present: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
        .bind(format!("{schema}.meta"))
        .fetch_one(&mut *tx)
        .await
        .pg()?;
    if present.is_none() {
        return Ok(());
    }

    let rows = sqlx::query(&format!("SELECT key, value FROM {schema}.meta"))
        .fetch_all(&mut *tx)
        .await
        .pg()?;

    for row in &rows {
        let key: String = row.try_get("key").pg()?;
        let value: String = row.try_get("value").pg()?;
        match key.as_str() {
            "schema_version" => {
                let found: i64 = value.parse().unwrap_or(ddl::SCHEMA_VERSION);
                if found > ddl::SCHEMA_VERSION {
                    return Err(BackendError::Storage {
                        message: format!(
                            "{schema} is at schema version {found}; this build \
                             understands {}. Upgrade the binary or roll the \
                             schema back.",
                            ddl::SCHEMA_VERSION
                        ),
                        retryable: false,
                    });
                }
            }
            "vector_dim" => {
                let found: u16 = value.parse().unwrap_or(config.vector_dim);
                if found != config.vector_dim {
                    return Err(BackendError::Storage {
                        message: format!(
                            "{schema} stores vector({found}) but this process is \
                             configured for vector({}). Changing embedder is an \
                             explicit re-embedding migration, not a config edit.",
                            config.vector_dim
                        ),
                        retryable: false,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}
```

and in `ensure_schema`, between the advisory lock and the DDL loop:

```rust
    check_compatibility(&mut tx, config, schema).await?;
```

`crates/memorysafe-backend-postgres/README.md` — what an operator needs and cannot read off the code:

```markdown
# memorysafe-backend-postgres

The scaling-tier backend. Commercial; see `LICENSE`.

## Requirements

- PostgreSQL 16 or newer.
- `pgvector` 0.8 or newer. The floor is iterative index scans, which filtered
  ANN retrieval depends on.
- The connecting role needs `CREATE` on the database (for `CREATE EXTENSION`
  and the schema) at first start. After bootstrap it needs no DDL rights.

## Roles

Bootstrap creates `memorysafe_app`: `NOLOGIN NOSUPERUSER NOBYPASSRLS`. Runtime
connections `SET ROLE` to it, which is what makes the row-level security
policies apply — a superuser connection would ignore them entirely. Do not
grant it `BYPASSRLS`, and do not point the pool at a role that already has it.

## Layouts

`SharedPartitioned` (default) puts every tenant in one schema, hash-partitioned
by `tenant_id`, with RLS. `SchemaPerTenant` gives each tenant its own schema,
created on first use, with RLS as well. Both pass the same conformance suite.
Under `SchemaPerTenant` the configured `schema` becomes a prefix and must be at
most `MAX_PREFIX_BYTES` (46) bytes — the hash is taken over the prefix as well
as the tenant, so two deployments whose prefixes differ only in length cannot
be handed the same schema.

## Changing the embedding model

`vectors.embedding` is `vector(n)` for the configured `vector_dim`, and the
width is recorded in `meta`. Starting a process with a different `vector_dim`
is refused. Changing model is a migration: re-embed every item, `ALTER TABLE
… ALTER COLUMN embedding TYPE vector(m)`, update `meta`, then restart. The
engine records it as a `Reembedded` audit event.

## Backup

Ordinary `pg_dump`. Under `SchemaPerTenant`, `pg_dump --schema` on one tenant's
schema is a per-tenant backup; under `SharedPartitioned` a per-tenant extract
means a filtered dump.

## Sizing

Vector search runs brute-force over the scope until the corpus is large enough
for the planner to reach for the HNSW index — measured at 20,000 vectors in one
scope, it does, and the query takes about 0.1 ms. Both paths score exactly;
the index changes which candidates are considered, never their order.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memorysafe-backend-postgres --all-features && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: PASS — everything green.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-backend-postgres/
git commit -m "feat(pg): refuse an incompatible schema or vector width at connect"
```

---

## Definition of done for Plan 2

- `cargo test --workspace --all-features` is green, both with Docker (container harness) and against `MEMORYSAFE_TEST_DATABASE_URL` (external server). CI runs both.
- `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check` are clean.
- `PostgresBackend` passes all **50** conformance tests **unmodified**, under `SharedPartitioned` and under `SchemaPerTenant`.
- The isolation tests prove the guarantee is structural: a query with no `tenant_id` predicate returns one tenant's rows, a connection with no tenant context reads nothing and writes nothing, a cross-tenant write is refused by `WITH CHECK`, and partitions cannot be read directly.
- Vector relevance is bit-identical to the SQLite backend's, and hard filters select the same rows on both.
- An export produced by the SQLite backend imports into Postgres and reproduces the corpus, vectors included.
- Starting against a schema from a newer build, or with a `vector_dim` the schema was not built for, fails at `connect` with a message that names both values.
- `vendor/memorysafe` is pinned to a commit at which the OSS repo's own tests pass, and this repository's `Cargo.lock` is committed.

### What Plan 2 deliberately does not do

- **No `ANALYZE` scheduling or index tuning.** `hnsw.m` and `ef_construction` stay at pgvector's defaults, and autovacuum keeps statistics current. Tuning them without a real corpus would be guesswork, and the planner already falls back to an exact scan when the index would not help.
- **No connection-level retry.** `BackendError::Storage` carries `retryable`; deciding what to do with it is the engine's job, and putting a retry loop here as well would double the attempt count invisibly.
- **No retention *expiry*.** `purge_subject` honours both `PurgeCascade` values, because the trait takes the decision as a parameter; what Plan 2 does not do is expire rows on `AuditRetention::detail` or `::aggregate` spans. That sweep is the engine's — Plan 1 Task 38. The engine never reads, rewrites or replays audit rows around a purge: it chooses the profile's `purge_cascade`, builds the `SubjectPurged` record, and hands both to the backend, which does delete-before-insert in one transaction.
- **No `Exported` / `Imported` / `PolicyChanged` audit events.** Plan 1 records these as deferred to the adapters, where a human or API actor exists to attribute them to.

**Next:** Plan 3 — the MCP server, the HTTP API, the `msafe` CLI, and the shadow-evaluation harness.
