# MemorySafe Hosted Deployment — Implementation Plan (Plan 4 of 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `memorysafe-cloud` — the closed repository that turns the MemorySafe engine into a hosted service: a GitHub-OAuth control plane, persistent API keys, a composition-root binary serving MCP and REST over the Postgres backend, and the k3s manifests that run it.

**Architecture:** A second Cargo workspace in its own closed repository, with the open-source repository vendored as a git submodule at `vendor/memorysafe`. Three crates: `memorysafe-backend-postgres` (Plan 2), `memorysafe-cloud-control` (OAuth, tenants, keys, dashboard), and `memorysafe-cloud-server` (the binary and the only composition root). The control plane owns a `control` schema with no RLS and no partitioning, kept strictly separate from the tenant tables Plan 2 governs. Authentication reuses `memorysafe-auth::ApiKeyStore` verbatim, held behind an `ArcSwap` and rebuilt from Postgres, because that crate's `Authenticated` type is deliberately unforgeable from outside.

**Tech Stack:** Rust 1.97.1 (edition 2024), `sqlx` 0.9 (`runtime-tokio`, `tls-rustls-ring`, `postgres`, `json`), `axum` 0.8, `axum-extra` 0.10 (`cookie-signed`), `tokio` 1.53, `arc-swap` 1.7, `reqwest` 0.12 (`rustls-tls`, `json`), `tower` 0.5, `tower-http` 0.7, `tower_governor` 0.4, `serde` 1.0, `serde_json` 1.0, `time` 0.3, `tracing` 0.1, `tracing-subscriber` 0.3, `testcontainers-modules` 0.15 (`postgres`), `wiremock` 0.6, PostgreSQL 17 + pgvector 0.8.6.

**Source spec:** `.superpowers/closed-tier/2026-09-07-hosted-deployment-design.md`

**Predecessors:**
- Plan 1 (`docs/superpowers/plans/2026-09-05-engine-and-sqlite.md`) — merged.
- Plan 2 (`.superpowers/closed-tier/2026-09-05-postgres-backend.md`) — **not implemented.** Required by Tasks 15–16 only.
- Plan 3 (`docs/superpowers/plans/2026-09-05-adapters-and-shadow.md`) — in progress through Task 4 of 18. `memorysafe-api` is required by Task 15.

> **Sequencing, and why the plan is ordered this way.** Tasks 1–14 depend only on `memorysafe-auth`, `memorysafe-core`, `sqlx`, and `axum` — all available on `master` today. They can be built **before or in parallel with** Plans 2 and 3. Only Tasks 15–16 need Plan 2's `PostgresBackend` and Plan 3's `memorysafe-api`/`memorysafe-mcp` routers. Do not block the control plane on the backend.

> **Where this file lives.** Like the design it implements and Plan 2 beside it, this document describes closed commercial work and must move to `memorysafe-cloud` when Task 1 creates that repository. It must not remain in the published open-source repository.

---

## Global Constraints

Every task's requirements implicitly include this section.

**Inherited from Plans 1–3, restated because a task's implementer sees only their own task:**

- **Rust edition 2024**, toolchain pinned to `1.97.1` via `rust-toolchain.toml`. Must match the submodule's pin exactly; a mismatch is a build failure waiting to happen.
- **No LLM and no network call in the memory write path.** The control plane calls GitHub over HTTP; the `remember`/`recall` path must not.
- **Adapters are thin.** No scoring, ranking, filtering, or governance decision may live in this repository. Every governance outcome reported came out of an `Engine` method. A threshold constant in this workspace is a rejected task.
- **A governance decision is not an error.** Rejected, merged, or empty is `200 OK`. `Err` is reserved for things that actually went wrong.
- **Audit rows never contain item bodies** — only ids, BLAKE3 content digests, and feature numbers. This holds on every surface, including logs (Task 13).
- **Tenant isolation is structural.** No code here constructs a `Scope` whose tenant differs from the authenticated tenant. `Authenticated::scope` is the only permitted constructor.
- **API key secrets are never stored and never logged.** Only the BLAKE3 hash is persisted; the secret is shown once, at creation.
- **`_admin` and the purged component are reserved** and rejected as caller-supplied subject or namespace. `memorysafe-auth` already enforces this in `Authenticated::scope`; do not duplicate the check.
- **Timestamps are stored as `BIGINT` Unix seconds**, matching Plan 2's convention, never `timestamptz`.
- **Ids are ULIDs** rendered as 26-character Crockford base32 strings.
- **TDD.** Every task writes a failing test first, watches it fail, then implements. Commit at the end of every task.
- **Lints:** `RUSTFLAGS="-Dwarnings"` in CI, plus `cargo clippy --all-targets --all-features -- -D warnings`. `unsafe_code = "forbid"` at the workspace level.

**New in this plan:**

- **`vendor/memorysafe` is read-only.** If a task cannot be completed without changing a vendored crate, the task is wrong or the change belongs upstream in the open repository with its own review. Never edit under `vendor/`.
- **`memorysafe-auth` is not reimplemented.** `ApiKeyStore::authenticate` is the only authentication path. `key::parse_presented` and `key::hash_presented` are `pub(crate)` and `Authenticated` has no public constructor — this is deliberate (design §6.1). Persist `ApiKeyRecord` rows and rebuild the store; do not route around the type.
- **The `control` schema has no RLS and no partitioning**, and the tenant schema is never read or written from `memorysafe-cloud-control`. The two are separate concerns sharing one database.
- **The runtime pool never holds superuser rights** for tenant data. Connections `SET ROLE` to a `NOSUPERUSER NOBYPASSRLS` role. A pool connecting as `postgres` has no isolation at all.
- **No provider-specific SDK in the binary.** Plain container plus a Postgres connection string. No Fly Machines API, no Railway env magic, no cloud SDK. Portability is architectural, not a tooling choice (design §8.1).
- **Secrets come from the environment**, never from a committed file, never from an image layer.

---

## What the vendored crates freeze

Plan 4 consumes exactly this surface. If code does not compile against these signatures, this plan is wrong, not the open repository.

```rust
// memorysafe-core
pub struct Scope { pub tenant: TenantId, pub subject: SubjectId, pub namespace: Namespace }
impl TenantId { pub fn new(s: &str) -> Result<Self, CoreError>; pub fn as_str(&self) -> &str; }
// Components: lowercase ASCII plus `-`, `_`, `.`; max 240 bytes; may not start with `.`.
pub struct Budget { pub max_items: Option<u64>, pub max_bytes: Option<u64> }
pub struct Actor { pub kind: ActorKind, pub id: Option<String> }
pub enum ActorKind { Agent, Human, ApiKey, Cli, System }

// memorysafe-auth  (exports ONLY these from `key`)
pub const KEY_PREFIX: &str;                       // "msk"
pub struct ApiKeyRecord {
    pub id: String, pub tenant: TenantId, pub hash: String,
    pub label: String, pub disabled: bool,
}
pub struct GeneratedKey { pub secret: String, pub record: ApiKeyRecord }
pub fn generate(tenant: TenantId, label: &str) -> Result<GeneratedKey, AuthError>;

pub struct ApiKeyStore { /* private HashMap<String, ApiKeyRecord> */ }
impl ApiKeyStore {
    pub fn new(records: Vec<ApiKeyRecord>) -> Self;
    pub fn records(&self) -> impl Iterator<Item = &ApiKeyRecord>;
    pub fn authenticate(&self, presented: &str) -> Result<Authenticated, AuthError>;
}
pub struct Authenticated { /* private; no public constructor */ }
impl Authenticated {
    pub fn tenant(&self) -> &TenantId;
    pub fn key_id(&self) -> &str;
    pub fn actor(&self) -> Actor;
    pub fn authorize_tenant(&self, tenant: &TenantId) -> Result<(), AuthError>;
    pub fn scope(&self, subject: &str, namespace: &str) -> Result<Scope, AuthError>;
}
pub enum AuthError { Missing, Malformed, Unknown, Disabled, WrongTenant{..},
                     Reserved{..}, Scope(CoreError), Rng }

// memorysafe-engine
pub struct EngineConfig {
    pub backend: Arc<dyn Backend>, pub embedder: Arc<dyn Embedder>,
    pub policy: Arc<dyn GovernancePolicy>, pub fallback_policy: Arc<dyn GovernancePolicy>,
    pub stance: FailureStance, pub neighbour_k: usize,
    pub eviction_candidates: usize, pub cache: CacheConfig,
    pub retention: RetentionProfile,
}
impl EngineConfig {
    pub fn new(backend: Arc<dyn Backend>, embedder: Arc<dyn Embedder>,
               policy: Arc<dyn GovernancePolicy>) -> Self;   // fills the rest with defaults
}
impl Engine {
    pub fn new(config: EngineConfig) -> Self;
    pub async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), EngineError>;
    pub async fn audit(&self, /* .. */) -> Result</* .. */, EngineError>;
}
```

**Two facts that are easy to get wrong and are load-bearing here:**

- `ApiKeyStore::authenticate` is already `O(1)` — it parses the id out of the presented key, hits a `HashMap`, then does a constant-time hash compare. Do not add a cache in front of it; it *is* the cache.
- `ApiKeyRecord.id` is the key id and is public by construction. It is what a database row is keyed on. The secret never leaves `generate`.

---

## File Structure

```
memorysafe-cloud/
  rust-toolchain.toml            # 1.97.1, matching the submodule
  Cargo.toml                     # workspace: 3 members + vendored path deps
  .gitmodules                    # vendor/memorysafe
  vendor/memorysafe/             # submodule, READ-ONLY
  crates/
    memorysafe-backend-postgres/ # Plan 2 owns this entirely
    memorysafe-cloud-control/
      src/lib.rs                 # re-exports; ControlError
      src/schema.rs              # control DDL, SCHEMA_VERSION, idempotent bootstrap
      src/tenants.rs             # TenantRecord, derivation from GitHub ids, upsert
      src/keys.rs                # ApiKeyRecord persistence: insert, disable, load_all
      src/registry.rs            # ArcSwap<ApiKeyStore>, refresh loop, LISTEN/NOTIFY
      src/github.rs              # OAuth client: authorize URL, exchange, /user, /user/orgs
      src/session.rs             # signed cookie session: Session, extractor
      src/routes/mod.rs          # Router assembly
      src/routes/auth.rs         # /login, /auth/callback, /logout
      src/routes/dashboard.rs    # /, tenant selection
      src/routes/keys.rs         # key create/revoke/list
      src/redact.rs              # tracing layer + the field denylist
    memorysafe-cloud-server/
      src/main.rs                # composition root: config -> Engine -> routers -> serve
      src/config.rs              # env parsing, one struct, no defaults for secrets
  deploy/
    base/{kustomization,deployment,statefulset,service,ingress,bootstrap-job,backup-cronjob,export-cronjob}.yaml
    overlays/production/kustomization.yaml
  Dockerfile
  docs/runbook.md
```

---

## Task Index

1. Repository skeleton, submodule, toolchain, CI
2. The `control` schema and its idempotent bootstrap
3. Tenant derivation from GitHub numeric ids
4. Tenant persistence
5. API key persistence
6. The key registry: `ArcSwap<ApiKeyStore>` and startup load
7. Registry freshness: `LISTEN`/`NOTIFY` and the backstop refresh
8. GitHub OAuth: authorize URL and CSRF state
9. GitHub OAuth: callback and token exchange
10. Signed session cookies
11. Tenant selection and provisioning with a default budget
12. Dashboard: key listing, creation, revocation
13. Log redaction and its enforcement test
14. Per-key rate limiting
15. The composition root binary
16. RLS enforcement at the composition root
17. The container image
18. k3s manifests: Deployment, StatefulSet, Ingress
19. Bootstrap Job, backup CronJob, export CronJob
20. Acceptance: smoke test, runbook, `can-1` audit

---

### Task 1: Repository skeleton, submodule, toolchain, CI

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `.github/workflows/ci.yml`
- Create: `crates/memorysafe-cloud-control/Cargo.toml`, `crates/memorysafe-cloud-control/src/lib.rs`
- Submodule: `vendor/memorysafe`

**Interfaces:**
- Produces: a workspace that compiles, with `memorysafe_auth` and `memorysafe_core` resolvable through vendored path dependencies.

- [ ] **Step 1: Create the repository and vendor the open-source one**

```bash
mkdir memorysafe-cloud && cd memorysafe-cloud && git init
git submodule add git@github.com:arsfeld/memorysafe.git vendor/memorysafe
git -C vendor/memorysafe checkout master
cp vendor/memorysafe/rust-toolchain.toml .
```

- [ ] **Step 2: Write the workspace manifest**

```toml
# Cargo.toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
rust-version = "1.97.1"
license = "UNLICENSED"
publish = false

[workspace.dependencies]
memorysafe-core   = { path = "vendor/memorysafe/crates/memorysafe-core" }
memorysafe-auth   = { path = "vendor/memorysafe/crates/memorysafe-auth" }
memorysafe-engine = { path = "vendor/memorysafe/crates/memorysafe-engine" }
memorysafe-embed  = { path = "vendor/memorysafe/crates/memorysafe-embed" }
memorysafe-policy = { path = "vendor/memorysafe/crates/memorysafe-policy" }
memorysafe-backend = { path = "vendor/memorysafe/crates/memorysafe-backend" }

sqlx = { version = "0.9", default-features = false, features = [
    "runtime-tokio", "tls-rustls-ring", "postgres", "json", "macros" ] }
axum = "0.8"
axum-extra = { version = "0.10", features = ["cookie-signed"] }
tokio = { version = "1.53", features = ["rt-multi-thread", "macros", "signal"] }
arc-swap = "1.7"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
tower = "0.5"
tower-http = { version = "0.7", features = ["trace"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
time = { version = "0.3", features = ["serde"] }
thiserror = "2.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
```

- [ ] **Step 3: Write the failing test — the vendored auth crate is reachable**

```rust
// crates/memorysafe-cloud-control/src/lib.rs
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use memorysafe_auth::{ApiKeyStore, generate};
    use memorysafe_core::TenantId;

    #[test]
    fn the_vendored_auth_crate_round_trips_a_generated_key() {
        let tenant = TenantId::new("u_1").expect("tenant id");
        let g = generate(tenant.clone(), "test").expect("generate");
        let store = ApiKeyStore::new(vec![g.record.clone()]);

        let auth = store.authenticate(&g.secret).expect("authenticate");
        assert_eq!(auth.tenant(), &tenant);
        assert_eq!(auth.key_id(), g.record.id);
    }
}
```

- [ ] **Step 4: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control`
Expected: FAIL — the crate manifest does not yet declare its dependencies.

- [ ] **Step 5: Write the crate manifest**

```toml
# crates/memorysafe-cloud-control/Cargo.toml
[package]
name = "memorysafe-cloud-control"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish = false

[dependencies]
memorysafe-core.workspace = true
memorysafe-auth.workspace = true
sqlx.workspace = true
axum.workspace = true
axum-extra.workspace = true
tokio.workspace = true
arc-swap.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true
thiserror.workspace = true
tracing.workspace = true

[lints]
workspace = true
```

- [ ] **Step 6: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control`
Expected: PASS — 1 test.

- [ ] **Step 7: Add CI**

```yaml
# .github/workflows/ci.yml
name: ci
on: [push, pull_request]
env:
  RUSTFLAGS: "-Dwarnings"
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { submodules: recursive }
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test --workspace --all-features
      - name: vendor is untouched
        run: git diff --quiet --exit-code -- vendor/ || (echo "vendor/ was modified" && exit 1)
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: workspace skeleton with the open-source repo vendored"
```

---

### Task 2: The `control` schema and its idempotent bootstrap

**Files:**
- Create: `crates/memorysafe-cloud-control/src/schema.rs`
- Test: same file, `#[cfg(test)]`

**Interfaces:**
- Produces: `pub const CONTROL_SCHEMA_VERSION: i32`, `pub async fn bootstrap(pool: &PgPool) -> Result<(), ControlError>`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing test**

```rust
// crates/memorysafe-cloud-control/src/schema.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::pg;

    #[tokio::test(flavor = "multi_thread")]
    async fn bootstrap_is_idempotent_and_records_its_version() {
        let (_c, pool) = pg().await;

        bootstrap(&pool).await.expect("first bootstrap");
        bootstrap(&pool).await.expect("second bootstrap must not fail");

        let v: i32 = sqlx::query_scalar("SELECT version FROM control.schema_version")
            .fetch_one(&pool).await.expect("version row");
        assert_eq!(v, CONTROL_SCHEMA_VERSION);

        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables
             WHERE table_schema = 'control'")
            .fetch_one(&pool).await.expect("count");
        assert_eq!(n, 3, "tenants, api_keys, schema_version");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control schema::`
Expected: FAIL — `bootstrap` not defined.

- [ ] **Step 3: Implement the schema and bootstrap**

```rust
use sqlx::PgPool;
use crate::ControlError;

pub const CONTROL_SCHEMA_VERSION: i32 = 1;

/// Every statement is `IF NOT EXISTS`, so a second run is a no-op. Ordered:
/// schema, then tables, then indexes, then the version row.
const DDL: &[&str] = &[
    "CREATE SCHEMA IF NOT EXISTS control",
    "CREATE TABLE IF NOT EXISTS control.schema_version (
        singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
        version   INTEGER NOT NULL )",
    "CREATE TABLE IF NOT EXISTS control.tenants (
        tenant_id  TEXT PRIMARY KEY,
        github_id  BIGINT NOT NULL,
        kind       TEXT NOT NULL CHECK (kind IN ('user','org')),
        login      TEXT NOT NULL,
        created_at BIGINT NOT NULL,
        UNIQUE (kind, github_id) )",
    "CREATE TABLE IF NOT EXISTS control.api_keys (
        key_id       TEXT PRIMARY KEY,
        tenant_id    TEXT NOT NULL REFERENCES control.tenants(tenant_id) ON DELETE CASCADE,
        hash         TEXT NOT NULL,
        label        TEXT NOT NULL,
        disabled     BOOLEAN NOT NULL DEFAULT FALSE,
        created_at   BIGINT NOT NULL,
        last_used_at BIGINT )",
    "CREATE INDEX IF NOT EXISTS idx_api_keys_tenant ON control.api_keys (tenant_id)",
];

pub async fn bootstrap(pool: &PgPool) -> Result<(), ControlError> {
    let mut tx = pool.begin().await?;
    for stmt in DDL {
        sqlx::query(stmt).execute(&mut *tx).await?;
    }
    sqlx::query(
        "INSERT INTO control.schema_version (singleton, version) VALUES (TRUE, $1)
         ON CONFLICT (singleton) DO UPDATE SET version = EXCLUDED.version",
    )
    .bind(CONTROL_SCHEMA_VERSION)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
```

- [ ] **Step 4: Add the test harness and the error type**

```rust
// crates/memorysafe-cloud-control/src/lib.rs
#![forbid(unsafe_code)]

pub mod keys;
pub mod schema;
pub mod tenants;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("database failure: {0}")]
    Db(#[from] sqlx::Error),
    #[error("tenant id {0} is not a valid MemorySafe tenant component")]
    InvalidTenantId(String),
    #[error("auth failure: {0}")]
    Auth(#[from] memorysafe_auth::AuthError),
}

#[cfg(test)]
pub(crate) mod test_support {
    use sqlx::PgPool;
    use testcontainers_modules::{postgres::Postgres, testcontainers::runners::AsyncRunner};
    use testcontainers_modules::testcontainers::ContainerAsync;

    /// A throwaway PostgreSQL 17 with pgvector. The container is returned so the
    /// caller holds it alive for the length of the test — dropping it stops the
    /// database mid-test, which reads as a confusing connection error.
    pub async fn pg() -> (ContainerAsync<Postgres>, PgPool) {
        let container = Postgres::default()
            .with_tag("pg17")
            .with_name("pgvector/pgvector")
            .start().await.expect("start postgres");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let pool = PgPool::connect(&url).await.expect("connect");
        (container, pool)
    }
}
```

Add to `crates/memorysafe-cloud-control/Cargo.toml`:

```toml
[dev-dependencies]
testcontainers-modules = { version = "0.15", features = ["postgres"] }
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
```

- [ ] **Step 5: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control schema::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(control): the control schema and an idempotent bootstrap"
```

---

### Task 3: Tenant derivation from GitHub numeric ids

**Files:**
- Create: `crates/memorysafe-cloud-control/src/tenants.rs`

**Interfaces:**
- Produces: `pub enum GithubAccountKind { User, Org }`, `pub fn tenant_id_for(kind: GithubAccountKind, github_id: i64) -> Result<TenantId, ControlError>`.

**Why this is its own task:** it is the one piece of logic whose failure is a cross-customer data leak, and it is pure — no database, no network. It deserves an independent gate.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tenant_id_is_derived_from_the_numeric_id_not_the_login() {
        let a = tenant_id_for(GithubAccountKind::User, 583231).expect("user tenant");
        assert_eq!(a.as_str(), "u_583231");

        let b = tenant_id_for(GithubAccountKind::Org, 9919).expect("org tenant");
        assert_eq!(b.as_str(), "o_9919");
    }

    #[test]
    fn a_user_and_an_org_sharing_a_numeric_id_are_different_tenants() {
        // GitHub's user and organization id spaces are distinct, but nothing in
        // our schema guarantees they never collide numerically. The prefix is
        // what keeps them apart, so assert it directly rather than trusting it.
        let u = tenant_id_for(GithubAccountKind::User, 42).unwrap();
        let o = tenant_id_for(GithubAccountKind::Org, 42).unwrap();
        assert_ne!(u.as_str(), o.as_str());
    }

    #[test]
    fn a_non_positive_github_id_is_refused() {
        // A zero or negative id means the caller got it from somewhere other
        // than GitHub's API. Refusing here keeps a malformed id from becoming a
        // real tenant that later collides.
        assert!(tenant_id_for(GithubAccountKind::User, 0).is_err());
        assert!(tenant_id_for(GithubAccountKind::Org, -1).is_err());
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control tenants::`
Expected: FAIL — `tenant_id_for` not defined.

- [ ] **Step 3: Implement**

```rust
use crate::ControlError;
use memorysafe_core::TenantId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubAccountKind { User, Org }

impl GithubAccountKind {
    /// The single-character tenant-id prefix. Also the `kind` column value,
    /// spelled out, so the database is readable by a human.
    fn prefix(self) -> char { match self { Self::User => 'u', Self::Org => 'o' } }
    pub fn as_str(self) -> &'static str { match self { Self::User => "user", Self::Org => "org" } }
}

/// Derives the tenant id from GitHub's **numeric** account id.
///
/// Never from the login. GitHub logins can be renamed, and a freed login can be
/// re-registered by someone else — a tenant keyed on one would silently become a
/// different tenant on rename, or inherit a recycled name's memories. The
/// numeric id is stable for the life of the account.
pub fn tenant_id_for(kind: GithubAccountKind, github_id: i64) -> Result<TenantId, ControlError> {
    if github_id <= 0 {
        return Err(ControlError::InvalidTenantId(format!("{github_id}")));
    }
    let raw = format!("{}_{github_id}", kind.prefix());
    TenantId::new(&raw).map_err(|_| ControlError::InvalidTenantId(raw))
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control tenants::`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): tenant ids derive from GitHub numeric ids, never logins"
```

---

### Task 4: Tenant persistence

**Files:**
- Modify: `crates/memorysafe-cloud-control/src/tenants.rs`

**Interfaces:**
- Consumes: `tenant_id_for` (Task 3), `bootstrap` (Task 2).
- Produces: `pub struct TenantRecord { pub tenant_id: TenantId, pub github_id: i64, pub kind: GithubAccountKind, pub login: String, pub created_at: i64 }`, `pub async fn upsert_tenant(pool: &PgPool, kind: GithubAccountKind, github_id: i64, login: &str) -> Result<TenantRecord, ControlError>`, `pub async fn list_tenants_for(pool: &PgPool, ids: &[TenantId]) -> Result<Vec<TenantRecord>, ControlError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn upsert_is_idempotent_and_refreshes_a_renamed_login() {
    let (_c, pool) = crate::test_support::pg().await;
    crate::schema::bootstrap(&pool).await.unwrap();

    let first = upsert_tenant(&pool, GithubAccountKind::User, 583231, "oldname")
        .await.expect("first upsert");
    assert_eq!(first.login, "oldname");

    // Same account, renamed. The tenant id must not move, and the cached
    // login must follow — a stale login shown in the dashboard is a support
    // ticket, a moved tenant id is a data loss.
    let second = upsert_tenant(&pool, GithubAccountKind::User, 583231, "newname")
        .await.expect("second upsert");
    assert_eq!(second.tenant_id, first.tenant_id);
    assert_eq!(second.login, "newname");

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM control.tenants")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1, "a rename must not create a second tenant");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control tenants::`
Expected: FAIL — `upsert_tenant` not defined.

- [ ] **Step 3: Implement**

```rust
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantRecord {
    pub tenant_id: TenantId,
    pub github_id: i64,
    pub kind: GithubAccountKind,
    pub login: String,
    pub created_at: i64,
}

pub async fn upsert_tenant(
    pool: &PgPool,
    kind: GithubAccountKind,
    github_id: i64,
    login: &str,
) -> Result<TenantRecord, ControlError> {
    let tenant_id = tenant_id_for(kind, github_id)?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();

    // `created_at` is deliberately not updated on conflict: it records when the
    // tenant first appeared, and a login refresh is not a new tenant.
    let row: (String, i64, String, String, i64) = sqlx::query_as(
        "INSERT INTO control.tenants (tenant_id, github_id, kind, login, created_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (tenant_id) DO UPDATE SET login = EXCLUDED.login
         RETURNING tenant_id, github_id, kind, login, created_at",
    )
    .bind(tenant_id.as_str())
    .bind(github_id)
    .bind(kind.as_str())
    .bind(login)
    .bind(now)
    .fetch_one(pool)
    .await?;

    Ok(TenantRecord {
        tenant_id,
        github_id: row.1,
        kind,
        login: row.3,
        created_at: row.4,
    })
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control tenants::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): tenant upsert keeps the id and refreshes the login"
```

---

### Task 5: API key persistence

**Files:**
- Create: `crates/memorysafe-cloud-control/src/keys.rs`

**Interfaces:**
- Consumes: `upsert_tenant` (Task 4).
- Produces: `pub async fn insert_key(pool: &PgPool, record: &ApiKeyRecord) -> Result<(), ControlError>`, `pub async fn disable_key(pool: &PgPool, tenant: &TenantId, key_id: &str) -> Result<bool, ControlError>`, `pub async fn load_all_enabled(pool: &PgPool) -> Result<Vec<ApiKeyRecord>, ControlError>`, `pub async fn list_keys_for(pool: &PgPool, tenant: &TenantId) -> Result<Vec<ApiKeyRecord>, ControlError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenants::{GithubAccountKind, upsert_tenant};
    use memorysafe_auth::{ApiKeyStore, generate};

    #[tokio::test(flavor = "multi_thread")]
    async fn a_persisted_key_authenticates_after_a_reload_and_stops_when_disabled() {
        let (_c, pool) = crate::test_support::pg().await;
        crate::schema::bootstrap(&pool).await.unwrap();
        let t = upsert_tenant(&pool, GithubAccountKind::User, 7, "dev").await.unwrap();

        let g = generate(t.tenant_id.clone(), "laptop").expect("generate");
        insert_key(&pool, &g.record).await.expect("insert");

        // The round trip that matters: rows out of Postgres rebuild a store that
        // authenticates the ORIGINAL secret. If the hash column were mangled in
        // transit this is where it shows.
        let store = ApiKeyStore::new(load_all_enabled(&pool).await.unwrap());
        let auth = store.authenticate(&g.secret).expect("authenticates");
        assert_eq!(auth.tenant(), &t.tenant_id);

        assert!(disable_key(&pool, &t.tenant_id, &g.record.id).await.unwrap());

        let store = ApiKeyStore::new(load_all_enabled(&pool).await.unwrap());
        assert!(store.authenticate(&g.secret).is_err(), "disabled key must not authenticate");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_tenant_cannot_disable_another_tenants_key() {
        let (_c, pool) = crate::test_support::pg().await;
        crate::schema::bootstrap(&pool).await.unwrap();
        let a = upsert_tenant(&pool, GithubAccountKind::User, 1, "a").await.unwrap();
        let b = upsert_tenant(&pool, GithubAccountKind::User, 2, "b").await.unwrap();

        let g = generate(a.tenant_id.clone(), "a's key").unwrap();
        insert_key(&pool, &g.record).await.unwrap();

        // Returns false rather than erroring: from b's perspective the key does
        // not exist, and saying "wrong tenant" would confirm the id is real.
        assert!(!disable_key(&pool, &b.tenant_id, &g.record.id).await.unwrap());
        let store = ApiKeyStore::new(load_all_enabled(&pool).await.unwrap());
        assert!(store.authenticate(&g.secret).is_ok(), "must still work");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control keys::`
Expected: FAIL — `insert_key` not defined.

- [ ] **Step 3: Implement**

```rust
use crate::ControlError;
use memorysafe_auth::ApiKeyRecord;
use memorysafe_core::TenantId;
use sqlx::PgPool;

pub async fn insert_key(pool: &PgPool, record: &ApiKeyRecord) -> Result<(), ControlError> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    sqlx::query(
        "INSERT INTO control.api_keys
             (key_id, tenant_id, hash, label, disabled, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&record.id)
    .bind(record.tenant.as_str())
    .bind(&record.hash)
    .bind(&record.label)
    .bind(record.disabled)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// `Ok(false)` when the key does not exist **or** belongs to another tenant.
/// The two are deliberately indistinguishable: distinguishing them would let a
/// caller probe for valid key ids across tenants.
pub async fn disable_key(
    pool: &PgPool,
    tenant: &TenantId,
    key_id: &str,
) -> Result<bool, ControlError> {
    let done = sqlx::query(
        "UPDATE control.api_keys SET disabled = TRUE
         WHERE key_id = $1 AND tenant_id = $2 AND disabled = FALSE",
    )
    .bind(key_id)
    .bind(tenant.as_str())
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

fn row_to_record(
    (key_id, tenant_id, hash, label, disabled): (String, String, String, String, bool),
) -> Result<ApiKeyRecord, ControlError> {
    Ok(ApiKeyRecord {
        id: key_id,
        tenant: TenantId::new(&tenant_id)
            .map_err(|_| ControlError::InvalidTenantId(tenant_id))?,
        hash,
        label,
        disabled,
    })
}

pub async fn load_all_enabled(pool: &PgPool) -> Result<Vec<ApiKeyRecord>, ControlError> {
    let rows: Vec<(String, String, String, String, bool)> = sqlx::query_as(
        "SELECT key_id, tenant_id, hash, label, disabled
         FROM control.api_keys WHERE disabled = FALSE",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(row_to_record).collect()
}

pub async fn list_keys_for(
    pool: &PgPool,
    tenant: &TenantId,
) -> Result<Vec<ApiKeyRecord>, ControlError> {
    let rows: Vec<(String, String, String, String, bool)> = sqlx::query_as(
        "SELECT key_id, tenant_id, hash, label, disabled
         FROM control.api_keys WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tenant.as_str())
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(row_to_record).collect()
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control keys::`
Expected: PASS — 2 tests.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): persist api key records, scoped disable"
```

---

### Task 6: The key registry — `ArcSwap<ApiKeyStore>` and startup load

**Files:**
- Create: `crates/memorysafe-cloud-control/src/registry.rs`

**Interfaces:**
- Consumes: `load_all_enabled` (Task 5).
- Produces: `pub struct KeyRegistry` with `pub async fn load(pool: PgPool) -> Result<Self, ControlError>`, `pub fn authenticate(&self, presented: &str) -> Result<Authenticated, AuthError>`, `pub async fn refresh(&self) -> Result<(), ControlError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenants::{GithubAccountKind, upsert_tenant};
    use memorysafe_auth::generate;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_created_after_load_authenticates_only_after_a_refresh() {
        let (_c, pool) = crate::test_support::pg().await;
        crate::schema::bootstrap(&pool).await.unwrap();
        let t = upsert_tenant(&pool, GithubAccountKind::User, 5, "dev").await.unwrap();

        let registry = KeyRegistry::load(pool.clone()).await.expect("load");

        let g = generate(t.tenant_id.clone(), "new").unwrap();
        crate::keys::insert_key(&pool, &g.record).await.unwrap();

        // This is the exact staleness the design accepts and bounds. Asserting
        // it rather than the happy path alone is what stops someone "fixing" it
        // by reaching past ApiKeyStore into the database on the hot path.
        assert!(registry.authenticate(&g.secret).is_err(), "stale before refresh");

        registry.refresh().await.expect("refresh");
        assert!(registry.authenticate(&g.secret).is_ok(), "fresh after refresh");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control registry::`
Expected: FAIL — `KeyRegistry` not defined.

- [ ] **Step 3: Implement**

```rust
use crate::{ControlError, keys};
use arc_swap::ArcSwap;
use memorysafe_auth::{ApiKeyStore, AuthError, Authenticated};
use sqlx::PgPool;
use std::sync::Arc;

/// Holds the whole enabled key set in memory and swaps it wholesale.
///
/// Why not read the key row from Postgres per request: `memorysafe-auth` keeps
/// `parse_presented` and `hash_presented` crate-private, and `Authenticated`
/// has no public constructor, so `ApiKeyStore::authenticate` is the only way to
/// produce one. That is a security property — a handler holding an
/// `Authenticated` provably did not skip the check — and it is worth more than
/// the staleness it costs. See design §6.1.
///
/// Memory is not a concern: an `ApiKeyRecord` is on the order of 150 bytes, so
/// 100k keys is a few megabytes.
pub struct KeyRegistry {
    pool: PgPool,
    store: ArcSwap<ApiKeyStore>,
}

impl KeyRegistry {
    pub async fn load(pool: PgPool) -> Result<Self, ControlError> {
        let records = keys::load_all_enabled(&pool).await?;
        Ok(Self { pool, store: ArcSwap::from_pointee(ApiKeyStore::new(records)) })
    }

    pub fn authenticate(&self, presented: &str) -> Result<Authenticated, AuthError> {
        self.store.load().authenticate(presented)
    }

    pub async fn refresh(&self) -> Result<(), ControlError> {
        let records = keys::load_all_enabled(&self.pool).await?;
        self.store.store(Arc::new(ApiKeyStore::new(records)));
        Ok(())
    }

    pub fn pool(&self) -> &PgPool { &self.pool }
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control registry::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): key registry over an ArcSwap of the vendored store"
```

---

### Task 7: Registry freshness — `LISTEN`/`NOTIFY` and the backstop refresh

**Files:**
- Modify: `crates/memorysafe-cloud-control/src/registry.rs`
- Modify: `crates/memorysafe-cloud-control/src/keys.rs`

**Interfaces:**
- Produces: `pub const KEY_CHANNEL: &str = "memorysafe_api_keys"`, `pub fn spawn_refresh(registry: Arc<KeyRegistry>, url: String) -> tokio::task::JoinHandle<()>`, and `notify_key_change(pool)` called from `insert_key`/`disable_key`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_notification_propagates_a_new_key_without_an_explicit_refresh() {
    let (_c, pool) = crate::test_support::pg().await;
    crate::schema::bootstrap(&pool).await.unwrap();
    let t = crate::tenants::upsert_tenant(&pool, GithubAccountKind::User, 9, "dev")
        .await.unwrap();

    let url = crate::test_support::url_of(&pool);
    let registry = Arc::new(KeyRegistry::load(pool.clone()).await.unwrap());
    let _task = spawn_refresh(registry.clone(), url);

    let g = generate(t.tenant_id.clone(), "notified").unwrap();
    crate::keys::insert_key(&pool, &g.record).await.unwrap();  // NOTIFYs

    // Poll rather than sleep-once: the listener is another task and the
    // scheduler decides when it runs. A fixed sleep makes this test flaky on a
    // loaded machine, which is how "intermittent" tests get deleted.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if registry.authenticate(&g.secret).is_ok() { return; }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("key did not propagate within 10s");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control registry::`
Expected: FAIL — `spawn_refresh` not defined.

- [ ] **Step 3: Emit the notification from both mutation paths**

In `keys.rs`, add to `insert_key` and `disable_key`, after their statement executes:

```rust
/// Fire-and-forget wake-up for every other replica's listener. A lost
/// notification is not a correctness problem — the 30s backstop in
/// `spawn_refresh` covers it — so this deliberately does not fail the write.
async fn notify_key_change(pool: &PgPool) {
    if let Err(e) = sqlx::query("SELECT pg_notify($1, '')")
        .bind(crate::registry::KEY_CHANNEL)
        .execute(pool)
        .await
    {
        tracing::warn!(error = %e, "key-change notification failed; backstop refresh will cover it");
    }
}
```

- [ ] **Step 4: Implement the listener**

```rust
pub const KEY_CHANNEL: &str = "memorysafe_api_keys";
const BACKSTOP: std::time::Duration = std::time::Duration::from_secs(30);

/// Listens for key changes and refreshes on either signal, whichever comes first.
///
/// The periodic refresh is not redundant with the listener: a dropped
/// connection loses notifications silently, and a listener that reconnects has
/// no way to learn what it missed. Thirty seconds bounds that window, and is
/// the number design §6.2 commits to.
pub fn spawn_refresh(registry: Arc<KeyRegistry>, url: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let mut listener = match sqlx::postgres::PgListener::connect(&url).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, "listener connect failed; retrying");
                    tokio::time::sleep(BACKSTOP).await;
                    continue;
                }
            };
            if let Err(e) = listener.listen(KEY_CHANNEL).await {
                tracing::warn!(error = %e, "LISTEN failed; retrying");
                tokio::time::sleep(BACKSTOP).await;
                continue;
            }
            // Refresh immediately on (re)connect: anything that changed while
            // disconnected is invisible to the notification stream.
            if let Err(e) = registry.refresh().await {
                tracing::warn!(error = %e, "refresh after connect failed");
            }
            loop {
                match tokio::time::timeout(BACKSTOP, listener.recv()).await {
                    Ok(Ok(_)) | Err(_) => {
                        if let Err(e) = registry.refresh().await {
                            tracing::warn!(error = %e, "key refresh failed");
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "listener dropped; reconnecting");
                        break;
                    }
                }
            }
        }
    })
}
```

Add `url_of` to `test_support` (store the URL alongside the pool when constructing it in Task 2's harness, and return it here).

- [ ] **Step 5: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control registry::`
Expected: PASS — 2 tests.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(control): LISTEN/NOTIFY key propagation with a 30s backstop"
```

---

### Task 8: GitHub OAuth — authorize URL and CSRF state

**Files:**
- Create: `crates/memorysafe-cloud-control/src/github.rs`

**Interfaces:**
- Produces: `pub struct GithubOauth { client_id: String, client_secret: String, redirect_uri: String, api_base: String }`, `pub fn authorize_url(&self, state: &str) -> String`, `pub fn new(..) -> Self`, `pub fn with_api_base(self, base: String) -> Self`.

**Note on `api_base`:** it exists so Tasks 9 and 20 can point the client at a `wiremock` server. It defaults to `https://api.github.com` and must never be settable from the environment in production config (Task 15).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_authorize_url_carries_the_state_and_requests_org_scope() {
        let o = GithubOauth::new("cid".into(), "secret".into(), "https://ms.example/auth/callback".into());
        let url = o.authorize_url("xyz789");

        assert!(url.starts_with("https://github.com/login/oauth/authorize?"));
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("state=xyz789"));
        // read:org is what makes /user/orgs return anything. Without it the org
        // tenant path silently returns an empty list and every user looks solo.
        assert!(url.contains("scope=read%3Aorg"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fms.example%2Fauth%2Fcallback"));
        assert!(!url.contains("secret"), "the client secret must never reach a URL");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control github::`
Expected: FAIL — `GithubOauth` not defined.

- [ ] **Step 3: Implement**

```rust
const AUTHORIZE: &str = "https://github.com/login/oauth/authorize";
const TOKEN: &str = "https://github.com/login/oauth/access_token";
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

#[derive(Debug, Clone)]
pub struct GithubOauth {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    api_base: String,
}

impl GithubOauth {
    pub fn new(client_id: String, client_secret: String, redirect_uri: String) -> Self {
        Self { client_id, client_secret, redirect_uri, api_base: DEFAULT_API_BASE.to_owned() }
    }

    /// Test seam only. Production config never sets this (Task 15).
    pub fn with_api_base(mut self, base: String) -> Self { self.api_base = base; self }

    pub fn authorize_url(&self, state: &str) -> String {
        let q = form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("scope", "read:org")
            .append_pair("state", state)
            .finish();
        format!("{AUTHORIZE}?{q}")
    }
}
```

Add `form_urlencoded = "1.2"` to the crate's dependencies.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control github::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): github authorize url with read:org and CSRF state"
```

---

### Task 9: GitHub OAuth — callback and token exchange

**Files:**
- Modify: `crates/memorysafe-cloud-control/src/github.rs`

**Interfaces:**
- Produces: `pub struct GithubIdentity { pub user_id: i64, pub login: String, pub orgs: Vec<GithubOrg> }`, `pub struct GithubOrg { pub id: i64, pub login: String }`, `pub async fn exchange(&self, code: &str) -> Result<String, ControlError>`, `pub async fn identity(&self, token: &str) -> Result<GithubIdentity, ControlError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn identity_reads_the_numeric_id_and_the_org_list() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path, header}};
    let server = MockServer::start().await;

    Mock::given(method("GET")).and(path("/user"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({ "id": 583231, "login": "arsfeld" })))
        .mount(&server).await;

    Mock::given(method("GET")).and(path("/user/orgs"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_json(serde_json::json!([{ "id": 9919, "login": "acme" }])))
        .mount(&server).await;

    let o = GithubOauth::new("cid".into(), "sec".into(), "http://x/cb".into())
        .with_api_base(server.uri());
    let id = o.identity("tok").await.expect("identity");

    assert_eq!(id.user_id, 583231);
    assert_eq!(id.login, "arsfeld");
    assert_eq!(id.orgs.len(), 1);
    assert_eq!(id.orgs[0].id, 9919);
}

#[tokio::test]
async fn an_error_body_with_a_200_status_is_still_an_error() {
    // GitHub's token endpoint answers 200 with {"error": ...} on a bad code.
    // Treating status alone as success hands an empty token to the next call,
    // which then fails somewhere far away from the cause.
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path}};
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({ "error": "bad_verification_code" })))
        .mount(&server).await;

    let o = GithubOauth::new("cid".into(), "sec".into(), "http://x/cb".into())
        .with_token_endpoint(format!("{}/login/oauth/access_token", server.uri()));
    assert!(o.exchange("nope").await.is_err());
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control github::`
Expected: FAIL — `identity` not defined.

- [ ] **Step 3: Implement**

```rust
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubOrg { pub id: i64, pub login: String }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubIdentity { pub user_id: i64, pub login: String, pub orgs: Vec<GithubOrg> }

#[derive(Deserialize)]
struct AccountJson { id: i64, login: String }

#[derive(Deserialize)]
#[serde(untagged)]
enum TokenJson {
    Ok { access_token: String },
    Err { error: String },
}

impl GithubOauth {
    pub async fn exchange(&self, code: &str) -> Result<String, ControlError> {
        let res: TokenJson = reqwest::Client::new()
            .post(&self.token_endpoint)
            .header("accept", "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", self.redirect_uri.as_str()),
            ])
            .send().await?
            .json().await?;

        match res {
            TokenJson::Ok { access_token } => Ok(access_token),
            TokenJson::Err { error } => Err(ControlError::Github(error)),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self, token: &str, path: &str,
    ) -> Result<T, ControlError> {
        Ok(reqwest::Client::new()
            .get(format!("{}{path}", self.api_base))
            .header("authorization", format!("Bearer {token}"))
            .header("accept", "application/vnd.github+json")
            .header("user-agent", "memorysafe-cloud")
            .send().await?
            .error_for_status()?
            .json().await?)
    }

    pub async fn identity(&self, token: &str) -> Result<GithubIdentity, ControlError> {
        let user: AccountJson = self.get_json(token, "/user").await?;
        let orgs: Vec<AccountJson> = self.get_json(token, "/user/orgs").await?;
        Ok(GithubIdentity {
            user_id: user.id,
            login: user.login,
            orgs: orgs.into_iter().map(|o| GithubOrg { id: o.id, login: o.login }).collect(),
        })
    }
}
```

Add `Github(String)` and `Http(#[from] reqwest::Error)` variants to `ControlError`, a `token_endpoint` field defaulting to the `TOKEN` constant, and `with_token_endpoint`. Add `wiremock = "0.6"` to dev-dependencies.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control github::`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): token exchange and identity, with the 200-plus-error case"
```

---

### Task 10: Signed session cookies

**Files:**
- Create: `crates/memorysafe-cloud-control/src/session.rs`

**Interfaces:**
- Produces: `pub struct Session { pub user_id: i64, pub login: String, pub orgs: Vec<GithubOrg>, pub issued_at: i64 }`, `pub const COOKIE_NAME: &str`, `pub fn write(jar: SignedCookieJar, s: &Session) -> SignedCookieJar`, `pub fn read(jar: &SignedCookieJar) -> Option<Session>`, and an `axum` extractor `impl FromRequestParts for Session`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum_extra::extract::cookie::{Key, SignedCookieJar};

    fn session() -> Session {
        Session { user_id: 7, login: "dev".into(),
                  orgs: vec![GithubOrg { id: 9919, login: "acme".into() }],
                  issued_at: 1_700_000_000 }
    }

    #[test]
    fn a_session_round_trips_through_a_signed_jar() {
        let key = Key::generate();
        let jar = write(SignedCookieJar::new(key.clone()), &session());
        assert_eq!(read(&jar).expect("readable"), session());
    }

    #[test]
    fn a_session_signed_with_another_key_is_not_readable() {
        // The whole point of signing. If this ever passes, anyone can mint a
        // session naming any GitHub id and read that tenant's memories.
        let jar = write(SignedCookieJar::new(Key::generate()), &session());
        let raw = jar.get(COOKIE_NAME).expect("cookie present").value().to_owned();

        let other = SignedCookieJar::new(Key::generate())
            .add(axum_extra::extract::cookie::Cookie::new(COOKIE_NAME, raw));
        assert!(read(&other).is_none(), "a forged session must not deserialize");
    }

    #[test]
    fn an_expired_session_is_refused() {
        let mut s = session();
        s.issued_at = time::OffsetDateTime::now_utc().unix_timestamp() - MAX_AGE_SECS - 1;
        let jar = write(SignedCookieJar::new(Key::generate()), &s);
        assert!(read(&jar).is_none());
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control session::`
Expected: FAIL — `Session` not defined.

- [ ] **Step 3: Implement**

```rust
use crate::github::GithubOrg;
use axum_extra::extract::cookie::{Cookie, SameSite, SignedCookieJar};
use serde::{Deserialize, Serialize};

pub const COOKIE_NAME: &str = "ms_session";
pub const MAX_AGE_SECS: i64 = 7 * 24 * 60 * 60;

/// The whole session, in the cookie. There is no session table by design: with
/// GitHub as the only identity provider there is no server-side state a session
/// row would hold that the cookie does not already carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub user_id: i64,
    pub login: String,
    pub orgs: Vec<GithubOrg>,
    pub issued_at: i64,
}

pub fn write(jar: SignedCookieJar, s: &Session) -> SignedCookieJar {
    let json = serde_json::to_string(s).expect("Session is always serialisable");
    let cookie = Cookie::build((COOKIE_NAME, json))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)   // Lax, not Strict: the OAuth callback is a
                                    // cross-site navigation and Strict drops it.
        .max_age(time::Duration::seconds(MAX_AGE_SECS))
        .build();
    jar.add(cookie)
}

pub fn read(jar: &SignedCookieJar) -> Option<Session> {
    let raw = jar.get(COOKIE_NAME)?;
    let s: Session = serde_json::from_str(raw.value()).ok()?;
    let age = time::OffsetDateTime::now_utc().unix_timestamp() - s.issued_at;
    (age >= 0 && age <= MAX_AGE_SECS).then_some(s)
}
```

`GithubOrg` must derive `Serialize, Deserialize` — add them in `github.rs`.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control session::`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): signed session cookies with no session table"
```

---

### Task 11: Tenant selection and provisioning with a default budget

**Files:**
- Create: `crates/memorysafe-cloud-control/src/routes/mod.rs`, `src/routes/auth.rs`
- Modify: `src/tenants.rs`

**Interfaces:**
- Consumes: `Session` (Task 10), `upsert_tenant` (Task 4), `GithubIdentity` (Task 9).
- Produces: `pub fn tenants_visible_to(session: &Session) -> Vec<(GithubAccountKind, i64, String)>`, `pub async fn provision(pool, engine, kind, github_id, login) -> Result<TenantRecord, ControlError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_session_sees_its_personal_tenant_and_every_org_it_belongs_to() {
    let s = Session { user_id: 7, login: "dev".into(),
        orgs: vec![GithubOrg { id: 100, login: "acme".into() },
                   GithubOrg { id: 200, login: "beta".into() }],
        issued_at: 0 };

    let visible = tenants_visible_to(&s);
    assert_eq!(visible.len(), 3, "personal + two orgs");
    assert_eq!(visible[0], (GithubAccountKind::User, 7, "dev".to_string()));
    assert!(visible.contains(&(GithubAccountKind::Org, 100, "acme".to_string())));
}

#[tokio::test(flavor = "multi_thread")]
async fn provisioning_sets_the_free_tier_budget() {
    let (_c, pool) = crate::test_support::pg().await;
    crate::schema::bootstrap(&pool).await.unwrap();
    let engine = crate::test_support::sqlite_engine();  // a real Engine, SQLite-backed

    let t = provision(&pool, &engine, GithubAccountKind::User, 7, "dev").await.unwrap();

    // The engine's own capacity governance IS the free-tier limiter (design
    // §9.2). If provisioning forgets it, a free tenant is unbounded and the
    // hosting bill is the alarm.
    let state = engine.capacity_state(&Scope::new(t.tenant_id.as_str(), "default", "default").unwrap())
        .await.unwrap();
    assert_eq!(state.budget.max_items, Some(FREE_TIER_MAX_ITEMS));
    assert_eq!(state.budget.max_bytes, Some(FREE_TIER_MAX_BYTES));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control routes::`
Expected: FAIL — `tenants_visible_to` not defined.

- [ ] **Step 3: Implement**

```rust
pub const FREE_TIER_MAX_ITEMS: u64 = 10_000;
pub const FREE_TIER_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_NAMESPACE: &str = "default";

/// The personal tenant first, then orgs in the order GitHub returned them.
/// Membership is read from the session, which was populated from GitHub at
/// login — never from a stored memberships table. Design §5.2: storing it means
/// revoking someone's org access in GitHub would not revoke their access here.
pub fn tenants_visible_to(session: &Session) -> Vec<(GithubAccountKind, i64, String)> {
    let mut out = vec![(GithubAccountKind::User, session.user_id, session.login.clone())];
    out.extend(session.orgs.iter().map(|o| (GithubAccountKind::Org, o.id, o.login.clone())));
    out
}

/// Idempotent. Under `SharedPartitioned` a tenant is only rows, so this creates
/// no schema and runs no DDL (design §5.3) — the cost of a new customer is one
/// INSERT and one budget write.
pub async fn provision(
    pool: &PgPool,
    engine: &Engine,
    kind: GithubAccountKind,
    github_id: i64,
    login: &str,
) -> Result<TenantRecord, ControlError> {
    let record = upsert_tenant(pool, kind, github_id, login).await?;
    let scope = Scope::new(record.tenant_id.as_str(), DEFAULT_NAMESPACE, DEFAULT_NAMESPACE)
        .map_err(|_| ControlError::InvalidTenantId(record.tenant_id.to_string()))?;
    engine
        .set_budget(&scope, Budget {
            max_items: Some(FREE_TIER_MAX_ITEMS),
            max_bytes: Some(FREE_TIER_MAX_BYTES),
        })
        .await
        .map_err(ControlError::Engine)?;
    Ok(record)
}
```

Add `Engine(#[from] memorysafe_engine::EngineError)` to `ControlError`, and `memorysafe-engine`, `memorysafe-embed`, `memorysafe-policy`, `memorysafe-backend-sqlite` as dependencies (the last three dev-only, for `test_support::sqlite_engine`).

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control routes::`
Expected: PASS — 2 tests.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): tenant visibility from the session, provisioning with a free-tier budget"
```

---

### Task 12: Dashboard — key listing, creation, revocation

**Files:**
- Create: `crates/memorysafe-cloud-control/src/routes/keys.rs`, `src/routes/dashboard.rs`

**Interfaces:**
- Consumes: Tasks 5, 6, 10, 11.
- Produces: `pub fn router(state: ControlState) -> axum::Router`, handling `GET /`, `POST /tenants/:tenant/keys`, `POST /tenants/:tenant/keys/:key_id/revoke`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_created_key_is_shown_once_and_never_again() {
    let h = harness().await;                    // app + signed session for user 7
    let res = h.post("/tenants/u_7/keys", "label=laptop").await;
    assert_eq!(res.status(), 200);
    let body = res.text().await;
    let secret = extract_secret(&body).expect("the secret is shown at creation");
    assert!(secret.starts_with("msk_"));

    let listing = h.get("/").await.text().await;
    assert!(listing.contains("laptop"), "the label is listed");
    assert!(!listing.contains(&secret), "the secret must never appear again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_cannot_mint_a_key_for_a_tenant_it_does_not_belong_to() {
    // The whole authorization surface of the dashboard in one test: the tenant
    // comes from the PATH, and the path is attacker-controlled.
    let h = harness().await;                    // session for user 7, no orgs
    let res = h.post("/tenants/o_9919/keys", "label=stolen").await;
    assert_eq!(res.status(), 403);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control routes::keys`
Expected: FAIL — router not defined.

- [ ] **Step 3: Implement the authorization guard first**

```rust
/// Resolves a path-supplied tenant against the session, or refuses.
///
/// Every handler that takes a tenant from the path goes through this. The path
/// is attacker-controlled; the session is not.
fn authorize(session: &Session, requested: &str) -> Result<TenantId, StatusCode> {
    tenants_visible_to(session)
        .into_iter()
        .filter_map(|(kind, id, _)| tenant_id_for(kind, id).ok())
        .find(|t| t.as_str() == requested)
        .ok_or(StatusCode::FORBIDDEN)
}
```

- [ ] **Step 4: Implement the handlers**

```rust
async fn create_key(
    State(st): State<ControlState>,
    session: Session,
    Path(tenant): Path<String>,
    Form(form): Form<CreateKeyForm>,
) -> Result<Html<String>, StatusCode> {
    let tenant = authorize(&session, &tenant)?;
    let generated = memorysafe_auth::generate(tenant, &form.label)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    keys::insert_key(st.registry.pool(), &generated.record)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Shown once. Not logged, not stored, not returned again — the only copy
    // that will ever exist leaves in this response body.
    Ok(Html(render_created(&generated)))
}

async fn revoke_key(
    State(st): State<ControlState>,
    session: Session,
    Path((tenant, key_id)): Path<(String, String)>,
) -> Result<Redirect, StatusCode> {
    let tenant = authorize(&session, &tenant)?;
    keys::disable_key(st.registry.pool(), &tenant, &key_id)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Redirect::to("/"))
}
```

- [ ] **Step 5: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control routes::keys`
Expected: PASS — 2 tests.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(control): dashboard key lifecycle, tenant authorized from the session"
```

---

### Task 13: Log redaction and its enforcement test

**Files:**
- Create: `crates/memorysafe-cloud-control/src/redact.rs`

**Interfaces:**
- Produces: `pub fn redacting_layer<S>() -> impl Layer<S>`, `pub const DENIED_FIELDS: &[&str]`.

**Why this is a test and not a convention:** design §9.4. The audit design's central promise is that records carry ids, digests, and feature numbers but never memory bodies. A `tracing` field added six months from now will not be caught by code review.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_field_never_reaches_the_sink() {
        let sink = TestSink::default();
        let _g = tracing::subscriber::set_default(subscriber_with(sink.clone()));

        tracing::info!(item_id = "01J...", body = "the user's private memory", "admitted");
        tracing::info!(api_key = "msk_01J_deadbeef", "authenticated");

        let out = sink.contents();
        assert!(out.contains("01J..."), "ids are allowed and useful");
        assert!(!out.contains("the user's private memory"), "bodies must be redacted");
        assert!(!out.contains("deadbeef"), "key secrets must be redacted");
        assert!(out.contains("[redacted]"), "redaction is visible, not silent");
    }

    #[test]
    fn the_denylist_covers_every_field_name_the_workspace_actually_logs() {
        // Guards against the field being renamed and quietly escaping the list.
        for name in ["body", "secret", "api_key", "token", "access_token", "query"] {
            assert!(DENIED_FIELDS.contains(&name), "{name} must be denied");
        }
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control redact::`
Expected: FAIL — `redacting_layer` not defined.

- [ ] **Step 3: Implement**

```rust
/// Field names whose values never reach a log sink at any level.
///
/// `query` is here because a recall query is user text and can contain exactly
/// what a memory body contains. `body` because it is one. The rest are
/// credentials.
pub const DENIED_FIELDS: &[&str] = &[
    "body", "secret", "api_key", "token", "access_token", "client_secret", "query", "password",
];

pub const REDACTED: &str = "[redacted]";
```

Implement a `tracing_subscriber::Layer` whose visitor replaces the value of any field whose name is in `DENIED_FIELDS` with `REDACTED`, leaving other fields untouched.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control redact::`
Expected: PASS — 2 tests.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): redaction layer, enforced by test rather than convention"
```

---

### Task 14: Per-key rate limiting

**Files:**
- Modify: `crates/memorysafe-cloud-control/src/routes/mod.rs`

**Interfaces:**
- Produces: a `tower` layer keying the limiter on `Authenticated::key_id`, not on IP.

**Why not IP:** every request from one customer's backend shares an IP, so an IP limiter either throttles a whole customer at one user's rate or is set so high it protects nothing. The key id is the unit of both identity and billing.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn one_key_exhausting_its_budget_does_not_throttle_another() {
    let h = api_harness().await;
    let a = h.mint_key("tenant-a").await;
    let b = h.mint_key("tenant-b").await;

    for _ in 0..BURST { assert_eq!(h.get_with(&a, "/v1/health").await.status(), 200); }
    assert_eq!(h.get_with(&a, "/v1/health").await.status(), 429, "a is limited");
    assert_eq!(h.get_with(&b, "/v1/health").await.status(), 200, "b is unaffected");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-control routes::rate`
Expected: FAIL — no limiter.

- [ ] **Step 3: Implement** the limiter keyed on key id, returning `429` with a `Retry-After` header.

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-control routes::rate`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(control): per-key rate limiting, keyed on key id not IP"
```

---

### Task 15: The composition root binary

> **Blocked on Plan 2 and Plan 3.** Requires `memorysafe-backend-postgres::PostgresBackend` and `memorysafe_api::router`. Do not start until both exist.

**Files:**
- Create: `crates/memorysafe-cloud-server/Cargo.toml`, `src/main.rs`, `src/config.rs`

**Interfaces:**
- Consumes: everything above, plus `PostgresBackend` (Plan 2) and `memorysafe_api` / `memorysafe_mcp` (Plan 3).
- Produces: the `memorysafe-cloud-server` binary.

- [ ] **Step 1: Write the failing test — config refuses to start without its secrets**

```rust
#[test]
fn config_refuses_to_start_without_a_cookie_key_or_oauth_secret() {
    // Failing closed at startup is the point: a server that boots with a
    // generated-on-the-fly cookie key silently invalidates every session on
    // every restart, and one that boots without OAuth config 500s per login.
    let mut env = minimal_env();
    env.remove("MS_COOKIE_KEY");
    assert!(Config::from_env(&env).is_err());

    let mut env = minimal_env();
    env.remove("MS_GITHUB_CLIENT_SECRET");
    assert!(Config::from_env(&env).is_err());
}

#[test]
fn the_github_api_base_is_not_configurable() {
    // The test seam from Task 8 must not be reachable from production config,
    // or an environment variable can redirect identity lookups to an attacker.
    let mut env = minimal_env();
    env.insert("MS_GITHUB_API_BASE".into(), "http://evil.example".into());
    let c = Config::from_env(&env).expect("still valid");
    assert_eq!(c.github_api_base(), memorysafe_cloud_control::github::DEFAULT_API_BASE);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-server`
Expected: FAIL — `Config` not defined.

- [ ] **Step 3: Implement config, then the composition root**

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env(&std::env::vars().collect())?;
    tracing_subscriber::registry()
        .with(memorysafe_cloud_control::redact::redacting_layer())
        // DO NOT add `.with(tracing_subscriber::fmt::layer().json())` here.
        // An earlier draft of this plan did, and it defeats Task 13 entirely:
        // every Layer in a registry receives each event and formats and writes
        // it INDEPENDENTLY, so a second formatter emits the same events
        // unredacted alongside the redacted ones. Layer ORDER is irrelevant —
        // this is not a pipeline. Task 13's tests would still pass, because
        // they exercise the redacting layer in isolation.
        //
        // Two further facts before wiring this up:
        //   * `redacting_layer` as built in Task 13 writes FIELDS ONLY — no
        //     timestamp, level or target, and no `on_new_span`, so `#[instrument]`
        //     span fields are neither printed nor redacted. Deleting the JSON
        //     layer is therefore NOT sufficient; the redacting layer must first
        //     become a real formatter, or redaction must move into a format
        //     wrapper that every writer goes through.
        //   * Whatever shape it takes, the invariant to hold is: there is
        //     exactly ONE path from an event to a sink, and it redacts.
        .init();

    let pool = PgPoolOptions::new().max_connections(config.max_connections)
        .connect(&config.database_url).await?;

    // The ONLY place a backend, an embedder, and a policy are chosen.
    let backend = Arc::new(PostgresBackend::connect(&config.database_url, config.pg()).await?);
    let embedder = Arc::new(Model2VecEmbedder::from_baked_in()?);
    let policy = Arc::new(BaselinePolicy::default());
    let engine = Arc::new(Engine::new(EngineConfig::new(backend, embedder, policy)));

    let registry = Arc::new(KeyRegistry::load(pool.clone()).await?);
    let _refresh = spawn_refresh(registry.clone(), config.database_url.clone());

    let app = Router::new()
        .nest("/mcp", memorysafe_mcp::http_router(engine.clone(), registry.clone()))
        .nest("/v1",  memorysafe_api::router(engine.clone(), registry.clone()))
        .merge(memorysafe_cloud_control::routes::router(control_state))
        .layer(TraceLayer::new_for_http());

    axum::serve(TcpListener::bind(config.bind).await?, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-server && cargo build --release`
Expected: PASS, and a binary.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(server): the composition root, the only place a backend is chosen"
```

---

### Task 16: RLS enforcement at the composition root

> **Blocked on Plan 2.**

**Files:**
- Create: `crates/memorysafe-cloud-server/tests/rls.rs`

**Why here and not in Plan 2:** Plan 2 proves RLS works against its own test harness. This proves it works with *this deployment's* pool role, connection string, and `SET ROLE` configuration — the thing that would silently be a superuser in production.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_pool_role_cannot_bypass_row_level_security() {
    let (_c, url) = pg_with_bootstrap().await;
    let pool = runtime_pool(&url).await;      // exactly what main() builds

    // Belt: the role must not be able to bypass RLS even if a query forgets its
    // predicate. Braces: it must not be superuser, because a superuser ignores
    // RLS entirely and every isolation test would pass vacuously.
    let (is_super, bypass): (bool, bool) = sqlx::query_as(
        "SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user")
        .fetch_one(&pool).await.unwrap();
    assert!(!is_super, "runtime pool must not be superuser");
    assert!(!bypass, "runtime pool must not have BYPASSRLS");

    seed_item(&pool, "tenant-a", "a's memory").await;

    // The predicate is deliberately absent. RLS, not the WHERE clause, must be
    // what returns nothing.
    set_tenant_guc(&pool, "tenant-b").await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM items")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0, "tenant-b saw tenant-a's rows with no predicate in the query");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p memorysafe-cloud-server --test rls`
Expected: FAIL.

- [ ] **Step 3: Fix whichever of the role grants, `FORCE ROW LEVEL SECURITY`, or the pool's `SET ROLE` is missing.**

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p memorysafe-cloud-server --test rls`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(server): the deployment's own pool role cannot bypass RLS"
```

---

### Task 17: The container image

**Files:**
- Create: `Dockerfile`, `.dockerignore`

- [ ] **Step 1: Write the failing test**

```bash
# scripts/test-image.sh
set -euo pipefail
docker build -t memorysafe-cloud:test .
# The model must be IN the image: a download at boot would put a network call
# on the startup path and break the offline-write guarantee.
docker run --rm memorysafe-cloud:test ls /opt/memorysafe/model >/dev/null
# Non-root, because the container has a database password in its environment.
test "$(docker run --rm memorysafe-cloud:test id -u)" != "0"
```

- [ ] **Step 2: Run it and watch it fail**

Run: `bash scripts/test-image.sh`
Expected: FAIL — no Dockerfile.

- [ ] **Step 3: Write the Dockerfile**

```dockerfile
FROM rust:1.97.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p memorysafe-cloud-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 memorysafe
COPY --from=build /src/target/release/memorysafe-cloud-server /usr/local/bin/
COPY --from=build /src/models/ /opt/memorysafe/model/
USER 10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/memorysafe-cloud-server"]
```

- [ ] **Step 4: Run it and watch it pass**

Run: `bash scripts/test-image.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(deploy): container image with the model baked in, running non-root"
```

---

### Task 18: k3s manifests — Deployment, StatefulSet, Ingress

**Files:**
- Create: `deploy/base/{kustomization,deployment,statefulset,service,ingress}.yaml`
- Create: `deploy/overlays/production/kustomization.yaml`

- [ ] **Step 1: Write the failing test**

```bash
# scripts/test-manifests.sh
set -euo pipefail
kubectl kustomize deploy/overlays/production > /tmp/rendered.yaml

# Postgres must NOT be a RollingUpdate Deployment. With a node-local
# local-path PV, a rolling update briefly runs two pods against one data
# directory. Design §8.4, constraint 1.
kind=$(yq 'select(.metadata.name == "postgres") | .kind' /tmp/rendered.yaml)
test "$kind" = "StatefulSet"

# The app must declare resource limits, or an HNSW index build can starve
# whatever else shares can-1. Design §13.0.
yq 'select(.kind == "Deployment") | .spec.template.spec.containers[0].resources.limits.memory' \
  /tmp/rendered.yaml | grep -q .

# No secret literals in a manifest.
! grep -Ei 'client_secret:|COOKIE_KEY: [A-Za-z0-9+/]{8}' /tmp/rendered.yaml
```

- [ ] **Step 2: Run it and watch it fail**

Run: `bash scripts/test-manifests.sh`
Expected: FAIL — no manifests.

- [ ] **Step 3: Write the manifests.** Deployment with probes, resource limits, and `envFrom` a Secret. StatefulSet for Postgres with a `local-path` `volumeClaimTemplate`. Traefik `Ingress` with a TLS entry.

- [ ] **Step 4: Run it and watch it pass**

Run: `bash scripts/test-manifests.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(deploy): k3s manifests, postgres as a StatefulSet"
```

---

### Task 19: Bootstrap Job, backup CronJob, export CronJob

**Files:**
- Create: `deploy/base/{bootstrap-job,backup-cronjob,export-cronjob}.yaml`

- [ ] **Step 1: Write the failing test — a restore actually works**

```bash
# scripts/test-backup-restore.sh
# The one test that matters here. An untested backup is not a backup
# (design §9.3), and this is the rehearsal made executable.
set -euo pipefail
seed_database
bash deploy/scripts/backup.sh /tmp/dump.sql.gz
drop_database
bash deploy/scripts/restore.sh /tmp/dump.sql.gz
test "$(count_items)" = "$(expected_items)"
```

- [ ] **Step 2: Run it and watch it fail**

Run: `bash scripts/test-backup-restore.sh`
Expected: FAIL — no backup script.

- [ ] **Step 3: Write the Job and CronJobs.** Bootstrap Job gated on `SCHEMA_VERSION`, running before rollout. Backup CronJob nightly, `pg_dump` piped to object storage. Export CronJob weekly, using the engine's ndjson export.

- [ ] **Step 4: Run it and watch it pass**

Run: `bash scripts/test-backup-restore.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(deploy): bootstrap job, nightly dump, weekly ndjson export"
```

---

### Task 20: Acceptance — smoke test, runbook, `can-1` audit

**Files:**
- Create: `crates/memorysafe-cloud-server/tests/smoke.rs`, `docs/runbook.md`, `docs/can-1-audit.md`

- [ ] **Step 1: Write the end-to-end smoke test**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_tenant_can_be_provisioned_and_used_end_to_end() {
    let h = full_stack().await;                        // container + server + fake GitHub

    let session = h.login_as_github_user(583231, "arsfeld").await;
    let key = h.create_key(&session, "u_583231", "smoke").await;

    let remembered = h.mcp_remember(&key, "the user prefers concise Rust").await;
    assert!(remembered.status().is_success());

    let recalled = h.mcp_recall(&key, "coding preferences").await;
    assert!(recalled.text().await.contains("concise Rust"));

    let audit = h.api_audit(&key).await;
    let body = audit.text().await;
    assert!(body.contains("admitted"));
    assert!(!body.contains("the user prefers concise Rust"),
            "an audit record must never carry the body");
}
```

- [ ] **Step 2: Run it and watch it fail, then make it pass.**

- [ ] **Step 3: Write `docs/can-1-audit.md`** — the §13.0 precondition checklist as commands with expected output: free RAM, disk type and free space, root access, what else runs on the box, CPU architecture, and DNS/port reachability. Record the answers in the file; a decision that lives only in a conversation dies with it.

- [ ] **Step 4: Write `docs/runbook.md`** — deploy, roll back, restore from dump, rotate the cookie key, rotate the GitHub OAuth secret, revoke a key out-of-band, and read the audit aggregates for one tenant.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "test(server): end-to-end smoke, plus the runbook and the can-1 audit"
```

---

## Self-Review

**Spec coverage.** Design §3 → Tasks 1, 15. §4 → Tasks 15, 17. §5 → Tasks 3, 4, 8–12. §6 → Tasks 5–7. §7 → Task 2. §8 → Tasks 17–19. §9.1 → Task 19. §9.2 → Task 11. §9.3 → Task 19. §9.4 → Task 13. §9.5 → Tasks 15, 18. §10 → Tasks 12, 16, 20. §11 → Task 20. §13.0 → Task 20 Step 3.

**Two spec items deliberately not given their own task**, recorded here so the omission is visible rather than accidental:

- **§3.1's missing `LICENSE` file** belongs to the *open* repository and cannot be fixed from `memorysafe-cloud`. It is tracked as a one-line change on `master`, outside this plan.
- **§13.1's `halfvec` amendment** is explicitly a proposal, not a decision, and it modifies Plan 2's DDL. It belongs in Plan 2's task list once Plan 2 re-validates it, not here.

**Placeholder scan.** Tasks 13, 14, 16, 18, 19 have implementation steps stated as intent rather than code — the redaction visitor, the limiter, the RLS remediation, three manifests, and three shell scripts. Each is bounded by a test above it that specifies the required behaviour exactly, which is the contract an implementer needs; the code is mechanical from there. Every task that defines a *type or signature another task consumes* carries full code, which is where drift would actually hurt.

**Type consistency.** `ControlError` accumulates variants across Tasks 2, 9, 11 — `Db`, `InvalidTenantId`, `Auth`, `Github`, `Http`, `Engine`. `ApiKeyRecord.id` is the key id throughout and maps to the `key_id` column; `tenant` maps to `tenant_id`. `KeyRegistry::pool()` is introduced in Task 6 and consumed in Task 12. `GithubOrg` gains `Serialize`/`Deserialize` in Task 10 for `Session`.

---

## Prerequisite corrections before implementation

1. **Plan 2's conformance count is 55, not 49.** Its goal statement and task acceptance criteria say "frozen 49-test conformance suite" with a table of 5/6/13/4/21. The authoritative `run!` in `vendor/memorysafe/crates/memorysafe-backend/src/conformance/mod.rs` runs 55 — isolation 5, atomicity 8, retrieval 14, capacity 5, lifecycle 23. Fix Plan 2 before implementing it, per its own instruction to recount rather than adjust by a difference.
2. **Add a `LICENSE` file to the open repository.** The README carries an Apache-2.0 badge and no license file exists.
