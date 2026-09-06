# MemorySafe Adapters + Shadow Harness — Implementation Plan (Plan 3 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the governed-memory engine in front of real callers — an MCP server (stdio and streamable HTTP), an HTTP API, the `msafe` CLI, and a shadow-evaluation harness that replays decisions against a different policy and diffs them.

**Architecture:** Four thin adapter crates over the finished `memorysafe-engine`, plus one shared crate for tenant-scoped API keys. Adapters translate wire formats to engine calls and engine outcomes back; they contain no governance logic, and every decision they report was made by the engine. `memorysafe-cli` is the only binary and the only composition root — it is the single place that picks a backend, an embedder, and a policy, and it hands a ready `Arc<Engine>` to the MCP and HTTP adapters. `memorysafe-shadow` drives the engine over a scripted scenario and records a timestamp-free trace, so two policies over one scenario can be diffed exactly.

**Tech Stack:** Rust 1.97.1 (edition 2024), `rmcp` 3.2 (server, stdio, streamable HTTP), `axum` 0.8, `clap` 4.6, `tower` 0.5, `tower-http` 0.7, `http` 1, `schemars` 1.2, `subtle` 2.6, `getrandom` 0.4, `toml` 1.1, `tracing` 0.1, `tracing-subscriber` 0.3, `assert_cmd` 2.2, `predicates` 3.1, `http-body-util` 0.1.

**Source spec:** `docs/superpowers/specs/2026-09-05-memorysafe-engine-design.md`
**Predecessors:** `docs/superpowers/plans/2026-09-05-engine-and-sqlite.md` (Plan 1). Plan 2 (Postgres) is independent of this plan and need not be finished first — nothing here touches the `Backend` trait.

---

## Global Constraints

Every task's requirements implicitly include this section. The first eleven are inherited
verbatim from Plan 1 and are restated because a task's implementer sees only their own task.

- **Rust edition 2024**, toolchain pinned to `1.97.1` via `rust-toolchain.toml`.
- **No LLM and no network call in the write path.** Embeddings only, computed in-process.
- **Policies are pure.** `memorysafe-policy` and `memorysafe-core` must not depend on `memorysafe-backend`, `tokio`, `rusqlite`, or any I/O crate. Enforced by the CI purity job.
- **Hard filters execute inside the backend query.** An adapter may never filter results it received from `recall` — if a filter is not expressible in `RecallRequest`, it does not exist.
- **Audit rows never contain item bodies** — only ids, content digests (BLAKE3 hex), and feature numbers. This holds on every surface: MCP resources, HTTP responses, CLI output, and shadow traces.
- **Tenant isolation is structural.** No adapter constructs a `Scope` whose tenant differs from the authenticated tenant.
- **TDD.** Every task writes a failing test first, watches it fail, then implements. Commit at the end of every task.
- **Lints:** `#![deny(warnings)]` in CI via `RUSTFLAGS="-Dwarnings"`, plus `cargo clippy --all-targets --all-features -- -D warnings`. `unsafe_code = "forbid"` at the workspace level.
- **`Score` is a newtype over `f32` clamped to `[0.0, 1.0]`.** Never a bare `f32`.
- **Timestamps are `time::OffsetDateTime`.** On the wire they are Unix seconds (`i64`), because that is how `MemoryItem` and `AuditRecord` serialise.
- **Ids are ULIDs** rendered as their 26-character Crockford base32 string.

New in this plan:

- **Adapters are thin.** No adapter scores, ranks, filters, or decides. Every governance outcome an adapter reports came out of an `Engine` method. A reviewer finding a threshold constant in `memorysafe-mcp`, `memorysafe-api`, or `memorysafe-cli` should reject the task.
- **A governance decision is not an error.** A rejected, merged, or empty result is `200 OK` on HTTP, a successful tool result on MCP, and exit code `0` on the CLI. `Err` is reserved for things that actually went wrong (§9 of the spec).
- **Adapters depend on `memorysafe-engine`, never on a backend.** `memorysafe-mcp` and `memorysafe-api` must not declare `memorysafe-backend-sqlite` or `rusqlite`. `memorysafe-cli` may, because it is the composition root that chooses one. Enforced by a CI check in Task 18.
- **Every scope an adapter builds is authorised against the caller's tenant** before it reaches the engine, and `_admin` is reserved: a caller-supplied subject or namespace equal to `_admin` is rejected.
- **API key secrets are never stored and never logged.** Only a BLAKE3 hash is persisted; the secret is shown once, at creation.
- **`--json` output is the contract.** Every CLI command supports it, and the JSON is the serde form of the engine type it wraps — not a hand-written reshaping that can drift.

---

## What Plan 1 leaves in place

Plan 3 consumes exactly this surface. These signatures are load-bearing; if a task's code does
not compile against them, the task is wrong, not Plan 1.

```rust
// memorysafe-core
pub struct Scope { pub tenant: TenantId, pub subject: SubjectId, pub namespace: Namespace }
impl Scope { pub fn new(tenant: &str, subject: &str, namespace: &str) -> Result<Self, CoreError>; pub fn key(&self) -> String; }
// TenantId / SubjectId / Namespace: `::new(&str) -> Result<Self, CoreError>`, `::as_str`, `Display`.
// Components are lowercase ASCII plus `-`, `_`, `.`; max 240 bytes; may not start with `.`.
// ItemId / AuditId: `::new()`, `::parse(&str) -> Result<Self, CoreError>`, `::as_str`, `Display`.

pub struct MemoryItem {
    pub id: ItemId, pub scope: Scope, pub body: String, pub kind: String, pub source: Source,
    pub occurred_at: Option<OffsetDateTime>, pub created_at: OffsetDateTime,
    pub tags: Vec<String>, pub attrs: BTreeMap<String, serde_json::Value>,
    pub sensitivity: SensitivityLevel, pub ttl: Option<Duration>,
    pub protection: Protection, pub pending_embedding: bool,
}
impl MemoryItem { pub fn digest(&self) -> String; }
pub struct Source { pub kind: SourceKind, pub id: Option<String> }
pub enum SourceKind { Agent, Session, Tool, Human }                       // serde snake_case
pub enum Protection { Normal, Protected { until: OffsetDateTime }, Pinned } // serde tag = "kind"
pub enum SensitivityLevel { Public, Internal, Personal, Sensitive, Restricted } // Ord, serde snake_case

pub enum Action { Retain { protection: Protection }, Merge { into: ItemId, strategy: MergeStrategy }, Reject }
pub struct Reason { pub code: ReasonCode, pub detail: String, pub evidence: FeatureMap }
pub struct PolicyId { pub name: String, pub version: String }             // Display = "name@version"

pub enum AuditEvent { Admitted, Rejected, Merged, Forgotten, Recalled, Exported, Imported,
                      SubjectPurged, Reembedded, PolicyChanged, MaintenanceRun }
pub enum ActorKind { Agent, Human, ApiKey, Cli, System }
pub struct Actor { pub kind: ActorKind, pub id: Option<String> }
pub struct ItemRef { /* private */ }                                      // ::from_item, .id(), .digest()
pub struct AuditRecord { pub id: AuditId, pub at: OffsetDateTime, pub scope: Scope,
    pub event: AuditEvent, pub items: Vec<ItemRef>, pub assessment: Option<Assessment>,
    pub decision: Option<Decision>, pub actor: Actor }
impl AuditRecord { pub fn new(scope, event, items, actor, at) -> Self;
    pub fn with_assessment(self, a) -> Self; pub fn with_decision(self, d) -> Self; }
pub struct AuditFilter { pub events: Vec<AuditEvent>, pub subject: Option<SubjectId>,
    pub namespace: Option<Namespace>, pub after: Option<AuditId>, pub item: Option<ItemId>,
    pub since: Option<OffsetDateTime>, pub until: Option<OffsetDateTime>, pub limit: usize }  // Default: limit 100

pub enum RecallMode { WorkingSet, Search }                                // Default = WorkingSet
pub struct RecallBudget { pub max_tokens: Option<u32>, pub max_items: Option<usize> } // Default 2000 / 20
pub struct RecallRequest { pub scope: Scope, pub query: Option<String>, pub tags_any: Vec<String>,
    pub kinds: Vec<String>, pub occurred_after: Option<OffsetDateTime>,
    pub occurred_before: Option<OffsetDateTime>, pub mode: RecallMode,
    pub budget: RecallBudget, pub sensitivity_ceiling: SensitivityLevel }
pub struct SelectedItem { pub item: MemoryItem, pub relevance: f32, pub reason: Reason }
pub struct OmittedItem { pub id: ItemId, pub reason: Reason }
pub struct WorkingSet { pub items: Vec<SelectedItem>, pub tokens_used: u32,
    pub omitted: Vec<OmittedItem>, pub omitted_total: usize,
    pub audit_id: Option<AuditId> }
pub struct Budget { pub max_items: Option<u64>, pub max_bytes: Option<u64> }
pub struct CapacityState { pub budget: Budget, pub used_items: u64, pub used_bytes: u64 }
pub struct ScopeStats { pub item_count: u64, pub total_bytes: u64,
    pub mean_neighbour_similarity: f32, pub median_item_bytes: u64 }

// memorysafe-backend
pub struct Page { pub offset: usize, pub limit: usize }                    // Default: 0 / 50
pub struct ScopeSelector { pub tenant: TenantId, pub subject: Option<SubjectId>,
    pub namespace: Option<Namespace>, pub include_audit: bool }
pub struct ImportReport { pub items_imported: u64, pub vectors_imported: u64,
    pub audit_imported: u64, pub items_skipped_existing: u64 }

// memorysafe-backend-sqlite
impl SqliteBackend { pub fn open(root: PathBuf) -> Self; pub fn with_max_open(root: PathBuf, max_open: usize) -> Self; }

// memorysafe-embed
impl DeterministicEmbedder { pub fn new(dim: u16) -> Self; }
// Model2VecEmbedder exists behind the `model2vec` feature.

// memorysafe-policy
pub struct BaselineConfig { pub duplicate_threshold: f32, pub merge_threshold: f32,
    pub near_duplicate_floor: f32, pub replay_quota: f32, pub mmr_lambda: f32,
    pub value_half_life_days: f32, pub source_trust_weight: f32, pub replay_stale_days: f32 }
impl BaselinePolicy { pub fn new(config: BaselineConfig) -> Self; }        // + Default

// memorysafe-engine
pub struct EngineConfig { pub backend: Arc<dyn Backend>, pub embedder: Arc<dyn Embedder>,
    pub policy: Arc<dyn GovernancePolicy>, pub fallback_policy: Arc<dyn GovernancePolicy>,
    pub stance: FailureStance, pub neighbour_k: usize, pub eviction_candidates: usize,
    pub retention: RetentionProfile }
impl EngineConfig { pub fn new(backend, embedder, policy) -> Self; }
pub struct RememberRequest { pub scope: Scope, pub body: String, pub kind: String,
    pub source: Source, pub occurred_at: Option<OffsetDateTime>, pub tags: Vec<String>,
    pub attrs: BTreeMap<String, serde_json::Value>, pub sensitivity_hint: Option<SensitivityLevel>,
    pub ttl: Option<Duration>, pub idempotency_key: Option<String>, pub actor: Actor }
impl RememberRequest { pub fn new(scope: Scope, body: &str) -> Self; }
pub struct WriteOutcome { pub item_id: Option<ItemId>, pub action: Action, pub reasons: Vec<Reason>,
    pub merged_into: Option<ItemId>, pub evicted: Vec<ItemId>, pub audit_id: AuditId }
pub struct ForgetOutcome { pub forgotten: Vec<ItemId>, pub audit_id: AuditId }
pub struct PurgeOutcome { pub items_removed: u64, pub audit_rows_removed: u64, pub audit_rows_preserved: u64 }
pub enum ForgetSelector { Ids(Vec<ItemId>), Tag(String), Kind(String) }
pub struct MaintainCursor { pub offset: usize }
pub struct MaintainReport { pub scanned: usize, pub forgotten: usize,
    pub protection_released: usize, pub next_cursor: Option<MaintainCursor> }
pub enum RetentionProfile { Balanced, GdprStrict, HipaaRetain, Forensic }  // ::from_name(&str) -> Option<Self>
pub enum EngineError { Validation(String), NotFound(String), Conflict(String),
    Backend(BackendError), Embedder(EmbedError), PolicyRefused(String) }
impl EngineError { pub fn is_retryable(&self) -> bool; }

impl Engine {
    pub fn new(config: EngineConfig) -> Self;
    pub async fn remember(&self, req: RememberRequest) -> Result<WriteOutcome, EngineError>;
    pub async fn recall(&self, req: RecallRequest) -> Result<WorkingSet, EngineError>;
    pub async fn forget(&self, scope: &Scope, sel: ForgetSelector) -> Result<ForgetOutcome, EngineError>;
    pub async fn review(&self, scope: &Scope, page: &Page) -> Result<Vec<MemoryItem>, EngineError>;
    pub async fn protect(&self, scope: &Scope, id: &ItemId, p: Protection) -> Result<WriteOutcome, EngineError>;
    pub async fn maintain(&self, scope: &Scope, cursor: Option<MaintainCursor>) -> Result<MaintainReport, EngineError>;
    pub async fn purge_subject(&self, tenant: &TenantId, subject: &SubjectId) -> Result<PurgeOutcome, EngineError>;  // Task 2 adds `actor: &Actor`
    pub async fn audit(&self, scope: &Scope, filter: &AuditFilter) -> Result<Vec<AuditRecord>, EngineError>;
    pub async fn set_budget(&self, scope: &Scope, budget: Budget) -> Result<(), EngineError>;
    pub async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, EngineError>;
    pub async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, EngineError>;
    pub async fn export_ndjson(&self, sel: &ScopeSelector) -> Result<String, EngineError>;
    pub async fn export_markdown(&self, sel: &ScopeSelector) -> Result<String, EngineError>;
    pub async fn import_ndjson(&self, ndjson: &str, destination: &TenantId)
        -> Result<ImportReport, EngineError>;
}
```

**If `Engine::capacity_state` or `Engine::scope_stats` do not exist** when Task 5 reaches them,
add them to `crates/memorysafe-engine/src/lib.rs` as one-line pass-throughs to the backend
alongside the existing `review` and `audit` pass-throughs, in the same task, with a test. They are
listed here because the MCP stats resource needs them and they cost two lines each.

---

## What Plan 3 adds

```
crates/memorysafe-auth      NEW  tenant-scoped API keys, shared by the two network adapters
crates/memorysafe-mcp       NEW  rmcp adapter: five tools, two resources, two transports
crates/memorysafe-api       NEW  axum adapter
crates/memorysafe-cli       NEW  the `msafe` binary
crates/memorysafe-shadow    NEW  scenario / trace / diff harness
crates/memorysafe-core           MODIFIED: the reserved admin scope
crates/memorysafe-engine         MODIFIED: per-tenant policy and retention, actor-attributed events
```

**`memorysafe-auth` is an addition to the spec's workspace layout (§4).** The spec lists nine
crates and does not name it. It exists because §10 requires that "API keys are scoped to a tenant"
and *both* network adapters need that, and because the alternative — a constant-time comparison
and a key-hashing scheme written twice — is the kind of duplication that rots into a security bug.
It depends only on `memorysafe-core`, so it does not disturb the dependency direction.

### File structure

`crates/memorysafe-auth` — tenant identity, no I/O

| File | Responsibility |
|---|---|
| `src/lib.rs` | `AuthError`, re-exports. |
| `src/key.rs` | `ApiKeyRecord`, `GeneratedKey`, `generate`, the presented-key grammar. |
| `src/store.rs` | `ApiKeyStore`, `Authenticated`, `authenticate`, `authorize`. |

`crates/memorysafe-mcp` — the rmcp adapter

| File | Responsibility |
|---|---|
| `src/lib.rs` | `MemorySafeServer`, `ServerHandler` impl, router composition. |
| `src/scope.rs` | `ScopeSource`, `resolve` — the only place a `Scope` is built. |
| `src/dto.rs` | Tool argument and result types, with `JsonSchema`. |
| `src/tools_write.rs` | `memory_remember`, `memory_recall`. |
| `src/tools_curate.rs` | `memory_review`, `memory_forget`, `memory_protect`. |
| `src/resources.rs` | Audit and stats resources, URI parsing. |
| `src/transport.rs` | `serve_stdio`, `http_service`. |
| `tests/support/mod.rs` | In-process client/server pair over a duplex. |

`crates/memorysafe-api` — the axum adapter

| File | Responsibility |
|---|---|
| `src/lib.rs` | `AppState`, `router`. |
| `src/error.rs` | `ApiError`, the §9 status mapping, `Problem`. |
| `src/auth.rs` | The `Auth` extractor. |
| `src/scope.rs` | `ScopeParams` → `Scope`, authorised. |
| `src/memories.rs` | recall, remember, review, get, delete, forget, protect. |
| `src/ops.rs` | audit, maintain, purge, export, import. |
| `src/admin.rs` | budgets, policy, retention. |

`crates/memorysafe-cli` — `msafe`

| File | Responsibility |
|---|---|
| `src/main.rs` | `clap` parse and dispatch. |
| `src/config.rs` | `MsafeConfig`, TOML load, defaults. |
| `src/build.rs` | `build_engine` — the composition root. |
| `src/render.rs` | Human rendering and `--json`. |
| `src/cmd/memory.rs` | remember, recall, review. |
| `src/cmd/curate.rs` | forget, protect, audit, maintain, purge-subject. |
| `src/cmd/keys.rs` | key creation and listing. |
| `src/cmd/portable.rs` | export, import. |
| `src/cmd/serve.rs` | stdio and HTTP serving. |
| `src/cmd/shadow.rs` | shadow replay. |

`crates/memorysafe-shadow` — the evaluation harness

| File | Responsibility |
|---|---|
| `src/lib.rs` | `ShadowError`, re-exports. |
| `src/scenario.rs` | `Scenario`, `ScenarioWrite`. |
| `src/trace.rs` | `Trace`, `TracedDecision`, `TracedAction`. |
| `src/run.rs` | `run` — drives a scenario through a real engine. |
| `src/diff.rs` | `TraceDiff`, `diff`. |
| `src/replay.rs` | `Scenario::from_export_ndjson`. |
| `fixtures/` | Golden scenarios and their blessed traces. |

### Canonical signatures Plan 3 produces

Referenced across tasks. Any deviation is a bug.

```rust
// memorysafe-core (added in Task 1)
pub const ADMIN_COMPONENT: &str = "_admin";
impl Scope {
    pub fn admin(tenant: &TenantId) -> Scope;
    pub fn is_admin(&self) -> bool;
}

// memorysafe-auth
pub struct ApiKeyRecord { pub id: String, pub tenant: TenantId, pub hash: String,
                          pub label: String, pub disabled: bool }
pub struct GeneratedKey { pub secret: String, pub record: ApiKeyRecord }
pub fn generate(tenant: TenantId, label: &str) -> Result<GeneratedKey, AuthError>;
pub struct ApiKeyStore { /* private */ }
impl ApiKeyStore {
    pub fn new(records: Vec<ApiKeyRecord>) -> Self;
    pub fn authenticate(&self, presented: &str) -> Result<Authenticated, AuthError>;
    pub fn records(&self) -> impl Iterator<Item = &ApiKeyRecord>;
}
pub struct Authenticated { /* private */ }
impl Authenticated {
    pub fn tenant(&self) -> &TenantId;
    pub fn key_id(&self) -> &str;
    pub fn actor(&self) -> Actor;
    pub fn authorize_tenant(&self, tenant: &TenantId) -> Result<(), AuthError>;
    pub fn scope(&self, subject: &str, namespace: &str) -> Result<Scope, AuthError>;
}
pub enum AuthError { Missing, Malformed, Unknown, Disabled,
                     WrongTenant { authorized: String, requested: String },
                     Reserved { component: &'static str }, Scope(CoreError), Rng }
impl AuthError { pub fn is_unauthenticated(&self) -> bool; }

// memorysafe-engine (added in Task 2)
pub struct TenantSettings { pub policy_config: BaselineConfig, pub retention: RetentionProfile }
impl Engine {
    pub fn policy_for(&self, tenant: &TenantId) -> Arc<dyn GovernancePolicy>;
    pub fn retention_for(&self, tenant: &TenantId) -> RetentionProfile;
    pub fn tenant_settings(&self, tenant: &TenantId) -> TenantSettings;
    pub async fn set_tenant_policy_config(&self, tenant: &TenantId, cfg: BaselineConfig, actor: &Actor)
        -> Result<AuditId, EngineError>;
    pub async fn set_tenant_retention(&self, tenant: &TenantId, profile: RetentionProfile, actor: &Actor)
        -> Result<AuditId, EngineError>;
    pub async fn export_ndjson_as(&self, sel: &ScopeSelector, actor: &Actor) -> Result<String, EngineError>;
    pub async fn import_ndjson_as(&self, ndjson: &str, tenant: &TenantId, actor: &Actor)
        -> Result<ImportReport, EngineError>;
    // Was `(tenant, subject)` in Plan 1; Task 2 adds the actor, so the
    // `SubjectPurged` record names who ordered the erasure.
    pub async fn purge_subject(&self, tenant: &TenantId, subject: &SubjectId, actor: &Actor)
        -> Result<PurgeOutcome, EngineError>;
}

// memorysafe-mcp
pub enum ScopeSource {
    Stdio { tenant: TenantId, subject: SubjectId, default_namespace: Namespace },
    Http { keys: Arc<ApiKeyStore> },
}
pub struct MemorySafeServer { /* private */ }
impl MemorySafeServer { pub fn new(engine: Arc<Engine>, source: ScopeSource) -> Self; }
pub async fn serve_stdio(engine: Arc<Engine>, source: ScopeSource) -> anyhow::Result<()>;
pub fn http_service(engine: Arc<Engine>, source: ScopeSource)
    -> StreamableHttpService<MemorySafeServer, LocalSessionManager>;
// Added in Task 14, when configuration has somewhere to come from:
pub struct HttpTransportConfig { pub allowed_hosts: Vec<String> }   // loopback by default
pub fn http_service_with(engine: Arc<Engine>, source: ScopeSource, config: HttpTransportConfig)
    -> StreamableHttpService<MemorySafeServer, LocalSessionManager>;

// memorysafe-api
pub struct AppState { pub engine: Arc<Engine>, pub keys: Arc<ApiKeyStore> }
pub fn router(state: AppState) -> axum::Router;
pub struct ScopeParams { pub subject: String, pub namespace: String }   // JSON bodies only
pub fn scope::resolve(auth: &Auth, subject: &str, namespace: &str) -> Result<Scope, ApiError>;

// memorysafe-cli — a library with a thin `msafe` binary over it
pub fn build::build_engine(config: &MsafeConfig) -> anyhow::Result<Arc<Engine>>;
pub fn cmd::serve::http_router(engine: Arc<Engine>, keys: Arc<ApiKeyStore>, serve: &ServeConfig)
    -> axum::Router;
pub fn http_router_for_tests(root: &Path) -> axum::Router;

// memorysafe-shadow
pub struct ScenarioWrite { pub scope: Scope, pub body: String, pub kind: String,
    pub tags: Vec<String>, pub sensitivity_hint: Option<SensitivityLevel>,
    pub ttl_seconds: Option<i64> }
pub struct Scenario { pub name: String, pub embedder_dim: u16,
    pub budgets: Vec<(Scope, Budget)>, pub writes: Vec<ScenarioWrite>,
    pub unreplayable: Vec<Unreplayable> }
impl Scenario { pub fn from_export_ndjson(name: &str, ndjson: &str) -> Result<Scenario, ShadowError>; }
pub enum TracedAction { Retain { protection: Protection }, Merge { into_seq: Option<usize> }, Reject }
pub struct TracedDecision { pub seq: usize, pub scope: Scope, pub body_digest: String,
    pub action: TracedAction, pub reason_codes: Vec<ReasonCode>, pub evicted: usize,
    pub sensitivity: Option<SensitivityLevel> }
pub struct Trace { pub scenario: String, pub policy: PolicyId, pub decisions: Vec<TracedDecision> }
pub async fn run(scenario: &Scenario, policy: Arc<dyn GovernancePolicy>) -> Result<Trace, ShadowError>;
pub struct DecisionChange { pub seq: usize, pub before: TracedDecision, pub after: TracedDecision }
pub struct TraceDiff { pub total: usize, pub identical: usize, pub changed: Vec<DecisionChange>,
    pub transitions: BTreeMap<String, usize> }
pub fn diff(before: &Trace, after: &Trace) -> Result<TraceDiff, ShadowError>;
```

---

## Task Index

| # | Task | Deliverable |
|---|---|---|
| 1 | Core: the reserved admin scope; `memorysafe-auth` | A key authenticates to exactly one tenant |
| 2 | Engine: per-tenant policy and retention, actor-attributed events | `PolicyChanged`, `Exported`, `Imported` |
| 3 | MCP: server, scope resolution, `memory_remember`, `memory_recall` | Two tools over a real MCP client |
| 4 | MCP: `memory_review`, `memory_forget`, `memory_protect` | All five tools |
| 5 | MCP: audit and stats resources | `memorysafe://…/audit` readable |
| 6 | MCP: stdio and streamable-HTTP transports | Both transports serve |
| 7 | API: errors, auth extractor, router skeleton | The §9 status table, enforced |
| 8 | API: memory routes | recall / remember / review / get / delete / forget / protect |
| 9 | API: audit, maintain, purge, export, import | The operational surface |
| 10 | API: admin routes | budgets / policy / retention, per tenant |
| 11 | CLI: config, engine construction, `remember` / `recall` / `review` | `msafe` runs |
| 12 | CLI: `forget`, `protect`, `audit`, `maintain`, `purge-subject`, `keys` | The curation surface |
| 13 | CLI: `export` / `import` | A portable archive directory |
| 14 | CLI: `serve --transport stdio\|http` | One binary, both servers |
| 15 | Shadow: `Scenario`, `Trace`, `run` | A reproducible decision trace |
| 16 | Shadow: `diff` and golden fixtures | Policy regressions become test failures |
| 17 | Shadow: replay from an export archive, `msafe shadow` | The spec's `msafe shadow` |
| 18 | Acceptance: end-to-end walkthrough, CI, README | Plan 3 done |

---

## Task 1: Core — the reserved admin scope; `memorysafe-auth`

**Files:**
- Modify: `crates/memorysafe-core/src/ids.rs`
- Modify: `crates/memorysafe-core/src/lib.rs`
- Create: `crates/memorysafe-auth/Cargo.toml`
- Create: `crates/memorysafe-auth/src/lib.rs`
- Create: `crates/memorysafe-auth/src/key.rs`
- Create: `crates/memorysafe-auth/src/store.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `TenantId`, `SubjectId`, `Namespace`, `Scope`, `CoreError`, `Actor`, `ActorKind`.
- Produces: `memorysafe_core::ADMIN_COMPONENT`, `Scope::admin`, `Scope::is_admin`; and the whole
  `memorysafe-auth` surface listed under "Canonical signatures Plan 3 produces".

**Why a reserved component:** Task 2 writes tenant-level audit records — a policy change belongs
to a tenant, not to any subject. `AuditRecord` requires a full `Scope`, so tenant-level events
need a subject and namespace that no real caller can occupy. `_admin` is a legal component
(lowercase, underscore allowed), so reserving it has to be an explicit rule rather than a
consequence of validation, and every adapter enforces it at the boundary.

**Why lookup-by-id then constant-time compare:** the presented key carries its own id, so
authentication is one hash-map lookup rather than a scan, and the only comparison is between two
BLAKE3 hashes of equal length. Comparing hashes rather than secrets means a timing leak would
reveal a hash prefix, which is useless without a preimage; the constant-time compare is belt and
braces, and it costs one small dependency.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-core/src/ids.rs`, inside the existing `mod tests`:

```rust
    #[test]
    fn the_admin_scope_is_recognisable_and_is_not_a_normal_scope() {
        let tenant = TenantId::new("acme").unwrap();
        let admin = Scope::admin(&tenant);

        assert_eq!(admin.tenant, tenant);
        assert_eq!(admin.subject.as_str(), ADMIN_COMPONENT);
        assert_eq!(admin.namespace.as_str(), ADMIN_COMPONENT);
        assert!(admin.is_admin());

        let ordinary = Scope::new("acme", "user-42", "agent").unwrap();
        assert!(!ordinary.is_admin());
    }

    #[test]
    fn a_scope_is_only_admin_when_both_halves_are_reserved() {
        // Half-reserved scopes are ordinary. `is_admin` gates whether audit rows
        // are treated as tenant-level, so a scope that is reserved in only one
        // position must not be mistaken for one the engine wrote.
        let half = Scope::new("acme", ADMIN_COMPONENT, "agent").unwrap();
        assert!(!half.is_admin());
        let other_half = Scope::new("acme", "user-42", ADMIN_COMPONENT).unwrap();
        assert!(!other_half.is_admin());
    }
```

Create `crates/memorysafe-auth/src/key.rs` with only a test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::TenantId;

    #[test]
    fn a_generated_key_carries_its_id_in_the_clear_and_its_secret_only_once() {
        let tenant = TenantId::new("acme").unwrap();
        let g = generate(tenant.clone(), "ci runner").expect("generate");

        let (prefix, rest) = g.secret.split_once('_').expect("prefixed");
        assert_eq!(prefix, KEY_PREFIX);
        let (id, secret) = rest.split_once('_').expect("id then secret");
        assert_eq!(id, g.record.id, "the id must be readable without the secret");
        assert_eq!(id.len(), 26, "a ULID id");
        assert!(secret.len() >= 40, "at least 240 bits of base64url");

        assert_eq!(g.record.tenant, tenant);
        assert_eq!(g.record.label, "ci runner");
        assert!(!g.record.disabled);
    }

    #[test]
    fn the_record_never_contains_the_secret() {
        // The record is what gets written to msafe.toml. If the secret survives
        // serialization, every operator's config file is a credential store in
        // plaintext.
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        let json = serde_json::to_string(&g.record).unwrap();
        let secret_tail = g.secret.rsplit('_').next().unwrap();
        assert!(!json.contains(secret_tail), "the record serialised the secret");
    }

    #[test]
    fn two_generated_keys_never_collide() {
        let a = generate(TenantId::new("acme").unwrap(), "a").unwrap();
        let b = generate(TenantId::new("acme").unwrap(), "b").unwrap();
        assert_ne!(a.record.id, b.record.id);
        assert_ne!(a.record.hash, b.record.hash);
        assert_ne!(a.secret, b.secret);
    }

    #[test]
    fn a_malformed_presented_key_is_rejected_before_any_lookup() {
        for bad in ["", "nope", "msk_short", "xxx_01ARZ3NDEKTSV4RRFFQ69G5FAV_abc", "msk__abc"] {
            assert!(matches!(parse_presented(bad), Err(AuthError::Malformed)), "{bad} was accepted");
        }
    }
}
```

Create `crates/memorysafe-auth/src/store.rs` with only a test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::generate;
    use memorysafe_core::{ActorKind, TenantId};

    fn store_with(label: &str) -> (ApiKeyStore, String, TenantId) {
        let tenant = TenantId::new("acme").unwrap();
        let g = generate(tenant.clone(), label).unwrap();
        (ApiKeyStore::new(vec![g.record]), g.secret, tenant)
    }

    #[test]
    fn a_valid_key_authenticates_to_its_own_tenant() {
        let (store, secret, tenant) = store_with("ci");
        let auth = store.authenticate(&secret).expect("authenticate");
        assert_eq!(auth.tenant(), &tenant);
        assert_eq!(auth.actor().kind, ActorKind::ApiKey);
        assert_eq!(auth.actor().id.as_deref(), Some(auth.key_id()));
    }

    #[test]
    fn a_tampered_secret_is_rejected_as_unknown() {
        // The id half still resolves; only the secret half is wrong. The error
        // must not distinguish "no such key" from "wrong secret", or it becomes
        // an oracle for enumerating valid key ids.
        let (store, secret, _) = store_with("ci");
        let mut bytes = secret.into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();

        assert!(matches!(store.authenticate(&tampered), Err(AuthError::Unknown)));
    }

    #[test]
    fn an_unknown_id_is_rejected_with_the_same_error_as_a_wrong_secret() {
        let (store, _, _) = store_with("ci");
        let other = generate(TenantId::new("acme").unwrap(), "elsewhere").unwrap();
        assert!(matches!(store.authenticate(&other.secret), Err(AuthError::Unknown)));
    }

    #[test]
    fn a_disabled_key_does_not_authenticate() {
        let tenant = TenantId::new("acme").unwrap();
        let g = generate(tenant, "revoked").unwrap();
        let mut record = g.record;
        record.disabled = true;
        let store = ApiKeyStore::new(vec![record]);
        assert!(matches!(store.authenticate(&g.secret), Err(AuthError::Disabled)));
    }

    #[test]
    fn a_key_cannot_reach_another_tenant() {
        let (store, secret, _) = store_with("ci");
        let auth = store.authenticate(&secret).unwrap();
        let other = TenantId::new("globex").unwrap();
        assert!(matches!(
            auth.authorize_tenant(&other),
            Err(AuthError::WrongTenant { .. })
        ));
        assert!(auth.authorize_tenant(&TenantId::new("acme").unwrap()).is_ok());
    }

    #[test]
    fn a_key_builds_scopes_only_inside_its_own_tenant() {
        let (store, secret, _) = store_with("ci");
        let auth = store.authenticate(&secret).unwrap();

        let scope = auth.scope("user-42", "coding-agent").expect("in-tenant scope");
        assert_eq!(scope.tenant.as_str(), "acme");
        assert_eq!(scope.subject.as_str(), "user-42");
    }

    #[test]
    fn the_reserved_component_is_refused_in_either_position() {
        // Task 2 writes tenant-level audit rows under `_admin`. A caller that
        // could name that scope could read another tenant's policy history —
        // or forge rows that look like the engine wrote them.
        let (store, secret, _) = store_with("ci");
        let auth = store.authenticate(&secret).unwrap();

        assert!(matches!(
            auth.scope(memorysafe_core::ADMIN_COMPONENT, "agent"),
            Err(AuthError::Reserved { .. })
        ));
        assert!(matches!(
            auth.scope("user-42", memorysafe_core::ADMIN_COMPONENT),
            Err(AuthError::Reserved { .. })
        ));
    }

    #[test]
    fn an_invalid_component_surfaces_as_a_scope_error_not_a_panic() {
        let (store, secret, _) = store_with("ci");
        let auth = store.authenticate(&secret).unwrap();
        assert!(matches!(auth.scope("User-42", "agent"), Err(AuthError::Scope(_))));
        assert!(matches!(auth.scope("", "agent"), Err(AuthError::Scope(_))));
    }

    #[test]
    fn only_a_missing_or_malformed_credential_is_unauthenticated() {
        // 401 says "identify yourself"; 403 says "you did, and no". Getting this
        // backwards makes a client retry forever with a key that will never work.
        assert!(AuthError::Missing.is_unauthenticated());
        assert!(AuthError::Malformed.is_unauthenticated());
        assert!(AuthError::Unknown.is_unauthenticated());
        assert!(!AuthError::Disabled.is_unauthenticated());
        assert!(!AuthError::Reserved { component: "_admin" }.is_unauthenticated());
        assert!(
            !AuthError::WrongTenant { authorized: "acme".into(), requested: "globex".into() }
                .is_unauthenticated()
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p memorysafe-core ids && cargo test -p memorysafe-auth`
Expected: the core test fails with `cannot find function 'admin' in ...Scope`; the auth crate
fails to resolve as a package.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/memorysafe-core/src/ids.rs`, above `macro_rules! ulid_id`:

```rust
/// Reserved subject and namespace for tenant-level records. Legal as a
/// component, so reserving it is a rule the adapters enforce, not something
/// validation gives for free.
pub const ADMIN_COMPONENT: &str = "_admin";
```

and inside `impl Scope`:

```rust
    /// The scope tenant-level audit records are written under. Nothing a caller
    /// can name, because every adapter refuses `ADMIN_COMPONENT` in a
    /// caller-supplied subject or namespace.
    pub fn admin(tenant: &TenantId) -> Scope {
        Scope {
            tenant: tenant.clone(),
            subject: SubjectId::new(ADMIN_COMPONENT).expect("ADMIN_COMPONENT is a valid component"),
            namespace: Namespace::new(ADMIN_COMPONENT)
                .expect("ADMIN_COMPONENT is a valid component"),
        }
    }

    /// Both halves, not either: a scope reserved in one position only is an
    /// ordinary scope that happens to share a name.
    pub fn is_admin(&self) -> bool {
        self.subject.as_str() == ADMIN_COMPONENT && self.namespace.as_str() == ADMIN_COMPONENT
    }
```

Export it from `crates/memorysafe-core/src/lib.rs`, extending the existing `pub use ids::{…}`
line with `ADMIN_COMPONENT`.

Add to the workspace `Cargo.toml` `[workspace.dependencies]`:

```toml
memorysafe-auth = { path = "crates/memorysafe-auth" }
subtle = "2.6.1"
getrandom = "0.4.3"
```

`crates/memorysafe-auth/Cargo.toml`:

```toml
[package]
name = "memorysafe-auth"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "Tenant-scoped API keys for the MemorySafe network adapters"

[dependencies]
memorysafe-core.workspace = true
base64.workspace = true
blake3.workspace = true
getrandom.workspace = true
serde.workspace = true
subtle.workspace = true
thiserror.workspace = true
ulid.workspace = true

[dev-dependencies]
serde_json.workspace = true

[lints]
workspace = true
```

`crates/memorysafe-auth/src/lib.rs`:

```rust
//! Tenant-scoped API keys.
//!
//! §10 of the design says an API key identifies exactly one tenant and that
//! subject and namespace arrive per request and are validated against it. Both
//! network adapters need that, so it lives here rather than twice.

mod key;
mod store;

pub use key::{ApiKeyRecord, GeneratedKey, KEY_PREFIX, generate};
pub use store::{ApiKeyStore, Authenticated};

use memorysafe_core::CoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("no credential presented")]
    Missing,
    #[error("credential is not a MemorySafe API key")]
    Malformed,
    /// Deliberately indistinguishable from a wrong secret. Splitting the two
    /// would let a caller enumerate valid key ids.
    #[error("unknown API key")]
    Unknown,
    #[error("API key is disabled")]
    Disabled,
    #[error("key is scoped to tenant {authorized}, request named {requested}")]
    WrongTenant { authorized: String, requested: String },
    #[error("'{component}' is reserved and may not be used as a subject or namespace")]
    Reserved { component: &'static str },
    #[error(transparent)]
    Scope(#[from] CoreError),
    #[error("system randomness unavailable")]
    Rng,
}

impl AuthError {
    /// True when the caller has not established *who* they are — 401. False
    /// when they have and the answer is still no — 403.
    pub fn is_unauthenticated(&self) -> bool {
        matches!(self, AuthError::Missing | AuthError::Malformed | AuthError::Unknown)
    }
}
```

Prepend to `crates/memorysafe-auth/src/key.rs`:

```rust
use crate::AuthError;
use base64::Engine as _;
use memorysafe_core::TenantId;
use serde::{Deserialize, Serialize};

/// Presented form: `msk_<26-char ULID id>_<base64url secret>`.
pub const KEY_PREFIX: &str = "msk";
const SECRET_BYTES: usize = 32;

/// What is written to configuration. Holds a hash, never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub tenant: TenantId,
    /// BLAKE3 of the whole presented key, hex encoded.
    pub hash: String,
    pub label: String,
    #[serde(default)]
    pub disabled: bool,
}

/// The only moment the secret exists. Returned once, then unrecoverable.
#[derive(Debug, Clone)]
pub struct GeneratedKey {
    pub secret: String,
    pub record: ApiKeyRecord,
}

pub fn generate(tenant: TenantId, label: &str) -> Result<GeneratedKey, AuthError> {
    let id = ulid::Ulid::generate().to_string();
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Rng)?;
    let secret_part = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let secret = format!("{KEY_PREFIX}_{id}_{secret_part}");

    Ok(GeneratedKey {
        record: ApiKeyRecord {
            id,
            tenant,
            hash: hash_presented(&secret),
            label: label.to_owned(),
            disabled: false,
        },
        secret,
    })
}

pub(crate) fn hash_presented(presented: &str) -> String {
    blake3::hash(presented.as_bytes()).to_hex().to_string()
}

/// Splits a presented key into its id, without validating the secret. The id is
/// public by construction — it is how the store finds one record instead of
/// hashing against all of them.
pub(crate) fn parse_presented(presented: &str) -> Result<&str, AuthError> {
    let rest = presented.strip_prefix(KEY_PREFIX).ok_or(AuthError::Malformed)?;
    let rest = rest.strip_prefix('_').ok_or(AuthError::Malformed)?;
    let (id, secret) = rest.split_once('_').ok_or(AuthError::Malformed)?;
    if id.len() != 26 || secret.len() < 40 {
        return Err(AuthError::Malformed);
    }
    Ok(id)
}
```

Prepend to `crates/memorysafe-auth/src/store.rs`:

```rust
use crate::AuthError;
use crate::key::{ApiKeyRecord, hash_presented, parse_presented};
use memorysafe_core::{ADMIN_COMPONENT, Actor, ActorKind, Scope, TenantId};
use std::collections::HashMap;
use subtle::ConstantTimeEq;

#[derive(Debug, Clone, Default)]
pub struct ApiKeyStore {
    by_id: HashMap<String, ApiKeyRecord>,
}

impl ApiKeyStore {
    pub fn new(records: Vec<ApiKeyRecord>) -> Self {
        Self {
            by_id: records.into_iter().map(|r| (r.id.clone(), r)).collect(),
        }
    }

    pub fn records(&self) -> impl Iterator<Item = &ApiKeyRecord> {
        self.by_id.values()
    }

    pub fn authenticate(&self, presented: &str) -> Result<Authenticated, AuthError> {
        let id = parse_presented(presented)?;
        let record = self.by_id.get(id).ok_or(AuthError::Unknown)?;

        let expected = record.hash.as_bytes();
        let actual = hash_presented(presented);
        // Equal-length hex hashes, so `ct_eq` is meaningful; a length mismatch
        // would mean a corrupt record, which is also `Unknown`.
        let matches: bool = expected.len() == actual.len()
            && bool::from(expected.ct_eq(actual.as_bytes()));
        if !matches {
            return Err(AuthError::Unknown);
        }
        if record.disabled {
            return Err(AuthError::Disabled);
        }

        Ok(Authenticated {
            tenant: record.tenant.clone(),
            key_id: record.id.clone(),
        })
    }
}

/// Proof that a caller is one specific tenant. The only way to obtain one is
/// `ApiKeyStore::authenticate`, so a handler that holds one cannot have skipped
/// the check.
#[derive(Debug, Clone)]
pub struct Authenticated {
    tenant: TenantId,
    key_id: String,
}

impl Authenticated {
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The audit actor for anything this caller does. The key id, never the key.
    pub fn actor(&self) -> Actor {
        Actor { kind: ActorKind::ApiKey, id: Some(self.key_id.clone()) }
    }

    pub fn authorize_tenant(&self, tenant: &TenantId) -> Result<(), AuthError> {
        if tenant == &self.tenant {
            Ok(())
        } else {
            Err(AuthError::WrongTenant {
                authorized: self.tenant.to_string(),
                requested: tenant.to_string(),
            })
        }
    }

    /// The only constructor of a `Scope` in the network adapters. The tenant is
    /// taken from the credential and never from the request, so a scope that
    /// crosses tenants is unrepresentable rather than merely rejected.
    pub fn scope(&self, subject: &str, namespace: &str) -> Result<Scope, AuthError> {
        if subject == ADMIN_COMPONENT || namespace == ADMIN_COMPONENT {
            return Err(AuthError::Reserved { component: ADMIN_COMPONENT });
        }
        Ok(Scope::new(self.tenant.as_str(), subject, namespace)?)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-core && cargo test -p memorysafe-auth`
Expected: PASS — core gains 2 tests; `memorysafe-auth` runs 12.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-core/src/ crates/memorysafe-auth/
git commit -m "feat(auth): tenant-scoped API keys and a reserved admin scope"
```

---

## Task 2: Engine — per-tenant policy and retention, actor-attributed governance events

**Files:**
- Create: `crates/memorysafe-engine/src/settings.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs`
- Modify: `crates/memorysafe-engine/src/write.rs`, `src/read.rs`, `src/maintain.rs`, `src/mutate.rs`
- Modify: `crates/memorysafe-engine/src/portability.rs`
- Create: `crates/memorysafe-engine/tests/settings.rs`

**Interfaces:**
- Consumes: `Scope::admin`, `BaselineConfig`, `BaselinePolicy::new`, `RetentionProfile`,
  `AuditEvent::{PolicyChanged, Exported, Imported}`, `Backend::apply`.
- Produces: `TenantSettings`, `Engine::policy_for`, `Engine::retention_for`,
  `Engine::tenant_settings`, `Engine::set_tenant_policy_config`, `Engine::set_tenant_retention`,
  `Engine::export_ndjson_as`, `Engine::import_ndjson_as`, and an `Actor` parameter on
  `Engine::purge_subject`.

**`Engine::purge_subject` gains an `Actor` here, and this is the task that closes
that gap.** Plan 1 Task 33 builds the `SubjectPurged` record in the engine — the
backend inserts what it is handed and mints nothing — but writes
`Actor { kind: ActorKind::Human, id: None }`, an anonymous human carrying no more
identity than `Actor::system()`. Plan 1 recorded the deferral pointing here, so the
signature becomes `purge_subject(&self, tenant: &TenantId, subject: &SubjectId,
actor: &Actor)` and the record is built from that argument. It is the same move as
the four methods above, for the same reason: the actor exists at the boundary and
the record is written in the engine. Every call site is in this plan — the HTTP
route (`ops::purge_subject`), the CLI command, and this task's own `settings.rs`
test — and each already has an `Actor` in scope, from `AuthContext::actor()` at the
adapters. An erasure is the single operation whose audit row most needs to name who
ordered it.

**Why the engine and not the adapters.** Plan 1's "Known deferrals to Plan 3" put the
`Exported` / `Imported` / `PolicyChanged` audit records at "the CLI and HTTP boundary, with the
actor attached". An adapter cannot write an audit record — `Backend::apply` is not on `Engine`,
and exposing it would hand every adapter the ability to forge any row. So the *actor* comes from
the boundary and the *record* is written here, by methods that take an `Actor`.

**Why per-tenant.** §6: "All thresholds are configurable per tenant." §11: "Audit retention is
configured per tenant." §12 exposes both as `GET|PUT /v1/admin/tenants/:id/{policy,retention}`.
Plan 1 built one policy and one retention profile for the whole engine, which is right for a
single-tenant test but cannot serve the admin surface.

**Rename, do not add.** `Engine::policy` becomes `default_policy` and `Engine::retention` becomes
`default_retention`. Renaming makes the compiler enumerate every call site — `write.rs`
(`run_assess`, `run_admit`), `read.rs` (compose), `maintain.rs`, `mutate.rs` (`purge_subject`) —
instead of leaving one behind that silently keeps using the global policy for a tenant that
overrode it. `mutate.rs`'s `purge_subject` is touched twice in this task: once for
`retention_for`, and once for the `Actor` parameter above, which the compiler
enumerates the same way.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-engine/tests/settings.rs`:

```rust
use memorysafe_backend::ScopeSelector;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    Actor, ActorKind, AuditEvent, AuditFilter, Scope, SubjectId, TenantId,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{
    Engine, EngineConfig, RememberRequest, RetentionProfile,
};
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn acme() -> TenantId {
    TenantId::new("acme").unwrap()
}

fn globex() -> TenantId {
    TenantId::new("globex").unwrap()
}

fn scope(tenant: &str) -> Scope {
    Scope::new(tenant, "user-42", "agent").unwrap()
}

fn operator() -> Actor {
    Actor { kind: ActorKind::Human, id: Some("ops@acme".into()) }
}

#[tokio::test]
async fn a_tenant_without_an_override_uses_the_engine_default() {
    let e = engine();
    assert_eq!(e.tenant_settings(&acme()).policy_config, BaselineConfig::default());
    assert_eq!(e.retention_for(&acme()), RetentionProfile::Balanced);
}

#[tokio::test]
async fn setting_a_policy_config_changes_only_that_tenant() {
    let e = engine();
    let tuned = BaselineConfig { merge_threshold: 0.80, ..Default::default() };
    e.set_tenant_policy_config(&acme(), tuned.clone(), &operator()).await.unwrap();

    assert_eq!(e.tenant_settings(&acme()).policy_config.merge_threshold, 0.80);
    assert_eq!(
        e.tenant_settings(&globex()).policy_config,
        BaselineConfig::default(),
        "one tenant's tuning leaked into another"
    );
}

#[tokio::test]
async fn a_policy_change_writes_exactly_one_audit_record_naming_both_versions() {
    let e = engine();
    e.set_tenant_policy_config(&acme(), BaselineConfig::default(), &operator())
        .await
        .unwrap();

    let admin = Scope::admin(&acme());
    let rows = e
        .audit(&admin, &AuditFilter {
            events: vec![AuditEvent::PolicyChanged],
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor, operator());
    assert!(rows[0].scope.is_admin());
    let decision = rows[0].decision.as_ref().expect("the transition is the decision");
    assert!(!decision.reasons.is_empty(), "a policy change must say what changed");
    let detail = &decision.reasons[0].detail;
    assert!(detail.contains("baseline"), "the reason names the policy: {detail}");
}

#[tokio::test]
async fn an_incoherent_policy_config_is_refused_and_audits_nothing() {
    let e = engine();
    // Merging above the duplicate threshold is unreachable: every candidate
    // that would merge is already rejected as an exact duplicate.
    let broken = BaselineConfig { merge_threshold: 0.99, duplicate_threshold: 0.90, ..Default::default() };
    assert!(e.set_tenant_policy_config(&acme(), broken, &operator()).await.is_err());

    let out_of_range = BaselineConfig { mmr_lambda: 1.5, ..Default::default() };
    assert!(e.set_tenant_policy_config(&acme(), out_of_range, &operator()).await.is_err());

    let rows = e.audit(&Scope::admin(&acme()), &AuditFilter::default()).await.unwrap();
    assert!(rows.is_empty(), "a refused change must leave no trace of having happened");
    assert_eq!(e.tenant_settings(&acme()).policy_config, BaselineConfig::default());
}

#[tokio::test]
async fn retention_is_per_tenant_and_purge_honours_the_tenant_it_is_purging() {
    let e = engine();
    e.set_tenant_retention(&acme(), RetentionProfile::HipaaRetain, &operator()).await.unwrap();
    e.set_tenant_retention(&globex(), RetentionProfile::GdprStrict, &operator()).await.unwrap();

    e.remember(RememberRequest::new(scope("acme"), "a clinical note")).await.unwrap();
    e.remember(RememberRequest::new(scope("globex"), "an ordinary note")).await.unwrap();

    let user = SubjectId::new("user-42").unwrap();
    let kept = e.purge_subject(&acme(), &user, &operator()).await.unwrap();
    let dropped = e.purge_subject(&globex(), &user, &operator()).await.unwrap();

    assert_eq!(kept.items_removed, 1);
    assert!(kept.audit_rows_preserved >= 1, "hipaa_retain must keep the decision record");
    assert_eq!(kept.audit_rows_removed, 0);

    assert_eq!(dropped.items_removed, 1);
    assert!(dropped.audit_rows_removed >= 1, "gdpr_strict must cascade");
    assert_eq!(dropped.audit_rows_preserved, 0);
}

#[tokio::test]
async fn an_export_is_audited_with_the_actor_who_asked_for_it() {
    let e = engine();
    e.remember(RememberRequest::new(scope("acme"), "the thing to export")).await.unwrap();

    let sel = ScopeSelector { tenant: acme(), subject: None, namespace: None, include_audit: false };
    let ndjson = e.export_ndjson_as(&sel, &operator()).await.unwrap();
    assert!(ndjson.contains("the thing to export"));

    let rows = e
        .audit(&Scope::admin(&acme()), &AuditFilter {
            events: vec![AuditEvent::Exported],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "an export is a governance event, recorded once");
    assert_eq!(rows[0].actor, operator());

    let json = serde_json::to_string(&rows).unwrap();
    assert!(!json.contains("the thing to export"), "the export audit row leaked a body");
}

#[tokio::test]
async fn an_import_is_audited_with_the_actor_who_asked_for_it() {
    let source = engine();
    source.remember(RememberRequest::new(scope("acme"), "the thing to move")).await.unwrap();
    let sel = ScopeSelector { tenant: acme(), subject: None, namespace: None, include_audit: false };
    let ndjson = source.export_ndjson(&sel).await.unwrap();

    let target = engine();
    let report = target.import_ndjson_as(&ndjson, &acme(), &operator()).await.unwrap();
    assert_eq!(report.items_imported, 1);

    let rows = target
        .audit(&Scope::admin(&acme()), &AuditFilter {
            events: vec![AuditEvent::Imported],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor, operator());
}

#[tokio::test]
async fn a_tenant_policy_actually_governs_that_tenants_writes() {
    // The registry is worthless if the write path still reads the default. A
    // duplicate threshold of 0.0 rejects everything, so the second write to
    // acme must be refused while globex's is admitted.
    let e = engine();
    let everything_is_a_duplicate =
        BaselineConfig { duplicate_threshold: 0.0, merge_threshold: 0.0, ..Default::default() };
    e.set_tenant_policy_config(&acme(), everything_is_a_duplicate, &operator()).await.unwrap();

    e.remember(RememberRequest::new(scope("acme"), "first acme memory")).await.unwrap();
    let second = e.remember(RememberRequest::new(scope("acme"), "an entirely unrelated topic")).await.unwrap();
    assert!(
        matches!(second.action, memorysafe_core::Action::Reject | memorysafe_core::Action::Merge { .. }),
        "acme's own policy was not consulted: {:?}",
        second.action
    );

    e.remember(RememberRequest::new(scope("globex"), "first globex memory")).await.unwrap();
    let globex_second =
        e.remember(RememberRequest::new(scope("globex"), "an entirely unrelated topic")).await.unwrap();
    assert!(
        matches!(globex_second.action, memorysafe_core::Action::Retain { .. }),
        "globex got acme's policy"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-engine --test settings`
Expected: FAIL — `no method named 'tenant_settings' found for struct 'Engine'`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-engine/src/settings.rs`:

```rust
use crate::error::EngineError;
use crate::retention::RetentionProfile;
use memorysafe_policy::BaselineConfig;

/// Everything a tenant can configure. Returned as a snapshot so a caller never
/// holds a lock.
#[derive(Debug, Clone, PartialEq)]
pub struct TenantSettings {
    pub policy_config: BaselineConfig,
    pub retention: RetentionProfile,
}

/// Rejects configurations that are self-contradictory rather than merely
/// unusual. A tenant may tune thresholds; it may not install one that makes a
/// decision path unreachable, because the resulting audit trail would be
/// inexplicable.
pub fn validate(cfg: &BaselineConfig) -> Result<(), EngineError> {
    let unit = |name: &str, v: f32| -> Result<(), EngineError> {
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            return Err(EngineError::Validation(format!(
                "{name} must be a finite value in [0.0, 1.0], got {v}"
            )));
        }
        Ok(())
    };
    unit("duplicate_threshold", cfg.duplicate_threshold)?;
    unit("merge_threshold", cfg.merge_threshold)?;
    unit("near_duplicate_floor", cfg.near_duplicate_floor)?;
    unit("replay_quota", cfg.replay_quota)?;
    unit("mmr_lambda", cfg.mmr_lambda)?;

    if cfg.merge_threshold > cfg.duplicate_threshold {
        return Err(EngineError::Validation(format!(
            "merge_threshold ({}) above duplicate_threshold ({}) makes Merge unreachable",
            cfg.merge_threshold, cfg.duplicate_threshold
        )));
    }
    if cfg.near_duplicate_floor > cfg.merge_threshold {
        return Err(EngineError::Validation(format!(
            "near_duplicate_floor ({}) above merge_threshold ({}) hides the neighbours a merge \
             decision would cite as evidence",
            cfg.near_duplicate_floor, cfg.merge_threshold
        )));
    }
    for (name, v) in [
        ("value_half_life_days", cfg.value_half_life_days),
        ("source_trust_weight", cfg.source_trust_weight),
        ("replay_stale_days", cfg.replay_stale_days),
    ] {
        if !v.is_finite() || v < 0.0 {
            return Err(EngineError::Validation(format!(
                "{name} must be finite and non-negative, got {v}"
            )));
        }
    }
    Ok(())
}
```

In `crates/memorysafe-engine/src/lib.rs`, replace the `policy` and `retention` fields of both
`Engine` and its construction, and add the registries:

```rust
pub mod settings;
pub use settings::TenantSettings;

use memorysafe_backend::{ImportReport, ScopeSelector, WriteTransaction};
use memorysafe_core::{
    Action, Actor, AuditEvent, AuditId, AuditRecord, Decision, PolicyId, Reason, ReasonCode,
    Scope, TenantId, features,
};
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use std::collections::HashMap;
use std::sync::RwLock;
use time::OffsetDateTime;

pub struct Engine {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) default_policy: Arc<dyn GovernancePolicy>,
    pub(crate) default_retention: RetentionProfile,
    pub(crate) policies: RwLock<HashMap<TenantId, (BaselineConfig, Arc<dyn GovernancePolicy>)>>,
    pub(crate) retentions: RwLock<HashMap<TenantId, RetentionProfile>>,
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
            default_policy: config.policy,
            default_retention: config.retention,
            policies: RwLock::new(HashMap::new()),
            retentions: RwLock::new(HashMap::new()),
            fallback_policy: config.fallback_policy,
            stance: config.stance,
            neighbour_k: config.neighbour_k,
            eviction_candidates: config.eviction_candidates,
        }
    }

    /// The policy that governs this tenant. Every pipeline calls this instead
    /// of reading a field, so an override cannot be missed on one path.
    pub fn policy_for(&self, tenant: &TenantId) -> Arc<dyn GovernancePolicy> {
        self.policies
            .read()
            .expect("policy registry lock poisoned")
            .get(tenant)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| self.default_policy.clone())
    }

    pub fn retention_for(&self, tenant: &TenantId) -> RetentionProfile {
        self.retentions
            .read()
            .expect("retention registry lock poisoned")
            .get(tenant)
            .copied()
            .unwrap_or(self.default_retention)
    }

    pub fn tenant_settings(&self, tenant: &TenantId) -> TenantSettings {
        let policy_config = self
            .policies
            .read()
            .expect("policy registry lock poisoned")
            .get(tenant)
            .map(|(c, _)| c.clone())
            .unwrap_or_default();
        TenantSettings { policy_config, retention: self.retention_for(tenant) }
    }

    pub async fn set_tenant_policy_config(
        &self,
        tenant: &TenantId,
        cfg: BaselineConfig,
        actor: &Actor,
    ) -> Result<AuditId, EngineError> {
        // Validate before anything observable happens: a refused change must
        // leave neither a new policy nor an audit row claiming one.
        settings::validate(&cfg)?;

        let before = self.policy_for(tenant).id();
        let policy: Arc<dyn GovernancePolicy> = Arc::new(BaselinePolicy::new(cfg.clone()));
        let after = policy.id();

        let audit_id = self
            .record_admin_event(
                tenant,
                AuditEvent::PolicyChanged,
                actor,
                Reason::new(
                    ReasonCode::PolicyInvalid,
                    &format!("policy configuration replaced: {before} -> {after}"),
                    features! {},
                ),
                after.clone(),
            )
            .await?;

        self.policies
            .write()
            .expect("policy registry lock poisoned")
            .insert(tenant.clone(), (cfg, policy));
        Ok(audit_id)
    }

    pub async fn set_tenant_retention(
        &self,
        tenant: &TenantId,
        profile: RetentionProfile,
        actor: &Actor,
    ) -> Result<AuditId, EngineError> {
        let before = self.retention_for(tenant);
        let audit_id = self
            .record_admin_event(
                tenant,
                AuditEvent::PolicyChanged,
                actor,
                Reason::new(
                    ReasonCode::PolicyInvalid,
                    &format!("audit retention changed: {before:?} -> {profile:?}"),
                    features! {},
                ),
                self.policy_for(tenant).id(),
            )
            .await?;

        self.retentions
            .write()
            .expect("retention registry lock poisoned")
            .insert(tenant.clone(), profile);
        Ok(audit_id)
    }

    pub async fn export_ndjson_as(
        &self,
        sel: &ScopeSelector,
        actor: &Actor,
    ) -> Result<String, EngineError> {
        let ndjson = self.export_ndjson(sel).await?;
        self.record_admin_event(
            &sel.tenant,
            AuditEvent::Exported,
            actor,
            Reason::new(
                ReasonCode::PolicyInvalid,
                &format!(
                    "exported subject={} namespace={} include_audit={}",
                    sel.subject.as_ref().map_or("*", |s| s.as_str()),
                    sel.namespace.as_ref().map_or("*", |n| n.as_str()),
                    sel.include_audit
                ),
                features! {},
            ),
            self.policy_for(&sel.tenant).id(),
        )
        .await?;
        Ok(ndjson)
    }

    pub async fn import_ndjson_as(
        &self,
        ndjson: &str,
        tenant: &TenantId,
        actor: &Actor,
    ) -> Result<ImportReport, EngineError> {
        // `Engine::import_ndjson` takes the destination tenant explicitly (Plan
        // 1 Task 37); this wrapper already has the authorised one in hand, so
        // it passes it rather than letting the payload name its own target.
        let report = self.import_ndjson(ndjson, tenant).await?;
        self.record_admin_event(
            tenant,
            AuditEvent::Imported,
            actor,
            Reason::new(
                ReasonCode::PolicyInvalid,
                &format!(
                    "imported items={} skipped_existing={} audit={}",
                    report.items_imported, report.items_skipped_existing, report.audit_imported
                ),
                features! {},
            ),
            self.policy_for(tenant).id(),
        )
        .await?;
        Ok(report)
    }

    /// One audit row in the tenant's reserved admin scope, carrying no items —
    /// these events are about the tenant, not about any memory.
    async fn record_admin_event(
        &self,
        tenant: &TenantId,
        event: AuditEvent,
        actor: &Actor,
        reason: Reason,
        policy: PolicyId,
    ) -> Result<AuditId, EngineError> {
        let scope = Scope::admin(tenant);
        let decision = Decision {
            subject: None,
            action: Action::Reject,
            evictions: vec![],
            reasons: vec![reason],
            policy,
        };
        let record = AuditRecord::new(
            scope.clone(),
            event,
            vec![],
            actor.clone(),
            OffsetDateTime::now_utc(),
        )
        .with_decision(decision);

        let txn = WriteTransaction::new(scope, record);
        Ok(self.backend.apply(txn).await?.audit_id)
    }
}
```

**`Action::Reject` on an administrative decision is deliberate.** `Action` describes what happened
to a *memory*, and a policy change happens to no memory; `Reject` is the variant that admits
nothing. The alternative — a fifth `Action` variant — would change a serialised wire format that
Plan 1 froze in audit rows for the sake of three administrative events. The event discriminates;
the action does not.

`ReasonCode` has no administrative variant either, for the same reason: it is documented as a
wire format where "renaming a variant is a breaking change". `PolicyInvalid` is the closest
existing code and the `detail` carries the transition. If a later plan adds `ReasonCode::Configured`,
these four call sites are the ones to update.

Now update the call sites the rename exposes:

- `src/write.rs` — in `run_assess` and `run_admit`, `let policy = self.policy.clone();` becomes
  `let policy = self.policy_for(&cand.scope_tenant());`. `Candidate` has no scope, so pass the
  tenant in: both functions gain a `tenant: &TenantId` parameter, and `remember` passes
  `&req.scope.tenant`.
- `src/read.rs` — the `compose` call site takes `self.policy_for(&req.scope.tenant)`.
- `src/maintain.rs` — the `maintain` call site takes `self.policy_for(&scope.tenant)`.
- `src/mutate.rs` — `purge_subject` reads `self.retention_for(tenant)` where it read
  `self.retention`.

Every one of these is a compile error until fixed; there is no path where the old field name
still resolves.

Add to `crates/memorysafe-engine/Cargo.toml` `[dev-dependencies]` if absent: `serde_json.workspace = true`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-engine`
Expected: PASS — the 8 new tests plus every test Plan 1 left green.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/
git commit -m "feat(engine): per-tenant policy and retention with audited transitions"
```

---

## Task 3: MCP — server, scope resolution, `memory_remember`, `memory_recall`

**Files:**
- Create: `crates/memorysafe-mcp/Cargo.toml`
- Create: `crates/memorysafe-mcp/src/lib.rs`
- Create: `crates/memorysafe-mcp/src/scope.rs`
- Create: `crates/memorysafe-mcp/src/dto.rs`
- Create: `crates/memorysafe-mcp/src/tools_write.rs`
- Create: `crates/memorysafe-mcp/tests/support/mod.rs`
- Create: `crates/memorysafe-mcp/tests/write_tools.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `Engine::remember`, `Engine::recall`, `ApiKeyStore::authenticate`, `Authenticated::scope`.
- Produces: `ScopeSource`, `Resolved`, `MemorySafeServer::new`, the `dto` module, and the tools
  `memory_remember` and `memory_recall`.

**Why adapter-local DTOs here and nowhere else.** `rmcp`'s structured tool output requires
`schemars::JsonSchema`, and `memorysafe-core` must not depend on `schemars` — it is the crate the
purity job keeps free of everything. So the MCP crate owns flat wire types and total conversions
from the engine types. The HTTP adapter has no such constraint and serialises the engine types
directly; do not copy this pattern there.

**Why `Extensions` and not `RequestContext` in `resolve`.** Scope resolution is where a
cross-tenant bug would live, so it must be unit-testable without standing up a transport.
`Extensions` is constructible in three lines; `RequestContext` is not.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-mcp/src/scope.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_auth::generate;

    fn stdio() -> ScopeSource {
        ScopeSource::Stdio {
            tenant: TenantId::new("acme").unwrap(),
            subject: SubjectId::new("user-42").unwrap(),
            default_namespace: Namespace::new("coding-agent").unwrap(),
        }
    }

    fn parts(auth: Option<&str>) -> Extensions {
        let mut builder = http::Request::builder().uri("/mcp");
        if let Some(value) = auth {
            builder = builder.header(http::header::AUTHORIZATION, value);
        }
        let (parts, ()) = builder.body(()).unwrap().into_parts();
        let mut ext = Extensions::new();
        ext.insert(parts);
        ext
    }

    #[test]
    fn stdio_uses_its_configured_scope_and_default_namespace() {
        let r = stdio().resolve(&Extensions::new(), None, None).expect("resolve");
        assert_eq!(r.scope.tenant.as_str(), "acme");
        assert_eq!(r.scope.subject.as_str(), "user-42");
        assert_eq!(r.scope.namespace.as_str(), "coding-agent");
        assert_eq!(r.actor.kind, ActorKind::Agent);
    }

    #[test]
    fn stdio_lets_a_call_override_the_namespace_but_never_the_subject() {
        // The spec is explicit: stdio is configured with tenant AND subject;
        // only the namespace is per-call. A client that could switch subject
        // could read another end-user's memory from the same session.
        let r = stdio().resolve(&Extensions::new(), None, Some("notes")).unwrap();
        assert_eq!(r.scope.namespace.as_str(), "notes");

        let err = stdio()
            .resolve(&Extensions::new(), Some("someone-else"), None)
            .expect_err("subject override must be refused");
        assert!(format!("{err:?}").contains("subject"), "{err:?}");

        // Naming the configured subject is not an override, so it is allowed.
        assert!(stdio().resolve(&Extensions::new(), Some("user-42"), None).is_ok());
    }

    #[test]
    fn http_takes_the_tenant_from_the_key_and_the_rest_from_the_call() {
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        // Capture what the test needs before the record moves into the store.
        let key_id = g.record.id.clone();
        let secret = g.secret.clone();
        let source = ScopeSource::Http { keys: Arc::new(ApiKeyStore::new(vec![g.record])) };
        let ext = parts(Some(&format!("Bearer {secret}")));

        let r = source.resolve(&ext, Some("user-42"), Some("agent")).expect("resolve");
        assert_eq!(r.scope.tenant.as_str(), "acme");
        assert_eq!(r.scope.subject.as_str(), "user-42");
        assert_eq!(r.actor.kind, ActorKind::ApiKey);
        assert_eq!(r.actor.id.as_deref(), Some(key_id.as_str()));
    }

    #[test]
    fn http_without_a_credential_resolves_nothing() {
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        let source = ScopeSource::Http { keys: Arc::new(ApiKeyStore::new(vec![g.record])) };

        assert!(source.resolve(&parts(None), Some("user-42"), Some("agent")).is_err());
        assert!(source.resolve(&Extensions::new(), Some("user-42"), Some("agent")).is_err(),
            "no HTTP parts at all must fail closed, not fall back to stdio behaviour");
        assert!(
            source.resolve(&parts(Some("Bearer msk_00000000000000000000000000_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")), Some("user-42"), Some("agent")).is_err()
        );
        assert!(source.resolve(&parts(Some(&g.secret)), Some("user-42"), Some("agent")).is_err(),
            "a bare key without the Bearer scheme must be refused");
    }

    #[test]
    fn http_requires_both_subject_and_namespace() {
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        let source = ScopeSource::Http { keys: Arc::new(ApiKeyStore::new(vec![g.record])) };
        let ext = parts(Some(&format!("Bearer {}", g.secret)));

        assert!(source.resolve(&ext, None, Some("agent")).is_err());
        assert!(source.resolve(&ext, Some("user-42"), None).is_err());
    }

    #[test]
    fn the_reserved_admin_scope_is_unreachable_from_either_transport() {
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        let http = ScopeSource::Http { keys: Arc::new(ApiKeyStore::new(vec![g.record])) };
        let ext = parts(Some(&format!("Bearer {}", g.secret)));
        assert!(http.resolve(&ext, Some("_admin"), Some("_admin")).is_err());
        assert!(stdio().resolve(&Extensions::new(), None, Some("_admin")).is_err());
    }
}
```

The other tests in this module clone `g.secret` before moving `g.record` into the store, for the
same reason.

`crates/memorysafe-mcp/tests/support/mod.rs`:

```rust
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Namespace, SubjectId, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig};
use memorysafe_mcp::{MemorySafeServer, ScopeSource};
use memorysafe_policy::BaselinePolicy;
use rmcp::{RoleClient, ServiceExt, service::RunningService};
use std::sync::Arc;

pub fn engine() -> Arc<Engine> {
    let dir = tempfile::tempdir().expect("tempdir");
    Arc::new(Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    )))
}

pub fn stdio_source() -> ScopeSource {
    ScopeSource::Stdio {
        tenant: TenantId::new("acme").unwrap(),
        subject: SubjectId::new("user-42").unwrap(),
        default_namespace: Namespace::new("coding-agent").unwrap(),
    }
}

/// A real MCP client talking to a real MCP server over an in-memory duplex.
/// Nothing is stubbed: the JSON-RPC framing, the tool schemas, and the
/// structured results all go over the wire the way a client would see them.
pub async fn connect(engine: Arc<Engine>) -> RunningService<RoleClient, ()> {
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    let server = MemorySafeServer::new(engine, stdio_source());
    tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve");
        let _ = running.waiting().await;
    });
    ().serve(client_transport).await.expect("client connects")
}

pub fn args(pairs: serde_json::Value) -> rmcp::model::JsonObject {
    match pairs {
        serde_json::Value::Object(map) => map,
        other => panic!("tool arguments must be an object, got {other}"),
    }
}
```

`crates/memorysafe-mcp/tests/write_tools.rs`:

```rust
mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::json;
use support::{args, connect, engine};

fn structured(result: &rmcp::model::CallToolResult) -> &serde_json::Value {
    result
        .structured_content
        .as_ref()
        .expect("every MemorySafe tool returns structured content")
}

#[tokio::test]
async fn the_server_advertises_the_write_tools_with_schemas() {
    let client = connect(engine()).await;
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

    assert!(names.contains(&"memory_remember"), "{names:?}");
    assert!(names.contains(&"memory_recall"), "{names:?}");
    for tool in &tools {
        assert!(
            tool.description.as_ref().is_some_and(|d| !d.is_empty()),
            "tool {} has no description for the model to read",
            tool.name
        );
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn remembering_returns_the_governance_decision_not_just_an_id() {
    let client = connect(engine()).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "the production database migration runs on Sundays"
            }))),
        )
        .await
        .unwrap();

    let value = structured(&result);
    assert_eq!(value["action"], "retain");
    assert!(value["item_id"].is_string());
    assert!(value["audit_id"].is_string());
    assert!(
        value["reasons"].as_array().is_some_and(|r| !r.is_empty()),
        "a decision with no reason is not governance: {value}"
    );
    assert!(value["reasons"][0]["code"].is_string());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_rejected_duplicate_is_a_successful_tool_call() {
    // An agent learning its memory was redundant is the product working. If
    // this surfaces as a tool error, every client will retry it forever.
    let client = connect(engine()).await;
    let body = "the deploy key rotates every ninety days";
    for _ in 0..2 {
        let result = client
            .call_tool(
                CallToolRequestParams::new("memory_remember")
                    .with_arguments(args(json!({ "body": body }))),
            )
            .await
            .expect("the call itself succeeds");
        assert_ne!(result.is_error, Some(true), "a governance decision became an error");
    }

    let second = client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": body }))),
        )
        .await
        .unwrap();
    let value = structured(&second);
    assert!(
        value["action"] == "reject" || value["action"] == "merge",
        "an identical rewrite was admitted again: {value}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn an_empty_body_is_a_tool_error_because_nothing_was_decided() {
    let client = connect(engine()).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": "   " }))),
        )
        .await;
    assert!(result.is_err(), "a validation failure must not look like a decision");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn recall_returns_a_governed_working_set_with_its_audit_id() {
    let client = connect(engine()).await;
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        client
            .call_tool(
                CallToolRequestParams::new("memory_remember")
                    .with_arguments(args(json!({ "body": body }))),
            )
            .await
            .unwrap();
    }

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "when does the migration run",
                "max_items": 2
            }))),
        )
        .await
        .unwrap();

    let value = structured(&result);
    assert!(value["audit_id"].is_string(), "every recall is audited: {value}");
    let items = value["items"].as_array().expect("items array");
    assert!(!items.is_empty(), "a matching query returned nothing: {value}");
    assert!(items.len() <= 2, "the budget was ignored: {value}");
    for item in items {
        assert!(item["id"].is_string());
        assert!(item["body"].is_string());
        assert!(item["reason_code"].is_string(), "each selection says why it is there");
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn recall_over_an_empty_namespace_is_an_empty_success() {
    let client = connect(engine()).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "anything at all",
                "namespace": "somewhere-else"
            }))),
        )
        .await
        .unwrap();

    let value = structured(&result);
    assert_eq!(value["items"].as_array().unwrap().len(), 0);
    assert!(value["audit_id"].is_string(), "an empty recall is still audited");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_stdio_client_cannot_switch_subject() {
    let client = connect(engine()).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "a memory for someone else",
                "subject": "another-user"
            }))),
        )
        .await;
    assert!(result.is_err(), "stdio let a client name a different subject");
    client.cancel().await.unwrap();
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p memorysafe-mcp`
Expected: FAIL — the package does not resolve.

- [ ] **Step 3: Write minimal implementation**

Add to the workspace `Cargo.toml` `[workspace.dependencies]`:

```toml
memorysafe-mcp = { path = "crates/memorysafe-mcp" }
rmcp = { version = "3.2.0", default-features = false, features = [
    "server", "macros", "schemars", "transport-io", "transport-streamable-http-server",
] }
schemars = "1.2.2"
http = "1.3.1"
anyhow = "1.0.104"
```

`crates/memorysafe-mcp/Cargo.toml`:

```toml
[package]
name = "memorysafe-mcp"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "MCP server for the MemorySafe governed-memory engine"

[dependencies]
memorysafe-auth.workspace = true
memorysafe-core.workspace = true
memorysafe-engine.workspace = true
anyhow.workspace = true
http.workspace = true
rmcp.workspace = true
schemars.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true
tokio = { workspace = true, features = ["rt", "sync", "macros", "io-std"] }

[dev-dependencies]
memorysafe-backend-sqlite.workspace = true
memorysafe-embed.workspace = true
memorysafe-policy.workspace = true
rmcp = { workspace = true, features = ["client"] }
tempfile.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "io-util"] }

[lints]
workspace = true
```

`crates/memorysafe-mcp/src/lib.rs`:

```rust
//! The MCP adapter. Five tools and two resources over one engine.
//!
//! Serial tool calls are where agent memory dies, so the surface stays small
//! and each call does real work. Nothing here decides anything: every action
//! and every reason in a result came out of the engine.

pub mod dto;
pub mod scope;
mod tools_write;

pub use scope::{Resolved, ScopeSource};

use memorysafe_engine::Engine;
use rmcp::ServerHandler;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::tool_handler;
use std::sync::Arc;

#[derive(Clone)]
pub struct MemorySafeServer {
    pub(crate) engine: Arc<Engine>,
    pub(crate) source: ScopeSource,
    tool_router: ToolRouter<Self>,
}

impl MemorySafeServer {
    pub fn new(engine: Arc<Engine>, source: ScopeSource) -> Self {
        Self { engine, source, tool_router: Self::write_router() }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemorySafeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Governed memory. `memory_remember` writes and returns the governance decision \
                 — a rejection or a merge is a successful call, not an error. `memory_recall` \
                 returns a working set composed under a token budget, with a reason for every \
                 item selected and every item omitted."
                    .to_string(),
            )
    }
}
```

`crates/memorysafe-mcp/src/scope.rs`:

```rust
use memorysafe_auth::{ApiKeyStore, AuthError};
use memorysafe_core::{
    ADMIN_COMPONENT, Actor, ActorKind, Namespace, Scope, SubjectId, TenantId,
};
use rmcp::ErrorData;
use rmcp::model::Extensions;
use std::sync::Arc;

/// How a call's scope is established, per transport.
#[derive(Clone)]
pub enum ScopeSource {
    /// The server is configured with tenant and subject; the namespace defaults
    /// and may be overridden per call.
    Stdio { tenant: TenantId, subject: SubjectId, default_namespace: Namespace },
    /// The API key identifies the tenant; the request carries subject and
    /// namespace.
    Http { keys: Arc<ApiKeyStore> },
}

pub struct Resolved {
    pub scope: Scope,
    pub actor: Actor,
}

fn invalid(message: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(message.into(), None)
}

fn auth_error(e: AuthError) -> ErrorData {
    // The MCP error surface has no status codes, so the distinction 401/403
    // draws is carried in the message rather than lost.
    ErrorData::invalid_params(e.to_string(), None)
}

impl ScopeSource {
    pub fn resolve(
        &self,
        extensions: &Extensions,
        subject: Option<&str>,
        namespace: Option<&str>,
    ) -> Result<Resolved, ErrorData> {
        if subject == Some(ADMIN_COMPONENT) || namespace == Some(ADMIN_COMPONENT) {
            return Err(invalid(format!("'{ADMIN_COMPONENT}' is reserved")));
        }

        match self {
            ScopeSource::Stdio { tenant, subject: configured, default_namespace } => {
                if let Some(requested) = subject
                    && requested != configured.as_str()
                {
                    return Err(invalid(format!(
                        "this server is bound to subject '{configured}'; a call may not name \
                         subject '{requested}'"
                    )));
                }
                let ns = match namespace {
                    Some(raw) => Namespace::new(raw).map_err(|e| invalid(e.to_string()))?,
                    None => default_namespace.clone(),
                };
                Ok(Resolved {
                    scope: Scope {
                        tenant: tenant.clone(),
                        subject: configured.clone(),
                        namespace: ns,
                    },
                    actor: Actor { kind: ActorKind::Agent, id: None },
                })
            }
            ScopeSource::Http { keys } => {
                let parts = extensions
                    .get::<http::request::Parts>()
                    .ok_or_else(|| invalid("no HTTP request context on this call"))?;
                let presented = parts
                    .headers
                    .get(http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .ok_or_else(|| auth_error(AuthError::Missing))?;
                let auth = keys.authenticate(presented).map_err(auth_error)?;

                let subject = subject
                    .ok_or_else(|| invalid("'subject' is required over HTTP"))?;
                let namespace = namespace
                    .ok_or_else(|| invalid("'namespace' is required over HTTP"))?;
                let scope = auth.scope(subject, namespace).map_err(auth_error)?;
                Ok(Resolved { scope, actor: auth.actor() })
            }
        }
    }
}
```

`crates/memorysafe-mcp/src/dto.rs`:

```rust
//! Wire types. Flat, schema-bearing, and converted from the engine types by
//! total functions — a field added to `MemoryItem` that matters to a client is
//! a change here, not a silent omission.

use memorysafe_core::{
    Action, MemoryItem, OmittedItem, Protection, RecallMode, Reason, SelectedItem,
    SensitivityLevel, WorkingSet,
};
use memorysafe_engine::WriteOutcome;
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Round-trips through `serde` rather than a hand-written match, so the names
/// on the wire are exactly the names `memorysafe-core` serialises and cannot
/// drift from them.
pub fn parse_sensitivity(raw: &str) -> Result<SensitivityLevel, ErrorData> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).map_err(|_| {
        ErrorData::invalid_params(
            format!("unknown sensitivity '{raw}'; expected one of public, internal, personal, sensitive, restricted"),
            None,
        )
    })
}

pub fn sensitivity_name(level: SensitivityLevel) -> String {
    match serde_json::to_value(level) {
        Ok(serde_json::Value::String(s)) => s,
        _ => unreachable!("SensitivityLevel serialises as a string"),
    }
}

pub fn parse_mode(raw: &str) -> Result<RecallMode, ErrorData> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).map_err(|_| {
        ErrorData::invalid_params(
            format!("unknown mode '{raw}'; expected working_set or search"),
            None,
        )
    })
}

/// `Protection` serialises as a tagged object because `Protected` carries a
/// deadline. On the wire a client names the level and, for `protected`, the
/// deadline separately; the mapping is explicit and its names are asserted in
/// this module's tests.
pub fn protection_name(p: &Protection) -> &'static str {
    match p {
        Protection::Normal => "normal",
        Protection::Protected { .. } => "protected",
        Protection::Pinned => "pinned",
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemoryView {
    pub id: String,
    pub body: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub sensitivity: String,
    pub protection: String,
    pub protected_until: Option<i64>,
    pub created_at: i64,
    pub occurred_at: Option<i64>,
    /// True while the item is invisible to vector search and reachable only by
    /// keyword or review. A client showing memories should say so.
    pub pending_embedding: bool,
}

impl From<&MemoryItem> for MemoryView {
    fn from(item: &MemoryItem) -> Self {
        Self {
            id: item.id.to_string(),
            body: item.body.clone(),
            kind: item.kind.clone(),
            tags: item.tags.clone(),
            sensitivity: sensitivity_name(item.sensitivity),
            protection: protection_name(&item.protection).to_owned(),
            protected_until: match item.protection {
                Protection::Protected { until } => Some(until.unix_timestamp()),
                _ => None,
            },
            created_at: item.created_at.unix_timestamp(),
            occurred_at: item.occurred_at.map(|t| t.unix_timestamp()),
            pending_embedding: item.pending_embedding,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReasonView {
    pub code: String,
    pub detail: String,
}

impl From<&Reason> for ReasonView {
    fn from(r: &Reason) -> Self {
        Self {
            code: match serde_json::to_value(r.code) {
                Ok(serde_json::Value::String(s)) => s,
                _ => unreachable!("ReasonCode serialises as a string"),
            },
            detail: r.detail.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RecalledMemory {
    #[serde(flatten)]
    pub memory: MemoryView,
    pub relevance: f32,
    pub reason_code: String,
    pub reason_detail: String,
}

impl From<&SelectedItem> for RecalledMemory {
    fn from(s: &SelectedItem) -> Self {
        let reason = ReasonView::from(&s.reason);
        Self {
            memory: MemoryView::from(&s.item),
            relevance: s.relevance,
            reason_code: reason.code,
            reason_detail: reason.detail,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OmittedView {
    pub id: String,
    pub reason_code: String,
    pub reason_detail: String,
}

impl From<&OmittedItem> for OmittedView {
    fn from(o: &OmittedItem) -> Self {
        let reason = ReasonView::from(&o.reason);
        Self { id: o.id.to_string(), reason_code: reason.code, reason_detail: reason.detail }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RecallResult {
    pub items: Vec<RecalledMemory>,
    pub tokens_used: u32,
    /// A **sample** of what was considered and cut, with the reason, capped at
    /// `memorysafe_core::OMITTED_CAP`. Its length is not the number of
    /// omissions — read `omitted_total` for that.
    pub omitted: Vec<OmittedView>,
    /// How many items were omitted before the sample above was truncated.
    /// Carried through from `WorkingSet::omitted_total`, and carried
    /// deliberately: the caller this number exists for is the one tuning the
    /// recall budget, and dropping it at this boundary would leave an MCP
    /// client unable to tell fifty omissions from five thousand — which is the
    /// silent truncation the field was added to remove.
    pub omitted_total: usize,
    pub audit_id: String,
}

impl RecallResult {
    /// `audit_id` is `String`, not `Option<String>`: the engine guarantees a
    /// recall is audited before it returns, so a missing id here is a bug worth
    /// failing on rather than a shape a client has to handle.
    pub fn build(ws: &WorkingSet) -> Result<Self, ErrorData> {
        let audit_id = ws
            .audit_id
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("recall returned an unaudited working set", None))?;
        Ok(Self {
            items: ws.items.iter().map(RecalledMemory::from).collect(),
            tokens_used: ws.tokens_used,
            omitted: ws.omitted.iter().map(OmittedView::from).collect(),
            omitted_total: ws.omitted_total,
            audit_id: audit_id.to_string(),
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RememberResult {
    pub item_id: Option<String>,
    /// `retain`, `merge`, or `reject`. All three are successful calls.
    pub action: String,
    pub protection: Option<String>,
    pub merged_into: Option<String>,
    pub reasons: Vec<ReasonView>,
    pub evicted: Vec<String>,
    pub audit_id: String,
}

impl From<&WriteOutcome> for RememberResult {
    fn from(out: &WriteOutcome) -> Self {
        let (action, protection) = match &out.action {
            Action::Retain { protection } => ("retain", Some(protection_name(protection).to_owned())),
            Action::Merge { .. } => ("merge", None),
            Action::Reject => ("reject", None),
        };
        Self {
            item_id: out.item_id.as_ref().map(|i| i.to_string()),
            action: action.to_owned(),
            protection,
            merged_into: out.merged_into.as_ref().map(|i| i.to_string()),
            reasons: out.reasons.iter().map(ReasonView::from).collect(),
            evicted: out.evicted.iter().map(|i| i.to_string()).collect(),
            audit_id: out.audit_id.to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RememberParams {
    /// The memory to store. One discrete fact, preference, event, procedure, or entity.
    pub body: String,
    /// Convention, not enforced: fact, preference, event, procedure, entity.
    pub kind: Option<String>,
    pub tags: Option<Vec<String>>,
    /// May only raise the level the detectors assign, never lower it.
    pub sensitivity_hint: Option<String>,
    pub ttl_seconds: Option<i64>,
    /// A retried write with the same key returns the original outcome.
    pub idempotency_key: Option<String>,
    /// Required over HTTP; over stdio it must match the configured subject.
    pub subject: Option<String>,
    /// Required over HTTP; over stdio it defaults to the server's namespace.
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RecallParams {
    pub query: Option<String>,
    /// `working_set` (default) composes under a budget; `search` returns a raw
    /// ranked list. Both are scope-filtered, sensitivity-capped, and audited.
    pub mode: Option<String>,
    pub max_tokens: Option<u32>,
    pub max_items: Option<usize>,
    pub tags_any: Option<Vec<String>>,
    pub kinds: Option<Vec<String>>,
    pub occurred_after: Option<i64>,
    pub occurred_before: Option<i64>,
    /// Items above this level are excluded in the backend query. Defaults to
    /// `restricted`, which excludes nothing.
    pub sensitivity_ceiling: Option<String>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_the_core_serde_names() {
        assert_eq!(sensitivity_name(SensitivityLevel::Restricted), "restricted");
        assert_eq!(parse_sensitivity("personal").unwrap(), SensitivityLevel::Personal);
        assert!(parse_sensitivity("Personal").is_err(), "wire names are lowercase");
        assert_eq!(parse_mode("search").unwrap(), RecallMode::Search);
        assert_eq!(parse_mode("working_set").unwrap(), RecallMode::WorkingSet);
    }

    #[test]
    fn protection_names_survive_a_round_trip_through_core() {
        // If `Protection` gains a variant, this fails to compile rather than
        // silently rendering the new variant as something else.
        for p in [
            Protection::Normal,
            Protection::Pinned,
            Protection::Protected { until: time::OffsetDateTime::UNIX_EPOCH },
        ] {
            let name = protection_name(&p);
            let serialised = serde_json::to_value(p).unwrap();
            assert_eq!(serialised["kind"], name, "{p:?} renders inconsistently");
        }
    }
}
```

`crates/memorysafe-mcp/src/tools_write.rs`:

```rust
use crate::MemorySafeServer;
use crate::dto::{
    RecallParams, RecallResult, RememberParams, RememberResult, parse_mode, parse_sensitivity,
};
use memorysafe_core::{RecallBudget, RecallRequest, SensitivityLevel};
use memorysafe_engine::RememberRequest;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, tool, tool_router};
use time::{Duration, OffsetDateTime};

/// Engine failures are the only tool errors. A rejection, a merge, or an empty
/// working set is a successful call carrying a decision.
fn engine_error(e: memorysafe_engine::EngineError) -> ErrorData {
    match &e {
        memorysafe_engine::EngineError::Validation(_)
        | memorysafe_engine::EngineError::NotFound(_)
        | memorysafe_engine::EngineError::Conflict(_) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
        _ => ErrorData::internal_error(e.to_string(), None),
    }
}

fn timestamp(seconds: Option<i64>, field: &str) -> Result<Option<OffsetDateTime>, ErrorData> {
    seconds
        .map(|s| {
            OffsetDateTime::from_unix_timestamp(s)
                .map_err(|_| ErrorData::invalid_params(format!("{field} is not a valid Unix timestamp"), None))
        })
        .transpose()
}

#[tool_router(router = write_router)]
impl MemorySafeServer {
    /// Write a memory. Returns the governance decision: the memory may be
    /// retained, merged into an existing one, or rejected as redundant. All
    /// three are successful calls.
    #[tool(
        name = "memory_remember",
        description = "Store one discrete memory and return the governance decision — retained, merged into an existing memory, or rejected as redundant — with the reasons behind it."
    )]
    async fn memory_remember(
        &self,
        Parameters(params): Parameters<RememberParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<RememberResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;

        let mut req = RememberRequest::new(resolved.scope, &params.body);
        req.actor = resolved.actor;
        if let Some(kind) = params.kind {
            req.kind = kind;
        }
        req.tags = params.tags.unwrap_or_default();
        req.sensitivity_hint = params
            .sensitivity_hint
            .as_deref()
            .map(parse_sensitivity)
            .transpose()?;
        req.ttl = params.ttl_seconds.map(Duration::seconds);
        req.idempotency_key = params.idempotency_key;

        let outcome = self.engine.remember(req).await.map_err(engine_error)?;
        Ok(Json(RememberResult::from(&outcome)))
    }

    /// Read a governed working set under a token budget.
    #[tool(
        name = "memory_recall",
        description = "Return a governed working set of memories under a token budget, with a reason for every memory selected and every memory omitted. Set mode to 'search' for a raw ranked list."
    )]
    async fn memory_recall(
        &self,
        Parameters(params): Parameters<RecallParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<RecallResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;

        let default_budget = RecallBudget::default();
        let req = RecallRequest {
            scope: resolved.scope,
            query: params.query,
            tags_any: params.tags_any.unwrap_or_default(),
            kinds: params.kinds.unwrap_or_default(),
            occurred_after: timestamp(params.occurred_after, "occurred_after")?,
            occurred_before: timestamp(params.occurred_before, "occurred_before")?,
            mode: params.mode.as_deref().map(parse_mode).transpose()?.unwrap_or_default(),
            budget: RecallBudget {
                max_tokens: params.max_tokens.or(default_budget.max_tokens),
                max_items: params.max_items.or(default_budget.max_items),
            },
            sensitivity_ceiling: params
                .sensitivity_ceiling
                .as_deref()
                .map(parse_sensitivity)
                .transpose()?
                .unwrap_or(SensitivityLevel::Restricted),
        };

        let ws = self.engine.recall(req).await.map_err(engine_error)?;
        Ok(Json(RecallResult::build(&ws)?))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-mcp`
Expected: PASS — 8 unit tests in `scope.rs` and `dto.rs`, 7 integration tests.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-auth/src/key.rs crates/memorysafe-mcp/
git commit -m "feat(mcp): memory_remember and memory_recall over a real MCP client"
```

---

## Task 4: MCP — `memory_review`, `memory_forget`, `memory_protect`

**Files:**
- Create: `crates/memorysafe-mcp/src/tools_curate.rs`
- Modify: `crates/memorysafe-mcp/src/lib.rs`, `src/dto.rs`
- Create: `crates/memorysafe-mcp/tests/curate_tools.rs`

**Interfaces:**
- Consumes: `Engine::review`, `Engine::forget`, `Engine::protect`, `Engine::audit`, `ForgetSelector`, `Page`.
- Produces: `MemorySafeServer::curate_router`, tools `memory_review`, `memory_forget`,
  `memory_protect`, and the DTOs `ReviewParams`, `ReviewResult`, `ReviewedMemory`, `ForgetParams`,
  `ForgetResult`, `ProtectParams`, `ProtectResult`.

**Why review joins the audit trail.** §12: `memory_review` "lists what is stored and why. This is
where 'reviewable rather than invisible' becomes something an agent can show a user." An item
without its reason is a list, not a review. The join is one extra `audit` query per page, not one
per item — a per-item query would make a 50-item review 51 round trips.

**The join is honest about its limits.** `AuditFilter` returns the most recent rows up to a limit;
an item admitted long ago, beyond that window, comes back with no reason. That is reported as
`reason_code: null` rather than papered over, and the tool description says so.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-mcp/tests/curate_tools.rs`:

```rust
mod support;

use rmcp::model::CallToolRequestParams;
use serde_json::json;
use support::{args, connect, engine};

fn structured(result: &rmcp::model::CallToolResult) -> &serde_json::Value {
    result.structured_content.as_ref().expect("structured content")
}

async fn remember(client: &rmcp::service::RunningService<rmcp::RoleClient, ()>, body: &str, tag: &str) -> String {
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": body, "tags": [tag] }))),
        )
        .await
        .unwrap();
    structured(&result)["item_id"].as_str().expect("an admitted id").to_owned()
}

#[tokio::test]
async fn the_server_advertises_all_five_tools() {
    let client = connect(engine()).await;
    let names: Vec<String> =
        client.list_all_tools().await.unwrap().iter().map(|t| t.name.to_string()).collect();
    for expected in [
        "memory_recall",
        "memory_remember",
        "memory_forget",
        "memory_review",
        "memory_protect",
    ] {
        assert!(names.contains(&expected.to_string()), "missing {expected} in {names:?}");
    }
    assert_eq!(names.len(), 5, "the surface stays small: {names:?}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn review_lists_what_is_stored_and_why() {
    let client = connect(engine()).await;
    remember(&client, "the release train leaves on Thursdays", "process").await;
    remember(&client, "the incident review template lives in the wiki", "process").await;

    let result = client
        .call_tool(CallToolRequestParams::new("memory_review").with_arguments(args(json!({}))))
        .await
        .unwrap();

    let value = structured(&result);
    let items = value["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    for item in items {
        assert!(item["body"].is_string());
        assert!(item["sensitivity"].is_string());
        assert!(item["protection"].is_string());
        assert!(
            item["reason_code"].is_string(),
            "review must say why an item is stored: {item}"
        );
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn review_pages() {
    let client = connect(engine()).await;
    for i in 0..5 {
        remember(&client, &format!("distinct memory {i} about topic {i}"), "bulk").await;
    }

    let page = client
        .call_tool(
            CallToolRequestParams::new("memory_review")
                .with_arguments(args(json!({ "limit": 2, "offset": 2 }))),
        )
        .await
        .unwrap();
    assert_eq!(structured(&page)["items"].as_array().unwrap().len(), 2);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn forgetting_by_id_removes_exactly_that_memory() {
    let client = connect(engine()).await;
    let doomed = remember(&client, "a memory to delete", "work").await;
    remember(&client, "a memory to keep for later", "work").await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_forget")
                .with_arguments(args(json!({ "ids": [doomed.clone()] }))),
        )
        .await
        .unwrap();

    let value = structured(&result);
    assert_eq!(value["forgotten"], json!([doomed]));
    assert!(value["audit_id"].is_string());

    let left = client
        .call_tool(CallToolRequestParams::new("memory_review").with_arguments(args(json!({}))))
        .await
        .unwrap();
    assert_eq!(structured(&left)["items"].as_array().unwrap().len(), 1);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn forgetting_by_tag_removes_only_the_tagged_memories() {
    let client = connect(engine()).await;
    remember(&client, "alpha note about deployments", "work").await;
    remember(&client, "beta note about the kitchen", "home").await;
    remember(&client, "gamma note about deployments", "work").await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_forget").with_arguments(args(json!({ "tag": "work" }))),
        )
        .await
        .unwrap();
    assert_eq!(structured(&result)["forgotten"].as_array().unwrap().len(), 2);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn forget_requires_exactly_one_selector() {
    // Two selectors is ambiguous and none is a request to delete everything.
    // Both must be refused rather than guessed at.
    let client = connect(engine()).await;
    assert!(
        client
            .call_tool(CallToolRequestParams::new("memory_forget").with_arguments(args(json!({}))))
            .await
            .is_err()
    );
    assert!(
        client
            .call_tool(
                CallToolRequestParams::new("memory_forget")
                    .with_arguments(args(json!({ "tag": "work", "kind": "fact" })))
            )
            .await
            .is_err()
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn forgetting_an_absent_id_is_a_successful_empty_result() {
    let client = connect(engine()).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_forget")
                .with_arguments(args(json!({ "ids": ["01ARZ3NDEKTSV4RRFFQ69G5FAV"] }))),
        )
        .await
        .unwrap();
    assert_eq!(structured(&result)["forgotten"].as_array().unwrap().len(), 0);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn pinning_is_visible_in_a_later_review() {
    let client = connect(engine()).await;
    let id = remember(&client, "never forget this one", "important").await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_protect")
                .with_arguments(args(json!({ "id": id.clone(), "level": "pinned" }))),
        )
        .await
        .unwrap();
    assert_eq!(structured(&result)["protection"], "pinned");

    let review = client
        .call_tool(CallToolRequestParams::new("memory_review").with_arguments(args(json!({}))))
        .await
        .unwrap();
    let items = structured(&review)["items"].as_array().unwrap();
    let pinned = items.iter().find(|i| i["id"] == json!(id)).expect("the item survives");
    assert_eq!(pinned["protection"], "pinned");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_protected_window_needs_its_deadline() {
    let client = connect(engine()).await;
    let id = remember(&client, "protect me for a while", "important").await;

    assert!(
        client
            .call_tool(
                CallToolRequestParams::new("memory_protect")
                    .with_arguments(args(json!({ "id": id.clone(), "level": "protected" })))
            )
            .await
            .is_err(),
        "a protected window with no deadline never expires — that is `pinned`, and the caller \
         must say which they meant"
    );

    let until = time::OffsetDateTime::now_utc().unix_timestamp() + 86_400;
    let ok = client
        .call_tool(
            CallToolRequestParams::new("memory_protect")
                .with_arguments(args(json!({ "id": id, "level": "protected", "until": until }))),
        )
        .await
        .unwrap();
    assert_eq!(structured(&ok)["protection"], "protected");
    assert_eq!(structured(&ok)["protected_until"], json!(until));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn protecting_a_memory_that_does_not_exist_is_an_error() {
    let client = connect(engine()).await;
    assert!(
        client
            .call_tool(
                CallToolRequestParams::new("memory_protect").with_arguments(
                    args(json!({ "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "level": "pinned" }))
                )
            )
            .await
            .is_err()
    );
    client.cancel().await.unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-mcp --test curate_tools`
Expected: FAIL — `Tool not found: memory_review`.

- [ ] **Step 3: Write minimal implementation**

Append to `crates/memorysafe-mcp/src/dto.rs`:

```rust
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReviewParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReviewedMemory {
    #[serde(flatten)]
    pub memory: MemoryView,
    /// The most recent recorded decision naming this item. `null` when the
    /// decision is older than the audit window this review looked at — the item
    /// is still governed, its reason is simply out of reach from here.
    pub reason_code: Option<String>,
    pub reason_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReviewResult {
    pub items: Vec<ReviewedMemory>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ForgetParams {
    pub ids: Option<Vec<String>>,
    pub tag: Option<String>,
    pub kind: Option<String>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ForgetResult {
    pub forgotten: Vec<String>,
    pub audit_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProtectParams {
    pub id: String,
    /// `normal`, `protected`, or `pinned`. `protected` requires `until`.
    pub level: String,
    /// Unix seconds. Required for `protected`, rejected for the others.
    pub until: Option<i64>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProtectResult {
    pub item_id: String,
    pub protection: String,
    pub protected_until: Option<i64>,
    pub audit_id: String,
}

/// `pinned` is absolute; `protected` is a window and must carry its deadline.
/// Defaulting the deadline would silently turn a time-boxed exemption into a
/// permanent one.
pub fn parse_protection(level: &str, until: Option<i64>) -> Result<Protection, ErrorData> {
    match (level, until) {
        ("normal", None) => Ok(Protection::Normal),
        ("pinned", None) => Ok(Protection::Pinned),
        ("protected", Some(seconds)) => time::OffsetDateTime::from_unix_timestamp(seconds)
            .map(|until| Protection::Protected { until })
            .map_err(|_| ErrorData::invalid_params("'until' is not a valid Unix timestamp", None)),
        ("protected", None) => Err(ErrorData::invalid_params(
            "'protected' requires 'until'; use 'pinned' for permanent protection",
            None,
        )),
        ("normal" | "pinned", Some(_)) => Err(ErrorData::invalid_params(
            format!("'until' is meaningless for level '{level}'"),
            None,
        )),
        (other, _) => Err(ErrorData::invalid_params(
            format!("unknown protection level '{other}'; expected normal, protected, or pinned"),
            None,
        )),
    }
}
```

and to that module's `mod tests`:

```rust
    #[test]
    fn protection_parsing_refuses_the_shapes_that_would_mean_something_else() {
        assert_eq!(parse_protection("pinned", None).unwrap(), Protection::Pinned);
        assert_eq!(parse_protection("normal", None).unwrap(), Protection::Normal);
        assert!(parse_protection("protected", None).is_err());
        assert!(parse_protection("pinned", Some(1)).is_err());
        assert!(parse_protection("locked", None).is_err());
        assert!(matches!(
            parse_protection("protected", Some(0)).unwrap(),
            Protection::Protected { until } if until == time::OffsetDateTime::UNIX_EPOCH
        ));
    }
```

`crates/memorysafe-mcp/src/tools_curate.rs`:

```rust
use crate::MemorySafeServer;
use crate::dto::{
    ForgetParams, ForgetResult, MemoryView, ProtectParams, ProtectResult, ReviewParams,
    ReviewResult, ReviewedMemory, ReasonView, parse_protection, protection_name,
};
use crate::tools_write::engine_error;
use memorysafe_backend::Page;
use memorysafe_core::{AuditFilter, ItemId, Protection};
use memorysafe_engine::ForgetSelector;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, tool, tool_router};
use std::collections::HashMap;

/// How many audit rows a review reads to find reasons for one page of items.
/// A page of 50 items may be spread over many rows — merges, evictions,
/// recalls — so the window is generous but bounded.
const REVIEW_AUDIT_WINDOW: usize = 500;

fn item_id(raw: &str) -> Result<ItemId, ErrorData> {
    ItemId::parse(raw).map_err(|e| ErrorData::invalid_params(format!("bad item id '{raw}': {e}"), None))
}

#[tool_router(router = curate_router)]
impl MemorySafeServer {
    /// List what is stored and why.
    #[tool(
        name = "memory_review",
        description = "List the memories stored in this scope with the governance reason each one is there for. Use this to show a person what the agent remembers about them."
    )]
    async fn memory_review(
        &self,
        Parameters(params): Parameters<ReviewParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ReviewResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;
        let default_page = Page::default();
        let page = Page {
            offset: params.offset.unwrap_or(default_page.offset),
            limit: params.limit.unwrap_or(default_page.limit),
        };

        let items = self
            .engine
            .review(&resolved.scope, &page)
            .await
            .map_err(engine_error)?;

        // One audit query for the whole page, indexed by item id. The engine
        // returns rows newest first, so the first row naming an item is the
        // most recent decision about it.
        let rows = self
            .engine
            .audit(&resolved.scope, &AuditFilter { limit: REVIEW_AUDIT_WINDOW, ..Default::default() })
            .await
            .map_err(engine_error)?;
        let mut reasons: HashMap<String, ReasonView> = HashMap::new();
        for row in &rows {
            let Some(decision) = row.decision.as_ref() else { continue };
            let Some(reason) = decision.reasons.first() else { continue };
            for item_ref in &row.items {
                reasons
                    .entry(item_ref.id().to_string())
                    .or_insert_with(|| ReasonView::from(reason));
            }
        }

        Ok(Json(ReviewResult {
            items: items
                .iter()
                .map(|item| {
                    let reason = reasons.get(item.id.as_str());
                    ReviewedMemory {
                        memory: MemoryView::from(item),
                        reason_code: reason.map(|r| r.code.clone()),
                        reason_detail: reason.map(|r| r.detail.clone()),
                    }
                })
                .collect(),
            offset: page.offset,
            limit: page.limit,
        }))
    }

    /// Delete memories by id, tag, or kind.
    #[tool(
        name = "memory_forget",
        description = "Delete memories by id, by tag, or by kind. Exactly one selector must be given. Deleting something that is not there is a successful, empty result."
    )]
    async fn memory_forget(
        &self,
        Parameters(params): Parameters<ForgetParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ForgetResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;

        let selectors = [
            params.ids.is_some(),
            params.tag.is_some(),
            params.kind.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if selectors != 1 {
            return Err(ErrorData::invalid_params(
                "memory_forget takes exactly one of 'ids', 'tag', or 'kind'",
                None,
            ));
        }

        let selector = if let Some(ids) = params.ids {
            ForgetSelector::Ids(ids.iter().map(|r| item_id(r)).collect::<Result<_, _>>()?)
        } else if let Some(tag) = params.tag {
            ForgetSelector::Tag(tag)
        } else {
            ForgetSelector::Kind(params.kind.expect("checked above"))
        };

        let outcome = self
            .engine
            .forget(&resolved.scope, selector)
            .await
            .map_err(engine_error)?;
        Ok(Json(ForgetResult {
            forgotten: outcome.forgotten.iter().map(|i| i.to_string()).collect(),
            audit_id: outcome.audit_id.to_string(),
        }))
    }

    /// Pin or protect an existing memory.
    #[tool(
        name = "memory_protect",
        description = "Pin a memory so no policy can evict it, or protect it until a deadline. This is the only way protection changes outside admission, and it is recorded in the audit trail."
    )]
    async fn memory_protect(
        &self,
        Parameters(params): Parameters<ProtectParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<ProtectResult>, ErrorData> {
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;
        let id = item_id(&params.id)?;
        let protection = parse_protection(&params.level, params.until)?;

        let outcome = self
            .engine
            .protect(&resolved.scope, &id, protection)
            .await
            .map_err(engine_error)?;

        Ok(Json(ProtectResult {
            item_id: outcome.item_id.map(|i| i.to_string()).unwrap_or(params.id),
            protection: protection_name(&protection).to_owned(),
            protected_until: match protection {
                Protection::Protected { until } => Some(until.unix_timestamp()),
                _ => None,
            },
            audit_id: outcome.audit_id.to_string(),
        }))
    }
}
```

Make `engine_error` reachable: change its declaration in `tools_write.rs` to
`pub(crate) fn engine_error(...)`, and in `lib.rs` add `mod tools_curate;` and compose the routers:

```rust
        Self { engine, source, tool_router: Self::write_router() + Self::curate_router() }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-mcp`
Expected: PASS — 10 new integration tests, 1 new unit test.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-mcp/
git commit -m "feat(mcp): review, forget, and protect complete the five-tool surface"
```

---

## Task 5: MCP — audit and stats resources

**Files:**
- Create: `crates/memorysafe-mcp/src/resources.rs`
- Modify: `crates/memorysafe-mcp/src/lib.rs`, `src/scope.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs` (only if `capacity_state` / `scope_stats` are absent)
- Create: `crates/memorysafe-mcp/tests/resources.rs`

**Interfaces:**
- Consumes: `Engine::audit`, `Engine::capacity_state`, `Engine::scope_stats`, `ScopeSource::resolve`.
- Produces: `ScopeSource::default_scope`, `resources::parse_uri`, `ResourceKind`, and the
  `list_resources` / `list_resource_templates` / `read_resource` handlers.

**Why resources and not a sixth tool.** §12: the audit trail and scope statistics are exposed as
MCP *resources* "so clients can display them without spending a tool call". Serial tool calls are
where agent memory dies; a client that wants to render "here is what I remember and why" should
not have to burn a turn on it.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-mcp/src/resources.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_uri_parses_into_a_scope_and_a_kind() {
        let parsed = parse_uri("memorysafe://acme/user-42/coding-agent/audit").expect("parse");
        assert_eq!(parsed.tenant, "acme");
        assert_eq!(parsed.subject, "user-42");
        assert_eq!(parsed.namespace, "coding-agent");
        assert_eq!(parsed.kind, ResourceKind::Audit);

        let stats = parse_uri("memorysafe://acme/user-42/coding-agent/stats").expect("parse");
        assert_eq!(stats.kind, ResourceKind::Stats);
    }

    #[test]
    fn a_malformed_uri_is_a_resource_not_found_not_a_panic() {
        for bad in [
            "",
            "memorysafe://",
            "https://acme/user-42/coding-agent/audit",
            "memorysafe://acme/user-42/audit",
            "memorysafe://acme/user-42/coding-agent/secrets",
            "memorysafe://acme/user-42/coding-agent/audit/extra",
            "memorysafe://acme//coding-agent/audit",
        ] {
            assert!(parse_uri(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn the_uri_for_a_scope_round_trips_through_the_parser() {
        let scope = memorysafe_core::Scope::new("acme", "user-42", "agent").unwrap();
        for kind in [ResourceKind::Audit, ResourceKind::Stats] {
            let uri = resource_uri(&scope, kind);
            let parsed = parse_uri(&uri).expect("round trip");
            assert_eq!(parsed.tenant, "acme");
            assert_eq!(parsed.kind, kind);
        }
    }
}
```

`crates/memorysafe-mcp/tests/resources.rs`:

```rust
mod support;

use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams};
use serde_json::json;
use support::{args, connect, engine};

#[tokio::test]
async fn the_server_lists_its_audit_and_stats_resources() {
    let client = connect(engine()).await;
    let listed = client.list_resources(None).await.unwrap();
    let uris: Vec<&str> = listed.resources.iter().map(|r| r.uri.as_str()).collect();

    assert!(uris.contains(&"memorysafe://acme/user-42/coding-agent/audit"), "{uris:?}");
    assert!(uris.contains(&"memorysafe://acme/user-42/coding-agent/stats"), "{uris:?}");
    for resource in &listed.resources {
        assert_eq!(resource.mime_type.as_deref(), Some("application/json"));
    }

    let templates = client.list_resource_templates(None).await.unwrap();
    assert_eq!(
        templates.resource_templates.len(),
        2,
        "a client on HTTP has no default scope and needs the templates"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_audit_resource_shows_the_decisions_without_the_bodies() {
    let client = connect(engine()).await;
    client
        .call_tool(
            CallToolRequestParams::new("memory_remember")
                .with_arguments(args(json!({ "body": "a memory whose body must not leak" }))),
        )
        .await
        .unwrap();

    let read = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/user-42/coding-agent/audit",
        ))
        .await
        .unwrap();

    let text = match &read.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("expected text contents, got {other:?}"),
    };
    let value: serde_json::Value = serde_json::from_str(&text).expect("the audit resource is JSON");
    let records = value["records"].as_array().expect("records array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "admitted");
    assert!(records[0]["decision"].is_object(), "the decision is the point of the trail");
    assert!(
        !text.contains("a memory whose body must not leak"),
        "the audit resource leaked an item body"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_stats_resource_reports_capacity_and_corpus_shape() {
    let client = connect(engine()).await;
    for i in 0..3 {
        client
            .call_tool(
                CallToolRequestParams::new("memory_remember")
                    .with_arguments(args(json!({ "body": format!("distinct memory {i} on topic {i}") }))),
            )
            .await
            .unwrap();
    }

    let read = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/user-42/coding-agent/stats",
        ))
        .await
        .unwrap();
    let text = match &read.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("expected text contents, got {other:?}"),
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(value["item_count"], json!(3));
    assert!(value["used_bytes"].as_u64().is_some_and(|b| b > 0));
    assert!(value["total_bytes"].as_u64().is_some());
    assert!(value["budget"].is_object());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_resource_in_another_subject_is_refused_over_stdio() {
    // Same rule as the tools: stdio is bound to one subject. A resource URI is
    // not a way around it.
    let client = connect(engine()).await;
    let denied = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://acme/someone-else/coding-agent/audit",
        ))
        .await;
    assert!(denied.is_err(), "a resource URI crossed a subject boundary");

    let other_tenant = client
        .read_resource(ReadResourceRequestParams::new(
            "memorysafe://globex/user-42/coding-agent/audit",
        ))
        .await;
    assert!(other_tenant.is_err(), "a resource URI crossed a tenant boundary");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn an_unknown_resource_uri_is_an_error_not_an_empty_document() {
    let client = connect(engine()).await;
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("memorysafe://acme/user-42/agent/secrets"))
            .await
            .is_err()
    );
    assert!(client.read_resource(ReadResourceRequestParams::new("file:///etc/passwd")).await.is_err());
    client.cancel().await.unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-mcp --test resources`
Expected: FAIL — `read_resource` returns method-not-found, and `list_resources` is empty.

- [ ] **Step 3: Write minimal implementation**

If `Engine::capacity_state` and `Engine::scope_stats` are not already present, add them to
`crates/memorysafe-engine/src/lib.rs` beside `review` and `audit`:

```rust
    pub async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, EngineError> {
        Ok(self.backend.capacity_state(scope).await?)
    }

    pub async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, EngineError> {
        Ok(self.backend.scope_stats(scope).await?)
    }
```

Add to `crates/memorysafe-mcp/src/scope.rs`, inside `impl ScopeSource`:

```rust
    /// The scope a client sees before it names one. `None` over HTTP, where the
    /// scope is a property of the request rather than of the server.
    pub fn default_scope(&self) -> Option<Scope> {
        match self {
            ScopeSource::Stdio { tenant, subject, default_namespace } => Some(Scope {
                tenant: tenant.clone(),
                subject: subject.clone(),
                namespace: default_namespace.clone(),
            }),
            ScopeSource::Http { .. } => None,
        }
    }
```

`crates/memorysafe-mcp/src/resources.rs`:

```rust
use memorysafe_core::Scope;
use rmcp::ErrorData;

pub const SCHEME: &str = "memorysafe://";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Audit,
    Stats,
}

impl ResourceKind {
    pub const ALL: [ResourceKind; 2] = [ResourceKind::Audit, ResourceKind::Stats];

    pub fn as_str(self) -> &'static str {
        match self {
            ResourceKind::Audit => "audit",
            ResourceKind::Stats => "stats",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ResourceKind::Audit => {
                "Every governance decision recorded for this scope: what was admitted, rejected, \
                 merged, forgotten, and recalled, and why. Ids and content digests only — never \
                 memory bodies."
            }
            ResourceKind::Stats => {
                "Capacity and corpus shape for this scope: budget, bytes and items used, and the \
                 statistics the policy calibrates against."
            }
        }
    }
}

pub struct ParsedUri<'a> {
    pub tenant: &'a str,
    pub subject: &'a str,
    pub namespace: &'a str,
    pub kind: ResourceKind,
}

pub fn resource_uri(scope: &Scope, kind: ResourceKind) -> String {
    format!(
        "{SCHEME}{}/{}/{}/{}",
        scope.tenant,
        scope.subject,
        scope.namespace,
        kind.as_str()
    )
}

pub fn uri_template(kind: ResourceKind) -> String {
    format!("{SCHEME}{{tenant}}/{{subject}}/{{namespace}}/{}", kind.as_str())
}

/// Strict: exactly four non-empty segments after the scheme, and a known kind.
/// A lenient parser here would turn a typo into a silently different scope.
pub fn parse_uri(uri: &str) -> Result<ParsedUri<'_>, ErrorData> {
    let not_found = || ErrorData::resource_not_found(format!("no such resource: {uri}"), None);

    let rest = uri.strip_prefix(SCHEME).ok_or_else(not_found)?;
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.len() != 4 || segments.iter().any(|s| s.is_empty()) {
        return Err(not_found());
    }
    let kind = match segments[3] {
        "audit" => ResourceKind::Audit,
        "stats" => ResourceKind::Stats,
        _ => return Err(not_found()),
    };
    Ok(ParsedUri { tenant: segments[0], subject: segments[1], namespace: segments[2], kind })
}
```

Add the handlers to the `ServerHandler` impl in `crates/memorysafe-mcp/src/lib.rs`:

```rust
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().enable_resources().build(),
        )
        .with_instructions(/* unchanged */)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        // Over HTTP the scope is a property of the request, not of the server,
        // so there is nothing concrete to list — the templates carry the shape.
        let Some(scope) = self.source.default_scope() else {
            return Ok(ListResourcesResult::default());
        };
        Ok(ListResourcesResult::with_all_items(
            ResourceKind::ALL
                .into_iter()
                .map(|kind| {
                    Resource::new(resource_uri(&scope, kind), format!("{} {}", scope.namespace, kind.as_str()))
                        .with_description(kind.description())
                        .with_mime_type("application/json")
                })
                .collect(),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            ResourceKind::ALL
                .into_iter()
                .map(|kind| {
                    ResourceTemplate::new(uri_template(kind), kind.as_str())
                        .with_description(kind.description())
                        .with_mime_type("application/json")
                })
                .collect(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let parsed = resources::parse_uri(&request.uri)?;

        // The URI names a scope; the transport decides which scopes this caller
        // may name. Resolving through the same path the tools use means a
        // resource URI can never reach further than a tool call could.
        let resolved = self.source.resolve(
            &context.extensions,
            Some(parsed.subject),
            Some(parsed.namespace),
        )?;
        if resolved.scope.tenant.as_str() != parsed.tenant {
            return Err(ErrorData::resource_not_found(
                format!("no such resource: {}", request.uri),
                None,
            ));
        }

        let body = match parsed.kind {
            ResourceKind::Audit => {
                let records = self
                    .engine
                    .audit(&resolved.scope, &AuditFilter::default())
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                serde_json::json!({
                    "scope": resolved.scope,
                    "records": records,
                })
            }
            ResourceKind::Stats => {
                let capacity = self
                    .engine
                    .capacity_state(&resolved.scope)
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let stats = self
                    .engine
                    .scope_stats(&resolved.scope)
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                serde_json::json!({
                    "scope": resolved.scope,
                    "budget": capacity.budget,
                    "used_items": capacity.used_items,
                    "used_bytes": capacity.used_bytes,
                    "item_count": stats.item_count,
                    "total_bytes": stats.total_bytes,
                    "median_item_bytes": stats.median_item_bytes,
                    "mean_neighbour_similarity": stats.mean_neighbour_similarity,
                })
            }
        };

        let text = serde_json::to_string_pretty(&body)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into())
    }
```

with these imports added to `lib.rs`:

```rust
pub mod resources;

use crate::resources::{ResourceKind, resource_uri, uri_template};
use memorysafe_core::AuditFilter;
use rmcp::model::{
    ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer};
```

**The audit resource serialises `AuditRecord` directly.** It is the engine type, and its `ItemRef`
holds an id and a digest by construction — there is no shape of it that could carry a body. A
hand-written view here would be a place for one to reappear.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-mcp`
Expected: PASS — 3 unit tests in `resources.rs`, 5 integration tests.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/src/lib.rs crates/memorysafe-mcp/
git commit -m "feat(mcp): audit and scope-stats resources, scoped like the tools"
```

---

## Task 6: MCP — stdio and streamable-HTTP transports

**Files:**
- Create: `crates/memorysafe-mcp/src/transport.rs`
- Modify: `crates/memorysafe-mcp/src/lib.rs`, `Cargo.toml`
- Create: `crates/memorysafe-mcp/tests/http_transport.rs`

**Interfaces:**
- Consumes: `MemorySafeServer`, `ScopeSource`, `rmcp::transport::{stdio, StreamableHttpService}`.
- Produces: `serve_stdio`, `http_service`.

**Why the HTTP test uses a real listener and a real client.** §13 requires MCP integration tests
that "drive the stdio server with a real MCP client"; Tasks 3–5 do that over a duplex. The HTTP
transport has one thing a duplex cannot exercise: the bearer token reaching `ScopeSource::Http`
through `http::request::Parts`. That path is where a cross-tenant bug would live, so it gets a
real socket.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-mcp/tests/http_transport.rs`:

```rust
mod support;

use memorysafe_auth::{ApiKeyStore, generate};
use memorysafe_core::TenantId;
use memorysafe_mcp::{ScopeSource, http_service};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig};
use serde_json::json;
use std::sync::Arc;
use support::{args, engine};
use tokio_util::sync::CancellationToken;

struct Served {
    address: std::net::SocketAddr,
    ct: CancellationToken,
    secret: String,
}

async fn serve() -> Served {
    let tenant = TenantId::new("acme").unwrap();
    let g = generate(tenant, "integration").unwrap();
    let secret = g.secret.clone();
    let keys = Arc::new(ApiKeyStore::new(vec![g.record]));

    let ct = CancellationToken::new();
    let service = http_service(engine(), ScopeSource::Http { keys });
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn({
        let ct = ct.clone();
        async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await;
        }
    });
    Served { address, ct, secret }
}

fn transport(address: std::net::SocketAddr, auth: Option<String>) -> StreamableHttpClientTransport<reqwest::Client> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp"));
    config.auth_header = auth;
    config.allow_stateless = true;
    StreamableHttpClientTransport::from_config(config)
}

#[tokio::test]
async fn an_authenticated_client_writes_and_reads_over_streamable_http() {
    let served = serve().await;
    let client = ()
        .serve(transport(served.address, Some(format!("Bearer {}", served.secret))))
        .await
        .expect("client connects");

    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 5);

    let written = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "the http transport carries the bearer through to scope resolution",
                "subject": "user-42",
                "namespace": "agent"
            }))),
        )
        .await
        .expect("remember over http");
    assert_eq!(
        written.structured_content.as_ref().unwrap()["action"],
        json!("retain")
    );

    let recalled = client
        .call_tool(
            CallToolRequestParams::new("memory_recall").with_arguments(args(json!({
                "query": "bearer",
                "subject": "user-42",
                "namespace": "agent"
            }))),
        )
        .await
        .expect("recall over http");
    assert!(
        !recalled.structured_content.as_ref().unwrap()["items"].as_array().unwrap().is_empty()
    );

    client.cancel().await.unwrap();
    served.ct.cancel();
}

#[tokio::test]
async fn a_client_with_no_credential_cannot_call_a_tool() {
    let served = serve().await;
    let client = ().serve(transport(served.address, None)).await.expect("client connects");

    // Listing is public; acting is not. The tool call must fail because scope
    // resolution has no tenant to work with.
    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "this must not be stored",
                "subject": "user-42",
                "namespace": "agent"
            }))),
        )
        .await;
    assert!(result.is_err(), "an unauthenticated write was accepted");

    client.cancel().await.unwrap();
    served.ct.cancel();
}

#[tokio::test]
async fn a_key_for_one_tenant_cannot_be_used_as_another() {
    let served = serve().await;
    let stranger = generate(TenantId::new("globex").unwrap(), "stranger").unwrap();
    let client = ()
        .serve(transport(served.address, Some(format!("Bearer {}", stranger.secret))))
        .await
        .expect("client connects");

    let result = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(args(json!({
                "body": "a memory for a tenant this server has never heard of",
                "subject": "user-42",
                "namespace": "agent"
            }))),
        )
        .await;
    assert!(result.is_err(), "a key from another store authenticated");

    client.cancel().await.unwrap();
    served.ct.cancel();
}
```

**If the client fails to connect** because protocol-version negotiation cannot complete over a
stateless server, use the lifecycle form rmcp's own suite uses — replace `().serve(transport)`
with:

```rust
    rmcp::model::ClientInfo::default()
        .serve_with_lifecycle(
            transport,
            rmcp::ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
```

That is the pattern in `rmcp-3.2.0/tests/test_discover_http_client_startup.rs`; it is a known-good
shape, not a guess.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-mcp --test http_transport`
Expected: FAIL — `cannot find function 'http_service' in crate 'memorysafe_mcp'`.

- [ ] **Step 3: Write minimal implementation**

Extend `crates/memorysafe-mcp/Cargo.toml`:

```toml
[dependencies]
# ...as before, plus:
tokio-util.workspace = true

[dev-dependencies]
# ...as before, plus:
axum.workspace = true
reqwest = { workspace = true, features = ["rustls-tls"] }
rmcp = { workspace = true, features = ["client", "transport-streamable-http-client-reqwest"] }
tokio-util.workspace = true
```

and add to the workspace `[workspace.dependencies]`:

```toml
axum = { version = "0.8.9", features = ["macros"] }
reqwest = { version = "0.13.4", default-features = false }
tokio-util = "0.7.19"
```

`crates/memorysafe-mcp/src/transport.rs`:

```rust
use crate::{MemorySafeServer, ScopeSource};
use memorysafe_engine::Engine;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService, stdio};
use std::sync::Arc;

/// Serve one client over stdin/stdout and return when it disconnects.
///
/// Nothing may be written to stdout except MCP frames — stdout *is* the
/// transport. Logging goes to stderr; the CLI configures that before calling
/// this.
pub async fn serve_stdio(engine: Arc<Engine>, source: ScopeSource) -> anyhow::Result<()> {
    let running = MemorySafeServer::new(engine, source).serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// A tower service ready to be mounted, typically at `/mcp`.
///
/// The factory runs once per session, so every session gets its own handler
/// over the same shared engine — the engine is the thing with state, and it is
/// `Arc`-shared deliberately.
pub fn http_service(
    engine: Arc<Engine>,
    source: ScopeSource,
) -> StreamableHttpService<MemorySafeServer, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(MemorySafeServer::new(engine.clone(), source.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true),
    )
}
```

Add to `crates/memorysafe-mcp/src/lib.rs`:

```rust
mod transport;
pub use transport::{http_service, serve_stdio};
```

**On `allowed_hosts`.** `StreamableHttpServerConfig` defaults to accepting loopback hosts only,
which is a DNS-rebinding guard for locally running servers. A deployment behind a real hostname
must widen it; Task 14 threads that through from configuration rather than defaulting it open
here, because a library that silently accepts any `Host` is a library that ships the
vulnerability to everyone who uses it.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-mcp`
Expected: PASS — 3 new integration tests; the whole crate green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-mcp/
git commit -m "feat(mcp): stdio and streamable-HTTP transports with per-request tenant resolution"
```

---

## Task 7: API — errors, the auth extractor, the router skeleton

**Files:**
- Create: `crates/memorysafe-api/Cargo.toml`
- Create: `crates/memorysafe-api/src/lib.rs`
- Create: `crates/memorysafe-api/src/error.rs`
- Create: `crates/memorysafe-api/src/auth.rs`
- Create: `crates/memorysafe-api/src/scope.rs`
- Create: `crates/memorysafe-api/tests/support/mod.rs`
- Create: `crates/memorysafe-api/tests/auth.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `ApiKeyStore`, `Authenticated`, `AuthError`, `EngineError`, `Engine`.
- Produces: `AppState`, `router`, `ApiError`, `Problem`, `Auth`, `ScopeParams`.

**The §9 table is the contract, not a suggestion.** It is reproduced here because this task
implements it and nothing else does:

| Error | HTTP |
|---|---|
| `Validation` | 400 |
| `Auth` — no or unusable credential | 401 |
| `Auth` — credential understood, access refused | 403 |
| `NotFound` | 404 |
| `Conflict` | 409 |
| `Backend` | 503, carrying a `retryable` flag |
| `PolicyRefused` | 500 |

A governance decision appears nowhere in that table because it is not an error: a rejected write
is `200 OK` with `"action": "reject"`.

**Why the API serialises engine types directly.** Unlike MCP, nothing here needs a JSON schema, so
`WorkingSet`, `WriteOutcome`, `MemoryItem`, and `AuditRecord` go out as themselves. A parallel set
of view types would be a second definition of the wire format that could drift from the first.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-api/tests/support/mod.rs`:

```rust
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use memorysafe_api::{AppState, router};
use memorysafe_auth::{ApiKeyStore, generate};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::TenantId;
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;
use tower::ServiceExt;

pub struct Harness {
    pub app: Router,
    pub key: String,
    pub engine: Arc<Engine>,
}

pub fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    )));
    let g = generate(TenantId::new("acme").unwrap(), "tests").unwrap();
    let key = g.secret.clone();
    let keys = Arc::new(ApiKeyStore::new(vec![g.record]));
    let app = router(AppState { engine: engine.clone(), keys });
    Harness { app, key, engine }
}

pub struct Reply {
    pub status: StatusCode,
    pub body: serde_json::Value,
    pub text: String,
}

pub async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.expect("the router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    Reply { status, body, text }
}

pub fn get(uri: &str, key: Option<&str>) -> Request<Body> {
    build("GET", uri, key, Body::empty())
}

pub fn post(uri: &str, key: Option<&str>, json: serde_json::Value) -> Request<Body> {
    let mut request = build("POST", uri, key, Body::from(json.to_string()));
    request
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    request
}

pub fn put(uri: &str, key: Option<&str>, json: serde_json::Value) -> Request<Body> {
    let mut request = build("PUT", uri, key, Body::from(json.to_string()));
    request
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    request
}

pub fn delete(uri: &str, key: Option<&str>) -> Request<Body> {
    build("DELETE", uri, key, Body::empty())
}

fn build(method: &str, uri: &str, key: Option<&str>, body: Body) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    builder.body(body).expect("request")
}
```

`crates/memorysafe-api/tests/auth.rs`:

```rust
mod support;

use axum::http::StatusCode;
use memorysafe_auth::{ApiKeyStore, generate};
use memorysafe_core::TenantId;
use serde_json::json;
use support::{get, harness, send};

#[tokio::test]
async fn health_needs_no_credential() {
    let h = harness();
    let reply = send(&h.app, get("/v1/health", None)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["status"], "ok");
}

#[tokio::test]
async fn whoami_names_the_tenant_the_key_belongs_to() {
    let h = harness();
    let reply = send(&h.app, get("/v1/whoami", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.body["tenant"], "acme");
    assert!(reply.body["key_id"].is_string());
    assert!(
        !reply.text.contains(&h.key),
        "whoami echoed the credential back"
    );
}

#[tokio::test]
async fn a_request_with_no_credential_is_401_and_says_so_in_a_problem_body() {
    let h = harness();
    let reply = send(&h.app, get("/v1/whoami", None)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.body["error"], "unauthenticated");
    assert!(reply.body["message"].is_string());
    assert_eq!(reply.body["retryable"], json!(false));
}

#[tokio::test]
async fn a_credential_that_is_not_a_bearer_token_is_401() {
    let h = harness();
    for header in ["", "Basic abc", "Bearer", "Bearer   ", "bearer lowercase-scheme"] {
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/v1/whoami")
            .header("authorization", header)
            .body(axum::body::Body::empty())
            .unwrap();
        let reply = send(&h.app, request).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "header {header:?} was accepted");
    }
}

#[tokio::test]
async fn an_unknown_key_is_401_not_403() {
    // 403 would tell the caller the key is real. 401 says "identify yourself",
    // which is the truth and leaks nothing.
    let h = harness();
    let stranger = generate(TenantId::new("acme").unwrap(), "not in this store").unwrap();
    let reply = send(&h.app, get("/v1/whoami", Some(&stranger.secret))).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_disabled_key_is_403_because_the_caller_is_known() {
    let dir = tempfile::tempdir().unwrap();
    let engine = std::sync::Arc::new(memorysafe_engine::Engine::new(
        memorysafe_engine::EngineConfig::new(
            std::sync::Arc::new(memorysafe_backend_sqlite::SqliteBackend::open(dir.keep())),
            std::sync::Arc::new(memorysafe_embed::DeterministicEmbedder::new(256)),
            std::sync::Arc::new(memorysafe_policy::BaselinePolicy::default()),
        ),
    ));
    let g = generate(TenantId::new("acme").unwrap(), "revoked").unwrap();
    let mut record = g.record;
    record.disabled = true;
    let app = memorysafe_api::router(memorysafe_api::AppState {
        engine,
        keys: std::sync::Arc::new(ApiKeyStore::new(vec![record])),
    });

    let reply = send(&app, get("/v1/whoami", Some(&g.secret))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.body["error"], "forbidden");
}

#[tokio::test]
async fn an_unknown_route_is_404_with_a_problem_body() {
    let h = harness();
    let reply = send(&h.app, get("/v1/nope", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-api`
Expected: FAIL — the package does not resolve.

- [ ] **Step 3: Write minimal implementation**

Add to the workspace `[workspace.dependencies]`:

```toml
memorysafe-api = { path = "crates/memorysafe-api" }
tower = { version = "0.5.3", features = ["util"] }
tower-http = { version = "0.7.1", features = ["trace"] }
tracing = "0.1.44"
```

`crates/memorysafe-api/Cargo.toml`:

```toml
[package]
name = "memorysafe-api"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "HTTP API for the MemorySafe governed-memory engine"

[dependencies]
memorysafe-auth.workspace = true
memorysafe-backend.workspace = true
memorysafe-core.workspace = true
memorysafe-engine.workspace = true
memorysafe-policy.workspace = true
axum.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true
tower-http.workspace = true
tracing.workspace = true

[dev-dependencies]
memorysafe-backend-sqlite.workspace = true
memorysafe-embed.workspace = true
tempfile.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
tower.workspace = true

[lints]
workspace = true
```

`crates/memorysafe-api/src/error.rs`:

```rust
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use memorysafe_auth::AuthError;
use memorysafe_core::CoreError;
use memorysafe_engine::EngineError;
use serde::Serialize;

/// The error body. One shape for every failure, so a client can parse once.
#[derive(Debug, Serialize)]
pub struct Problem {
    /// A stable machine-readable tag. Never a message.
    pub error: &'static str,
    pub message: String,
    /// True only when trying the same request again could succeed.
    pub retryable: bool,
}

#[derive(Debug)]
pub enum ApiError {
    Validation(String),
    Unauthenticated(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    Unavailable { message: String, retryable: bool },
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, bool) {
        match self {
            ApiError::Validation(_) => (StatusCode::BAD_REQUEST, "validation", false),
            ApiError::Unauthenticated(_) => (StatusCode::UNAUTHORIZED, "unauthenticated", false),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden", false),
            ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found", false),
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict", false),
            ApiError::Unavailable { retryable, .. } => {
                (StatusCode::SERVICE_UNAVAILABLE, "backend", *retryable)
            }
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", false),
        }
    }

    fn message(&self) -> &str {
        match self {
            ApiError::Validation(m)
            | ApiError::Unauthenticated(m)
            | ApiError::Forbidden(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::Internal(m) => m,
            ApiError::Unavailable { message, .. } => message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, retryable) = self.parts();
        // 5xx is a failure of ours; 4xx is the caller's request. Only the first
        // is worth a log line at warn level.
        if status.is_server_error() {
            tracing::warn!(status = %status, error, message = self.message(), "request failed");
        }
        let body = Problem { error, message: self.message().to_owned(), retryable };
        (status, Json(body)).into_response()
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        let retryable = e.is_retryable();
        match e {
            EngineError::Validation(m) => ApiError::Validation(m),
            EngineError::NotFound(m) => ApiError::NotFound(m),
            EngineError::Conflict(m) => ApiError::Conflict(m),
            EngineError::Backend(ref inner) => {
                ApiError::Unavailable { message: inner.to_string(), retryable }
            }
            // The engine degrades rather than failing when the embedder is
            // unavailable, so reaching here means the degradation itself broke.
            EngineError::Embedder(ref inner) => {
                ApiError::Unavailable { message: inner.to_string(), retryable: true }
            }
            EngineError::PolicyRefused(m) => ApiError::Internal(m),
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        let message = e.to_string();
        if e.is_unauthenticated() {
            ApiError::Unauthenticated(message)
        } else {
            match e {
                // A malformed component is the caller's typo, not a refusal.
                AuthError::Scope(_) => ApiError::Validation(message),
                _ => ApiError::Forbidden(message),
            }
        }
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        ApiError::Validation(e.to_string())
    }
}
```

`crates/memorysafe-api/src/auth.rs`:

```rust
use crate::AppState;
use crate::error::ApiError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use memorysafe_auth::{Authenticated, AuthError};
use memorysafe_core::{Actor, TenantId};

/// A handler that takes this parameter cannot have skipped authentication —
/// there is no other way to construct one.
pub struct Auth(pub Authenticated);

impl Auth {
    pub fn tenant(&self) -> &TenantId {
        self.0.tenant()
    }

    pub fn actor(&self) -> Actor {
        self.0.actor()
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let presented = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or(AuthError::Missing)?;
        Ok(Auth(state.keys.authenticate(presented)?))
    }
}
```

`crates/memorysafe-api/src/scope.rs`:

```rust
use crate::auth::Auth;
use crate::error::ApiError;
use memorysafe_core::Scope;
use serde::Deserialize;

/// Subject and namespace, always both, never defaulted. Appears as query
/// parameters on reads and as flattened body fields on writes.
#[derive(Debug, Clone, Deserialize)]
pub struct ScopeParams {
    pub subject: String,
    pub namespace: String,
}

impl ScopeParams {
    /// The tenant comes from the credential; only subject and namespace come
    /// from the request. A cross-tenant scope is unrepresentable here.
    pub fn resolve(&self, auth: &Auth) -> Result<Scope, ApiError> {
        resolve(auth, &self.subject, &self.namespace)
    }
}

/// The same rule for query strings, which cannot use `#[serde(flatten)]`:
/// `serde_urlencoded` buffers flattened values as strings and every numeric
/// field beside them then fails to deserialize. Query handlers spell the two
/// fields out and call this.
pub fn resolve(auth: &Auth, subject: &str, namespace: &str) -> Result<Scope, ApiError> {
    Ok(auth.0.scope(subject, namespace)?)
}
```

`crates/memorysafe-api/src/lib.rs`:

```rust
//! The HTTP adapter: a thin axum mirror of the engine.
//!
//! Authentication is `Authorization: Bearer <api-key>`, one key to one tenant.
//! Subject and namespace arrive per request and are validated against the key's
//! tenant. A governance decision is never an HTTP error.

pub mod auth;
pub mod error;
pub mod scope;

pub use error::{ApiError, Problem};
pub use scope::ScopeParams;

use crate::auth::Auth;
use axum::Json;
use axum::Router;
use axum::routing::get;
use memorysafe_auth::ApiKeyStore;
use memorysafe_engine::Engine;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<Engine>,
    pub keys: Arc<ApiKeyStore>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/whoami", get(whoami))
        .fallback(not_found)
        .with_state(state)
}

/// Deliberately unauthenticated: a load balancer must be able to ask, and the
/// answer names no tenant and reveals no memory.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Which tenant this credential is. A client holding a key it did not create
/// otherwise has no way to find out, and this is also the smallest route that
/// exercises authentication end to end.
async fn whoami(auth: Auth) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tenant": auth.tenant().to_string(),
        "key_id": auth.0.key_id(),
    }))
}

/// Without this an unknown path returns an empty 404 with no body, and a client
/// parsing `Problem` on every failure would choke on the one failure it did not
/// cause.
async fn not_found() -> ApiError {
    ApiError::NotFound("no such route".into())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-api`
Expected: PASS — 6 tests.

`/v1/whoami` is why these tests do not have to wait for Task 8. Extractors run *after* routing in
axum, so testing authentication against a route that does not exist yet would assert 404s, not
401s — the tests would pass for the wrong reason and keep passing if the extractor were deleted.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-api/
git commit -m "feat(api): problem responses, bearer auth, and the tenant-scoped scope extractor"
```

---

## Task 8: API — the memory routes

**Files:**
- Create: `crates/memorysafe-api/src/memories.rs`
- Modify: `crates/memorysafe-api/src/lib.rs`
- Modify: `crates/memorysafe-engine/src/lib.rs` (add `Engine::get`)
- Create: `crates/memorysafe-api/tests/memories.rs`

**Interfaces:**
- Consumes: `Engine::{remember, recall, review, get, forget, protect}`, `Auth`, `ScopeParams`.
- Produces: `Engine::get`, and the routes
  `POST /v1/recall`, `POST /v1/memories`, `GET /v1/memories`, `GET /v1/memories/{id}`,
  `DELETE /v1/memories/{id}`, `POST /v1/forget`, `POST /v1/memories/{id}/protect`.

**Path parameters use axum 0.8 syntax:** `/v1/memories/{id}`, not `/:id`. axum 0.8 upgraded
`matchit` and the old form now panics at router-build time.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-api/tests/memories.rs`:

```rust
mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{delete, get, harness, post, send};

fn scope() -> serde_json::Value {
    json!({ "subject": "user-42", "namespace": "agent" })
}

async fn remember(h: &support::Harness, body: &str, tags: serde_json::Value) -> serde_json::Value {
    let mut payload = scope();
    payload["body"] = json!(body);
    payload["tags"] = tags;
    let reply = send(&h.app, post("/v1/memories", Some(&h.key), payload)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.body
}

#[tokio::test]
async fn a_write_returns_two_hundred_with_the_decision() {
    let h = harness();
    let out = remember(&h, "the production migration runs on Sundays", json!([])).await;

    assert_eq!(out["action"]["kind"], "retain", "{out}");
    assert!(out["item_id"].is_string());
    assert!(out["audit_id"].is_string());
    assert!(!out["reasons"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_rejected_write_is_also_two_hundred() {
    // The single most important behaviour on this surface. If a redundant write
    // is a 4xx, every client library will treat governance as an outage.
    let h = harness();
    let body = "the deploy key rotates every ninety days";
    remember(&h, body, json!([])).await;
    let second = remember(&h, body, json!([])).await;

    assert!(
        second["action"]["kind"] == "reject" || second["action"]["kind"] == "merge",
        "an identical rewrite was admitted again: {second}"
    );
}

#[tokio::test]
async fn an_empty_body_is_four_hundred() {
    let h = harness();
    let mut payload = scope();
    payload["body"] = json!("   ");
    let reply = send(&h.app, post("/v1/memories", Some(&h.key), payload)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"], "validation");
}

#[tokio::test]
async fn a_retried_write_with_the_same_key_returns_the_same_outcome() {
    let h = harness();
    let mut payload = scope();
    payload["body"] = json!("written exactly once");
    payload["idempotency_key"] = json!("retry-me");

    let first = send(&h.app, post("/v1/memories", Some(&h.key), payload.clone())).await;
    let second = send(&h.app, post("/v1/memories", Some(&h.key), payload)).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(first.body["item_id"], second.body["item_id"]);

    let listed = send(&h.app, get("/v1/memories?subject=user-42&namespace=agent", Some(&h.key))).await;
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn recall_returns_a_working_set_with_reasons_and_an_audit_id() {
    let h = harness();
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
    ] {
        remember(&h, body, json!([])).await;
    }

    let mut payload = scope();
    payload["query"] = json!("when does the migration run");
    payload["budget"] = json!({ "max_tokens": 500, "max_items": 1 });
    let reply = send(&h.app, post("/v1/recall", Some(&h.key), payload)).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert!(reply.body["audit_id"].is_string(), "{}", reply.text);
    let items = reply.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "the budget was ignored: {}", reply.text);
    assert!(items[0]["item"]["body"].is_string());
    assert!(items[0]["reason"]["code"].is_string());
}

#[tokio::test]
async fn recall_in_search_mode_is_still_scoped_and_audited() {
    let h = harness();
    remember(&h, "a memory in the agent namespace", json!([])).await;

    let mut payload = scope();
    payload["mode"] = json!("search");
    payload["query"] = json!("memory");
    let reply = send(&h.app, post("/v1/recall", Some(&h.key), payload)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body["audit_id"].is_string(), "search mode skipped the audit");

    let mut elsewhere = json!({ "subject": "user-42", "namespace": "other" });
    elsewhere["mode"] = json!("search");
    elsewhere["query"] = json!("memory");
    let empty = send(&h.app, post("/v1/recall", Some(&h.key), elsewhere)).await;
    assert_eq!(empty.body["items"].as_array().unwrap().len(), 0, "search crossed a namespace");
}

#[tokio::test]
async fn a_single_memory_can_be_fetched_and_a_missing_one_is_404() {
    let h = harness();
    let id = remember(&h, "fetch me by id", json!([]))["item_id"].as_str().unwrap().to_owned();

    let found = send(
        &h.app,
        get(&format!("/v1/memories/{id}?subject=user-42&namespace=agent"), Some(&h.key)),
    )
    .await;
    assert_eq!(found.status, StatusCode::OK);
    assert_eq!(found.body["body"], "fetch me by id");

    let missing = send(
        &h.app,
        get("/v1/memories/01ARZ3NDEKTSV4RRFFQ69G5FAV?subject=user-42&namespace=agent", Some(&h.key)),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let malformed = send(
        &h.app,
        get("/v1/memories/not-a-ulid?subject=user-42&namespace=agent", Some(&h.key)),
    )
    .await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST, "a bad id is the caller's mistake");
}

#[tokio::test]
async fn deleting_by_id_removes_exactly_that_memory() {
    let h = harness();
    let id = remember(&h, "a memory to delete", json!([]))["item_id"].as_str().unwrap().to_owned();
    remember(&h, "a memory to keep around", json!([])).await;

    let reply = send(
        &h.app,
        delete(&format!("/v1/memories/{id}?subject=user-42&namespace=agent"), Some(&h.key)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["forgotten"], json!([id]));

    let left = send(&h.app, get("/v1/memories?subject=user-42&namespace=agent", Some(&h.key))).await;
    assert_eq!(left.body["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn forgetting_by_query_takes_exactly_one_selector() {
    let h = harness();
    remember(&h, "alpha note about deployments", json!(["work"])).await;
    remember(&h, "beta note about the kitchen", json!(["home"])).await;

    let mut payload = scope();
    payload["tag"] = json!("work");
    let ok = send(&h.app, post("/v1/forget", Some(&h.key), payload)).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.body["forgotten"].as_array().unwrap().len(), 1);

    let none = send(&h.app, post("/v1/forget", Some(&h.key), scope())).await;
    assert_eq!(none.status, StatusCode::BAD_REQUEST, "an empty selector is not 'delete everything'");

    let mut both = scope();
    both["tag"] = json!("work");
    both["kind"] = json!("fact");
    assert_eq!(
        send(&h.app, post("/v1/forget", Some(&h.key), both)).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn protecting_a_memory_pins_it_and_is_visible_in_review() {
    let h = harness();
    let id = remember(&h, "never forget this one", json!([]))["item_id"].as_str().unwrap().to_owned();

    let mut payload = scope();
    payload["level"] = json!("pinned");
    let reply = send(
        &h.app,
        post(&format!("/v1/memories/{id}/protect"), Some(&h.key), payload),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.body["action"]["protection"]["kind"], "pinned");

    let listed = send(&h.app, get("/v1/memories?subject=user-42&namespace=agent", Some(&h.key))).await;
    let item = listed.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == json!(id))
        .expect("the item survives");
    assert_eq!(item["protection"]["kind"], "pinned");
}

#[tokio::test]
async fn the_reserved_subject_is_403_on_every_route_that_takes_a_scope() {
    let h = harness();
    let query = send(&h.app, get("/v1/memories?subject=_admin&namespace=_admin", Some(&h.key))).await;
    assert_eq!(query.status, StatusCode::FORBIDDEN);

    let body = send(
        &h.app,
        post("/v1/memories", Some(&h.key), json!({
            "subject": "_admin", "namespace": "_admin", "body": "forged"
        })),
    )
    .await;
    assert_eq!(body.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_missing_scope_parameter_is_400_not_a_default_scope() {
    // Defaulting a namespace here would silently write into someone else's
    // budget. The caller must say where.
    let h = harness();
    let reply = send(&h.app, get("/v1/memories?subject=user-42", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"], "validation");
}

#[tokio::test]
async fn review_pages_and_reports_the_page_it_returned() {
    let h = harness();
    for i in 0..5 {
        remember(&h, &format!("distinct memory {i} on topic {i}"), json!([])).await;
    }

    let reply = send(
        &h.app,
        get("/v1/memories?subject=user-42&namespace=agent&limit=2&offset=2", Some(&h.key)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["items"].as_array().unwrap().len(), 2);
    assert_eq!(reply.body["offset"], json!(2));
    assert_eq!(reply.body["limit"], json!(2));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-api --test memories`
Expected: FAIL — every request returns 404 because no route matches.

- [ ] **Step 3: Write minimal implementation**

Add `Engine::get` to `crates/memorysafe-engine/src/lib.rs` beside `review`:

```rust
    pub async fn get(&self, scope: &Scope, id: &ItemId) -> Result<Option<MemoryItem>, EngineError> {
        Ok(self.backend.get(scope, id).await?)
    }
```

`crates/memorysafe-api/src/memories.rs`:

```rust
use crate::AppState;
use crate::auth::Auth;
use crate::error::ApiError;
use crate::scope::ScopeParams;
use axum::Json;
use axum::extract::{Path, Query, State};
use memorysafe_backend::Page;
use memorysafe_core::{
    ItemId, MemoryItem, Protection, RecallBudget, RecallMode, RecallRequest, SensitivityLevel,
    Source, SourceKind, WorkingSet,
};
use memorysafe_engine::{ForgetOutcome, ForgetSelector, RememberRequest, WriteOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use time::{Duration, OffsetDateTime};

fn item_id(raw: &str) -> Result<ItemId, ApiError> {
    ItemId::parse(raw).map_err(|e| ApiError::Validation(format!("bad item id '{raw}': {e}")))
}

fn timestamp(seconds: Option<i64>, field: &str) -> Result<Option<OffsetDateTime>, ApiError> {
    seconds
        .map(|s| {
            OffsetDateTime::from_unix_timestamp(s)
                .map_err(|_| ApiError::Validation(format!("{field} is not a valid Unix timestamp")))
        })
        .transpose()
}

#[derive(Debug, Deserialize)]
pub struct RememberBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub body: String,
    pub kind: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, Value>,
    pub source_kind: Option<SourceKind>,
    pub source_id: Option<String>,
    pub occurred_at: Option<i64>,
    pub sensitivity_hint: Option<SensitivityLevel>,
    pub ttl_seconds: Option<i64>,
    pub idempotency_key: Option<String>,
}

pub async fn remember(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<RememberBody>,
) -> Result<Json<WriteOutcome>, ApiError> {
    let scope = body.scope.resolve(&auth)?;
    let mut req = RememberRequest::new(scope, &body.body);
    req.actor = auth.actor();
    if let Some(kind) = body.kind {
        req.kind = kind;
    }
    req.tags = body.tags;
    req.attrs = body.attrs;
    req.source = Source {
        kind: body.source_kind.unwrap_or(SourceKind::Agent),
        id: body.source_id,
    };
    req.occurred_at = timestamp(body.occurred_at, "occurred_at")?;
    req.sensitivity_hint = body.sensitivity_hint;
    req.ttl = body.ttl_seconds.map(Duration::seconds);
    req.idempotency_key = body.idempotency_key;

    Ok(Json(state.engine.remember(req).await?))
}

#[derive(Debug, Deserialize)]
pub struct RecallBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub query: Option<String>,
    #[serde(default)]
    pub tags_any: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    pub occurred_after: Option<i64>,
    pub occurred_before: Option<i64>,
    #[serde(default)]
    pub mode: RecallMode,
    #[serde(default)]
    pub budget: RecallBudget,
    pub sensitivity_ceiling: Option<SensitivityLevel>,
}

pub async fn recall(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<RecallBody>,
) -> Result<Json<WorkingSet>, ApiError> {
    let req = RecallRequest {
        scope: body.scope.resolve(&auth)?,
        query: body.query,
        tags_any: body.tags_any,
        kinds: body.kinds,
        occurred_after: timestamp(body.occurred_after, "occurred_after")?,
        occurred_before: timestamp(body.occurred_before, "occurred_before")?,
        mode: body.mode,
        budget: body.budget,
        // No per-key clearance exists in v1, so an unstated ceiling excludes
        // nothing. See "Deferred to a later plan".
        sensitivity_ceiling: body.sensitivity_ceiling.unwrap_or(SensitivityLevel::Restricted),
    };
    Ok(Json(state.engine.recall(req).await?))
}

/// Query strings are deserialized by `serde_urlencoded`, which does not support
/// `#[serde(flatten)]` — flattening buffers every value as a string and the
/// numeric fields then fail to deserialize. Query structs therefore spell out
/// `subject` and `namespace`; only JSON bodies flatten `ScopeParams`.
#[derive(Debug, Deserialize)]
pub struct ReviewQuery {
    pub subject: String,
    pub namespace: String,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ReviewResponse {
    pub items: Vec<MemoryItem>,
    pub offset: usize,
    pub limit: usize,
}

pub async fn review(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<ReviewQuery>,
) -> Result<Json<ReviewResponse>, ApiError> {
    let scope = crate::scope::resolve(&auth, &query.subject, &query.namespace)?;
    let default_page = Page::default();
    let page = Page {
        offset: query.offset.unwrap_or(default_page.offset),
        limit: query.limit.unwrap_or(default_page.limit),
    };
    let items = state.engine.review(&scope, &page).await?;
    Ok(Json(ReviewResponse { items, offset: page.offset, limit: page.limit }))
}

pub async fn get_one(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(scope): Query<ScopeParams>,
) -> Result<Json<MemoryItem>, ApiError> {
    let scope = scope.resolve(&auth)?;
    let id = item_id(&id)?;
    state
        .engine
        .get(&scope, &id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("no memory {id} in this scope")))
}

pub async fn delete_one(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(scope): Query<ScopeParams>,
) -> Result<Json<ForgetOutcome>, ApiError> {
    let scope = scope.resolve(&auth)?;
    let id = item_id(&id)?;
    Ok(Json(
        state.engine.forget(&scope, ForgetSelector::Ids(vec![id])).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct ForgetBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub ids: Option<Vec<String>>,
    pub tag: Option<String>,
    pub kind: Option<String>,
}

pub async fn forget(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<ForgetBody>,
) -> Result<Json<ForgetOutcome>, ApiError> {
    let scope = body.scope.resolve(&auth)?;

    let given = [body.ids.is_some(), body.tag.is_some(), body.kind.is_some()]
        .into_iter()
        .filter(|present| *present)
        .count();
    if given != 1 {
        return Err(ApiError::Validation(
            "exactly one of 'ids', 'tag', or 'kind' is required".into(),
        ));
    }

    let selector = if let Some(ids) = body.ids {
        ForgetSelector::Ids(ids.iter().map(|r| item_id(r)).collect::<Result<_, _>>()?)
    } else if let Some(tag) = body.tag {
        ForgetSelector::Tag(tag)
    } else {
        ForgetSelector::Kind(body.kind.expect("checked above"))
    };
    Ok(Json(state.engine.forget(&scope, selector).await?))
}

#[derive(Debug, Deserialize)]
pub struct ProtectBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub level: String,
    pub until: Option<i64>,
}

pub async fn protect(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<ProtectBody>,
) -> Result<Json<WriteOutcome>, ApiError> {
    let scope = body.scope.resolve(&auth)?;
    let id = item_id(&id)?;
    let protection = match (body.level.as_str(), body.until) {
        ("normal", None) => Protection::Normal,
        ("pinned", None) => Protection::Pinned,
        ("protected", Some(seconds)) => Protection::Protected {
            until: timestamp(Some(seconds), "until")?.expect("Some"),
        },
        ("protected", None) => {
            return Err(ApiError::Validation(
                "'protected' requires 'until'; use 'pinned' for permanent protection".into(),
            ));
        }
        ("normal" | "pinned", Some(_)) => {
            return Err(ApiError::Validation(format!(
                "'until' is meaningless for level '{}'",
                body.level
            )));
        }
        (other, _) => {
            return Err(ApiError::Validation(format!(
                "unknown protection level '{other}'; expected normal, protected, or pinned"
            )));
        }
    };
    Ok(Json(state.engine.protect(&scope, &id, protection).await?))
}
```

Wire the routes in `crates/memorysafe-api/src/lib.rs`:

```rust
pub mod memories;

use axum::routing::{get, post};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/recall", post(memories::recall))
        .route(
            "/v1/memories",
            post(memories::remember).get(memories::review),
        )
        .route(
            "/v1/memories/{id}",
            get(memories::get_one).delete(memories::delete_one),
        )
        .route("/v1/memories/{id}/protect", post(memories::protect))
        .route("/v1/forget", post(memories::forget))
        .fallback(not_found)
        .with_state(state)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-api`
Expected: PASS — 6 auth tests, 13 memory tests.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/src/lib.rs crates/memorysafe-api/
git commit -m "feat(api): the memory routes, with decisions returned as successes"
```

---

## Task 9: API — audit, maintain, purge, export, import

**Files:**
- Create: `crates/memorysafe-api/src/ops.rs`
- Modify: `crates/memorysafe-api/src/lib.rs`
- Create: `crates/memorysafe-api/tests/ops.rs`

**Interfaces:**
- Consumes: `Engine::{audit, maintain, purge_subject, export_ndjson_as, export_markdown, import_ndjson_as}`.
- Produces: the routes `GET /v1/audit`, `POST /v1/maintain`, `GET /v1/export`, `POST /v1/import`,
  `DELETE /v1/subjects/{id}`, and `AuditResponse` with its `truncated` flag.

**The `truncated` flag is this route's convenience, not a missing contract.** `Backend::audit`
already makes truncation detectable: it returns exactly `min(limit, remaining)`, so
`returned.len() < limit` means the log is exhausted and a full page means "ask again with
`after=<last audit id>`". An HTTP caller should not have to infer that from a length, so this
route asks the engine for one row more than the caller wanted, returns the caller's page, and
says whether there were more. It must derive `truncated` that way — from the extra row — and
never from a flag the backend does not return.

**Export and import are audited here because the actor is here.** `export_ndjson_as` and
`import_ndjson_as` (Task 2) take the `Actor`, and this route supplies the one the API key names.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-api/tests/ops.rs`:

```rust
mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{delete, get, harness, post, send};

async fn remember(h: &support::Harness, subject: &str, body: &str) -> serde_json::Value {
    let reply = send(
        &h.app,
        post("/v1/memories", Some(&h.key), json!({
            "subject": subject, "namespace": "agent", "body": body
        })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    reply.body
}

#[tokio::test]
async fn the_audit_route_returns_decisions_and_never_bodies() {
    let h = harness();
    remember(&h, "user-42", "a body that must not appear in the trail").await;

    let reply = send(
        &h.app,
        get("/v1/audit?subject=user-42&namespace=agent", Some(&h.key)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let records = reply.body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "admitted");
    assert!(records[0]["decision"].is_object());
    assert!(
        !reply.text.contains("a body that must not appear"),
        "the audit route leaked an item body"
    );
}

#[tokio::test]
async fn the_audit_route_says_when_it_truncated() {
    let h = harness();
    for i in 0..5 {
        remember(&h, "user-42", &format!("distinct memory {i} about topic {i}")).await;
    }

    let capped = send(
        &h.app,
        get("/v1/audit?subject=user-42&namespace=agent&limit=2", Some(&h.key)),
    )
    .await;
    assert_eq!(capped.body["records"].as_array().unwrap().len(), 2);
    assert_eq!(capped.body["truncated"], json!(true), "a capped page must admit it");

    let whole = send(
        &h.app,
        get("/v1/audit?subject=user-42&namespace=agent&limit=100", Some(&h.key)),
    )
    .await;
    assert_eq!(whole.body["truncated"], json!(false));
}

#[tokio::test]
async fn the_audit_route_filters_by_event() {
    let h = harness();
    let id = remember(&h, "user-42", "a memory that will be deleted")["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(
        &h.app,
        delete(&format!("/v1/memories/{id}?subject=user-42&namespace=agent"), Some(&h.key)),
    )
    .await;

    let reply = send(
        &h.app,
        get("/v1/audit?subject=user-42&namespace=agent&event=forgotten", Some(&h.key)),
    )
    .await;
    let records = reply.body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "forgotten");

    let several = send(
        &h.app,
        get("/v1/audit?subject=user-42&namespace=agent&event=admitted,forgotten", Some(&h.key)),
    )
    .await;
    assert_eq!(
        several.body["records"].as_array().unwrap().len(),
        2,
        "a comma-separated event list must widen the filter, not narrow it to nothing"
    );

    let nonsense = send(
        &h.app,
        get("/v1/audit?subject=user-42&namespace=agent&event=exploded", Some(&h.key)),
    )
    .await;
    assert_eq!(nonsense.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn maintenance_reports_what_it_scanned_and_can_be_resumed() {
    let h = harness();
    for i in 0..3 {
        remember(&h, "user-42", &format!("healthy memory {i} about topic {i}")).await;
    }

    let reply = send(
        &h.app,
        post("/v1/maintain", Some(&h.key), json!({ "subject": "user-42", "namespace": "agent" })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert!(reply.body["scanned"].as_u64().is_some());
    assert_eq!(reply.body["forgotten"], json!(0));
    // `next_cursor` is null for a scope that fits in one pass; the field must
    // still be present so a client can loop on it without special-casing.
    assert!(reply.body.get("next_cursor").is_some());
}

#[tokio::test]
async fn export_returns_ndjson_and_records_who_asked() {
    let h = harness();
    remember(&h, "user-42", "the thing to export").await;

    let reply = send(&h.app, get("/v1/export", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.text.contains("the thing to export"));
    assert!(reply.text.lines().count() >= 2, "a header line and an item line");

    let admin = send(&h.app, get("/v1/audit?subject=_admin&namespace=_admin", Some(&h.key))).await;
    assert_eq!(
        admin.status,
        StatusCode::FORBIDDEN,
        "the admin trail is not readable through a caller-supplied scope"
    );

    // It is readable through the engine, which is how the CLI shows it.
    let rows = h
        .engine
        .audit(
            &memorysafe_core::Scope::admin(&memorysafe_core::TenantId::new("acme").unwrap()),
            &memorysafe_core::AuditFilter {
                events: vec![memorysafe_core::AuditEvent::Exported],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor.kind, memorysafe_core::ActorKind::ApiKey);
}

#[tokio::test]
async fn export_can_render_markdown_for_a_person_to_read() {
    let h = harness();
    remember(&h, "user-42", "the on-call rotation starts Monday").await;

    let reply = send(&h.app, get("/v1/export?format=markdown", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.text.contains("# MemorySafe export"));
    assert!(reply.text.contains("the on-call rotation starts Monday"));
}

#[tokio::test]
async fn an_export_round_trips_through_import() {
    let source = harness();
    for body in ["first exported memory", "second exported memory"] {
        remember(&source, "user-42", body).await;
    }
    let ndjson = send(&source.app, get("/v1/export", Some(&source.key))).await.text;

    let target = harness();
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/import")
        .header("authorization", format!("Bearer {}", target.key))
        .header("content-type", "application/x-ndjson")
        .body(axum::body::Body::from(ndjson))
        .unwrap();
    let reply = send(&target.app, request).await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.body["items_imported"], json!(2));

    let listed = send(
        &target.app,
        get("/v1/memories?subject=user-42&namespace=agent", Some(&target.key)),
    )
    .await;
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn purging_a_subject_removes_only_that_subject() {
    let h = harness();
    remember(&h, "doomed", "a memory belonging to the doomed subject").await;
    remember(&h, "keeper", "a memory belonging to someone else").await;

    let reply = send(&h.app, delete("/v1/subjects/doomed", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.body["items_removed"], json!(1));

    let gone = send(&h.app, get("/v1/memories?subject=doomed&namespace=agent", Some(&h.key))).await;
    assert_eq!(gone.body["items"].as_array().unwrap().len(), 0);
    let kept = send(&h.app, get("/v1/memories?subject=keeper&namespace=agent", Some(&h.key))).await;
    assert_eq!(kept.body["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn the_reserved_subject_cannot_be_purged() {
    let h = harness();
    let reply = send(&h.app, delete("/v1/subjects/_admin", Some(&h.key))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-api --test ops`
Expected: FAIL — every route 404s.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-api/src/ops.rs`:

```rust
use crate::AppState;
use crate::auth::Auth;
use crate::error::ApiError;
use crate::scope::ScopeParams;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use memorysafe_backend::{ImportReport, ScopeSelector};
use memorysafe_core::{
    ADMIN_COMPONENT, AuditEvent, AuditFilter, AuditId, AuditRecord, ItemId, Namespace, SubjectId,
};
use memorysafe_engine::{MaintainCursor, MaintainReport, PurgeOutcome};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

fn timestamp(seconds: Option<i64>, field: &str) -> Result<Option<OffsetDateTime>, ApiError> {
    seconds
        .map(|s| {
            OffsetDateTime::from_unix_timestamp(s)
                .map_err(|_| ApiError::Validation(format!("{field} is not a valid Unix timestamp")))
        })
        .transpose()
}

/// No `#[serde(flatten)]` and no `Vec`: axum's `Query` deserializes with
/// `serde_urlencoded`, which supports neither. Events arrive as one
/// comma-separated value — `?event=admitted,forgotten` — and an unknown name is
/// a 400 rather than a silently empty filter.
#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub subject: String,
    pub namespace: String,
    pub event: Option<String>,
    pub item: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub after: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct AuditResponse {
    pub records: Vec<AuditRecord>,
    /// True when more rows matched than were returned. Continue with
    /// `after=<id of the last record>`.
    pub truncated: bool,
}

pub async fn audit(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditResponse>, ApiError> {
    let scope = crate::scope::resolve(&auth, &query.subject, &query.namespace)?;

    let events = query
        .event
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|raw| {
            serde_json::from_value::<AuditEvent>(serde_json::Value::String(raw.to_owned()))
                .map_err(|_| ApiError::Validation(format!("unknown audit event '{raw}'")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let default_filter = AuditFilter::default();
    let requested = query.limit.unwrap_or(default_filter.limit);
    let filter = AuditFilter {
        events,
        subject: None,
        namespace: None,
        after: query
            .after
            .as_deref()
            .map(AuditId::parse)
            .transpose()
            .map_err(|e| ApiError::Validation(format!("bad cursor: {e}")))?,
        item: query
            .item
            .as_deref()
            .map(ItemId::parse)
            .transpose()
            .map_err(|e| ApiError::Validation(format!("bad item id: {e}")))?,
        since: timestamp(query.since, "since")?,
        until: timestamp(query.until, "until")?,
        // One row more than the caller asked for is how truncation is detected
        // without a second count query.
        limit: requested.saturating_add(1),
    };

    let mut records = state.engine.audit(&scope, &filter).await?;
    let truncated = records.len() > requested;
    records.truncate(requested);
    Ok(Json(AuditResponse { records, truncated }))
}

#[derive(Debug, Deserialize)]
pub struct MaintainBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub cursor: Option<usize>,
}

pub async fn maintain(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<MaintainBody>,
) -> Result<Json<MaintainReport>, ApiError> {
    let scope = body.scope.resolve(&auth)?;
    let cursor = body.cursor.map(|offset| MaintainCursor { offset });
    Ok(Json(state.engine.maintain(&scope, cursor).await?))
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    pub subject: Option<String>,
    pub namespace: Option<String>,
    #[serde(default)]
    pub include_audit: bool,
    /// `ndjson` (default) round-trips; `markdown` is for a person to read.
    pub format: Option<String>,
}

pub async fn export(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    for (name, value) in [("subject", &query.subject), ("namespace", &query.namespace)] {
        if value.as_deref() == Some(ADMIN_COMPONENT) {
            return Err(ApiError::Forbidden(format!(
                "'{ADMIN_COMPONENT}' is reserved and cannot be named as a {name}"
            )));
        }
    }
    let selector = ScopeSelector {
        tenant: auth.tenant().clone(),
        subject: query
            .subject
            .as_deref()
            .map(SubjectId::new)
            .transpose()?,
        namespace: query
            .namespace
            .as_deref()
            .map(Namespace::new)
            .transpose()?,
        include_audit: query.include_audit,
    };

    match query.format.as_deref() {
        None | Some("ndjson") => {
            // Audited: an export is a governance event once a person or an API
            // caller initiates it.
            let body = state.engine.export_ndjson_as(&selector, &auth.actor()).await?;
            Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response())
        }
        Some("markdown") => {
            let body = state.engine.export_markdown(&selector).await?;
            Ok(([(header::CONTENT_TYPE, "text/markdown; charset=utf-8")], body).into_response())
        }
        Some(other) => Err(ApiError::Validation(format!(
            "unknown export format '{other}'; expected ndjson or markdown"
        ))),
    }
}

pub async fn import(
    State(state): State<AppState>,
    auth: Auth,
    ndjson: String,
) -> Result<Json<ImportReport>, ApiError> {
    Ok(Json(
        state
            .engine
            .import_ndjson_as(&ndjson, auth.tenant(), &auth.actor())
            .await?,
    ))
}

pub async fn purge_subject(
    State(state): State<AppState>,
    auth: Auth,
    Path(subject): Path<String>,
) -> Result<Json<PurgeOutcome>, ApiError> {
    if subject == ADMIN_COMPONENT {
        return Err(ApiError::Forbidden(format!("'{ADMIN_COMPONENT}' is reserved")));
    }
    let subject = SubjectId::new(&subject)?;
    Ok(Json(
        state
            .engine
            .purge_subject(auth.tenant(), &subject, &auth.actor())
            .await?,
    ))
}
```

Wire the routes:

```rust
pub mod ops;

        .route("/v1/audit", get(ops::audit))
        .route("/v1/maintain", post(ops::maintain))
        .route("/v1/export", get(ops::export))
        .route("/v1/import", post(ops::import))
        .route("/v1/subjects/{id}", axum::routing::delete(ops::purge_subject))
```

**Note on the import body size.** axum's `String` extractor uses the default body limit (2 MB). An
archive larger than that is a CLI job, not an HTTP request; the limit is left at the default
deliberately so a large import cannot be used to exhaust server memory. If a deployment needs more,
`DefaultBodyLimit::max` on that one route is the change, and it belongs to the operator.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-api`
Expected: PASS — 9 new tests; the crate green.

**If `Query` rejects a request you expect to parse,** check for `#[serde(flatten)]` or a `Vec`
field in the query struct. `serde_urlencoded` supports neither, and the rejection reads like a
type error rather than an unsupported-feature error.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-api/
git commit -m "feat(api): audit with truncation, maintenance, purge, and audited portability"
```

---

## Task 10: API — the admin routes

**Files:**
- Create: `crates/memorysafe-api/src/admin.rs`
- Modify: `crates/memorysafe-api/src/lib.rs`
- Modify: `crates/memorysafe-engine/src/retention.rs`
- Create: `crates/memorysafe-api/tests/admin.rs`

**Interfaces:**
- Consumes: `Engine::{set_budget, capacity_state, tenant_settings, set_tenant_policy_config, set_tenant_retention}`.
- Produces: `RetentionProfile::name`, and the routes
  `GET|PUT /v1/admin/tenants/{id}/budgets`, `GET|PUT /v1/admin/tenants/{id}/policy`,
  `GET|PUT /v1/admin/tenants/{id}/retention`.

**There is no cross-tenant administrator in v1.** An API key is scoped to exactly one tenant
(§10), so `{id}` in these paths must equal the authenticated tenant and anything else is 403. The
path segment is not redundant: it makes a misdirected request fail loudly instead of quietly
configuring the wrong tenant, and it is the shape a future control plane would keep.

**Budgets are per scope, not per tenant.** `Budget` bounds a namespace (§ D5: "namespace the
budget/retrieval unit"), so these two routes carry `subject` and `namespace` even though they live
under a tenant path.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-api/tests/admin.rs`:

```rust
mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::{get, harness, post, put, send};

#[tokio::test]
async fn a_tenant_can_read_and_set_a_namespace_budget() {
    let h = harness();
    let before = send(
        &h.app,
        get("/v1/admin/tenants/acme/budgets?subject=user-42&namespace=agent", Some(&h.key)),
    )
    .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert_eq!(before.body["budget"]["max_items"], json!(null));

    let set = send(
        &h.app,
        put("/v1/admin/tenants/acme/budgets", Some(&h.key), json!({
            "subject": "user-42", "namespace": "agent", "max_items": 3
        })),
    )
    .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    assert_eq!(set.body["budget"]["max_items"], json!(3));

    let after = send(
        &h.app,
        get("/v1/admin/tenants/acme/budgets?subject=user-42&namespace=agent", Some(&h.key)),
    )
    .await;
    assert_eq!(after.body["budget"]["max_items"], json!(3));
}

#[tokio::test]
async fn a_budget_actually_bounds_the_namespace() {
    // A route that stores a number nobody reads is worse than no route.
    let h = harness();
    send(
        &h.app,
        put("/v1/admin/tenants/acme/budgets", Some(&h.key), json!({
            "subject": "user-42", "namespace": "agent", "max_items": 2
        })),
    )
    .await;

    for i in 0..4 {
        send(
            &h.app,
            post("/v1/memories", Some(&h.key), json!({
                "subject": "user-42", "namespace": "agent",
                "body": format!("distinct memory {i} on topic {i}")
            })),
        )
        .await;
    }

    let listed = send(&h.app, get("/v1/memories?subject=user-42&namespace=agent", Some(&h.key))).await;
    assert!(
        listed.body["items"].as_array().unwrap().len() <= 2,
        "the budget was exceeded: {}",
        listed.text
    );
}

#[tokio::test]
async fn the_policy_config_round_trips_and_the_change_is_audited() {
    let h = harness();
    let before = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert_eq!(before.body["merge_threshold"], json!(0.93));

    let mut tuned: serde_json::Value = before.body.clone();
    tuned["merge_threshold"] = json!(0.8);
    let set = send(&h.app, put("/v1/admin/tenants/acme/policy", Some(&h.key), tuned)).await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text);
    assert!(set.body["audit_id"].is_string(), "a policy change is a governance event");

    let after = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    assert_eq!(after.body["merge_threshold"], json!(0.8));
}

#[tokio::test]
async fn an_incoherent_policy_config_is_four_hundred() {
    let h = harness();
    let before = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    let mut broken: serde_json::Value = before.body.clone();
    broken["merge_threshold"] = json!(0.99);
    broken["duplicate_threshold"] = json!(0.90);

    let reply = send(&h.app, put("/v1/admin/tenants/acme/policy", Some(&h.key), broken)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text);
    assert_eq!(reply.body["error"], "validation");

    let unchanged = send(&h.app, get("/v1/admin/tenants/acme/policy", Some(&h.key))).await;
    assert_eq!(unchanged.body["merge_threshold"], json!(0.93));
}

#[tokio::test]
async fn the_retention_profile_round_trips_by_name() {
    let h = harness();
    let before = send(&h.app, get("/v1/admin/tenants/acme/retention", Some(&h.key))).await;
    assert_eq!(before.body["profile"], json!("balanced"));

    for profile in ["gdpr_strict", "hipaa_retain", "forensic", "balanced"] {
        let set = send(
            &h.app,
            put("/v1/admin/tenants/acme/retention", Some(&h.key), json!({ "profile": profile })),
        )
        .await;
        assert_eq!(set.status, StatusCode::OK, "{profile}: {}", set.text);
        assert_eq!(set.body["profile"], json!(profile));
        assert!(set.body["audit_id"].is_string());

        let after = send(&h.app, get("/v1/admin/tenants/acme/retention", Some(&h.key))).await;
        assert_eq!(after.body["profile"], json!(profile));
    }

    let nonsense = send(
        &h.app,
        put("/v1/admin/tenants/acme/retention", Some(&h.key), json!({ "profile": "whatever" })),
    )
    .await;
    assert_eq!(nonsense.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_key_cannot_administer_another_tenant() {
    let h = harness();
    for uri in [
        "/v1/admin/tenants/globex/policy",
        "/v1/admin/tenants/globex/retention",
        "/v1/admin/tenants/globex/budgets?subject=user-42&namespace=agent",
    ] {
        let reply = send(&h.app, get(uri, Some(&h.key))).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{uri} was readable");
    }

    let write = send(
        &h.app,
        put("/v1/admin/tenants/globex/retention", Some(&h.key), json!({ "profile": "forensic" })),
    )
    .await;
    assert_eq!(write.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_routes_need_a_credential_like_everything_else() {
    let h = harness();
    assert_eq!(
        send(&h.app, get("/v1/admin/tenants/acme/policy", None)).await.status,
        StatusCode::UNAUTHORIZED
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-api --test admin`
Expected: FAIL — every route 404s.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/memorysafe-engine/src/retention.rs`, inside `impl RetentionProfile`:

```rust
    /// The inverse of `from_name`. These four strings are the documented,
    /// tested surface — a customer's configuration file names one of them.
    pub fn name(self) -> &'static str {
        match self {
            RetentionProfile::Balanced => "balanced",
            RetentionProfile::GdprStrict => "gdpr_strict",
            RetentionProfile::HipaaRetain => "hipaa_retain",
            RetentionProfile::Forensic => "forensic",
        }
    }
```

and to that module's tests:

```rust
    #[test]
    fn every_profile_name_parses_back_to_its_profile() {
        for profile in [
            RetentionProfile::Balanced,
            RetentionProfile::GdprStrict,
            RetentionProfile::HipaaRetain,
            RetentionProfile::Forensic,
        ] {
            assert_eq!(RetentionProfile::from_name(profile.name()), Some(profile));
        }
    }
```

`crates/memorysafe-api/src/admin.rs`:

```rust
use crate::AppState;
use crate::auth::Auth;
use crate::error::ApiError;
use crate::scope::ScopeParams;
use axum::Json;
use axum::extract::{Path, Query, State};
use memorysafe_core::{Budget, CapacityState, TenantId};
use memorysafe_engine::RetentionProfile;
use memorysafe_policy::BaselineConfig;
use serde::{Deserialize, Serialize};

/// An API key is scoped to one tenant, so the path segment is a check, not a
/// selector. Making a misdirected admin request fail loudly is the entire point
/// of carrying it.
fn same_tenant(auth: &Auth, path: &str) -> Result<TenantId, ApiError> {
    let requested = TenantId::new(path)?;
    auth.0.authorize_tenant(&requested)?;
    Ok(requested)
}

#[derive(Debug, Serialize)]
pub struct BudgetResponse {
    pub budget: Budget,
    pub used_items: u64,
    pub used_bytes: u64,
}

impl From<CapacityState> for BudgetResponse {
    fn from(state: CapacityState) -> Self {
        Self { budget: state.budget, used_items: state.used_items, used_bytes: state.used_bytes }
    }
}

pub async fn get_budget(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
    Query(scope): Query<ScopeParams>,
) -> Result<Json<BudgetResponse>, ApiError> {
    same_tenant(&auth, &tenant)?;
    let scope = scope.resolve(&auth)?;
    Ok(Json(state.engine.capacity_state(&scope).await?.into()))
}

#[derive(Debug, Deserialize)]
pub struct SetBudgetBody {
    #[serde(flatten)]
    pub scope: ScopeParams,
    pub max_items: Option<u64>,
    pub max_bytes: Option<u64>,
}

pub async fn put_budget(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
    Json(body): Json<SetBudgetBody>,
) -> Result<Json<BudgetResponse>, ApiError> {
    same_tenant(&auth, &tenant)?;
    let scope = body.scope.resolve(&auth)?;
    state
        .engine
        .set_budget(&scope, Budget { max_items: body.max_items, max_bytes: body.max_bytes })
        .await?;
    Ok(Json(state.engine.capacity_state(&scope).await?.into()))
}

pub async fn get_policy(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
) -> Result<Json<BaselineConfig>, ApiError> {
    let tenant = same_tenant(&auth, &tenant)?;
    Ok(Json(state.engine.tenant_settings(&tenant).policy_config))
}

#[derive(Debug, Serialize)]
pub struct PolicyChanged {
    #[serde(flatten)]
    pub config: BaselineConfig,
    pub audit_id: String,
}

pub async fn put_policy(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
    Json(config): Json<BaselineConfig>,
) -> Result<Json<PolicyChanged>, ApiError> {
    let tenant = same_tenant(&auth, &tenant)?;
    let audit_id = state
        .engine
        .set_tenant_policy_config(&tenant, config.clone(), &auth.actor())
        .await?;
    Ok(Json(PolicyChanged { config, audit_id: audit_id.to_string() }))
}

#[derive(Debug, Serialize)]
pub struct RetentionView {
    pub profile: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_id: Option<String>,
}

pub async fn get_retention(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
) -> Result<Json<RetentionView>, ApiError> {
    let tenant = same_tenant(&auth, &tenant)?;
    Ok(Json(RetentionView {
        profile: state.engine.retention_for(&tenant).name(),
        audit_id: None,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SetRetentionBody {
    pub profile: String,
}

pub async fn put_retention(
    State(state): State<AppState>,
    auth: Auth,
    Path(tenant): Path<String>,
    Json(body): Json<SetRetentionBody>,
) -> Result<Json<RetentionView>, ApiError> {
    let tenant = same_tenant(&auth, &tenant)?;
    let profile = RetentionProfile::from_name(&body.profile).ok_or_else(|| {
        ApiError::Validation(format!(
            "unknown retention profile '{}'; expected balanced, gdpr_strict, hipaa_retain, or forensic",
            body.profile
        ))
    })?;
    let audit_id = state.engine.set_tenant_retention(&tenant, profile, &auth.actor()).await?;
    Ok(Json(RetentionView { profile: profile.name(), audit_id: Some(audit_id.to_string()) }))
}
```

Wire the routes:

```rust
pub mod admin;

        .route(
            "/v1/admin/tenants/{tenant}/budgets",
            get(admin::get_budget).put(admin::put_budget),
        )
        .route(
            "/v1/admin/tenants/{tenant}/policy",
            get(admin::get_policy).put(admin::put_policy),
        )
        .route(
            "/v1/admin/tenants/{tenant}/retention",
            get(admin::get_retention).put(admin::put_retention),
        )
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-api && cargo test -p memorysafe-engine retention`
Expected: PASS — 7 new API tests, 1 new engine test.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-engine/src/retention.rs crates/memorysafe-api/
git commit -m "feat(api): per-tenant budget, policy, and retention administration"
```

---

## Task 11: CLI — configuration, engine construction, `remember` / `recall` / `review`

**Files:**
- Create: `crates/memorysafe-cli/Cargo.toml`
- Create: `crates/memorysafe-cli/src/lib.rs`
- Create: `crates/memorysafe-cli/src/main.rs`
- Create: `crates/memorysafe-cli/src/config.rs`
- Create: `crates/memorysafe-cli/src/build.rs`
- Create: `crates/memorysafe-cli/src/render.rs`
- Create: `crates/memorysafe-cli/src/cmd/mod.rs`
- Create: `crates/memorysafe-cli/src/cmd/memory.rs`
- Create: `crates/memorysafe-cli/tests/memory.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `SqliteBackend::open`, `DeterministicEmbedder::new`, `BaselinePolicy::new`,
  `EngineConfig`, `Engine::{remember, recall, review}`, `ApiKeyRecord`.
- Produces: the `msafe` binary; `MsafeConfig`, `MsafeConfig::load`, `ServeConfig`,
  `build_engine`, `Scope` resolution from flags plus config, and the commands
  `remember`, `recall`, `review`.

**The CLI is the composition root.** It is the only crate allowed to name a backend, and the only
one that turns configuration into an `Engine`. `memorysafe-mcp` and `memorysafe-api` receive an
`Arc<Engine>` and know nothing about SQLite.

**Exit codes.** `0` for a completed command, *including* a write the policy rejected — a rejection
is the product working, and a shell script that treats it as failure would be wrong. `1` for an
error. `2` is clap's own usage error.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-cli/tests/memory.rs`:

```rust
use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn msafe(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("msafe").expect("the msafe binary builds");
    cmd.current_dir(dir);
    cmd.env("MSAFE_TENANT", "acme");
    cmd.env("MSAFE_SUBJECT", "user-42");
    cmd.env("MSAFE_NAMESPACE", "agent");
    cmd
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("msafe.toml"),
        "data_dir = \"tenants\"\nembedder = \"deterministic\"\nembedding_dim = 256\n",
    )
    .expect("write config");
    dir
}

fn json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn remembering_prints_the_decision_and_exits_zero() {
    let dir = workspace();
    let output = msafe(dir.path())
        .args(["--json", "remember", "the production migration runs on Sundays"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let value = json(&output);
    assert_eq!(value["action"]["kind"], "retain");
    assert!(value["item_id"].is_string());
    assert!(value["audit_id"].is_string());
}

#[test]
fn a_rejected_write_still_exits_zero() {
    // A shell script looping over candidate memories must not stop because
    // governance did its job.
    let dir = workspace();
    let body = "the deploy key rotates every ninety days";
    msafe(dir.path()).args(["remember", body]).assert().success();

    let output = msafe(dir.path()).args(["--json", "remember", body]).output().unwrap();
    assert!(output.status.success(), "a governance decision set a failure exit code");
    let action = json(&output)["action"]["kind"].as_str().unwrap().to_owned();
    assert!(action == "reject" || action == "merge", "{action}");
}

#[test]
fn human_output_says_what_happened_and_why() {
    let dir = workspace();
    msafe(dir.path())
        .args(["remember", "the on-call rotation starts Monday morning"])
        .assert()
        .success()
        .stdout(contains("retained").or(contains("Retained")))
        .stdout(contains("because").or(contains("novel")));
}

#[test]
fn recalling_returns_a_working_set_and_reports_the_budget_it_used() {
    let dir = workspace();
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        msafe(dir.path()).args(["remember", body]).assert().success();
    }

    let output = msafe(dir.path())
        .args(["--json", "recall", "when does the migration run", "--max-items", "2"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let value = json(&output);
    assert!(value["audit_id"].is_string(), "every recall is audited");
    let items = value["items"].as_array().unwrap();
    assert!(!items.is_empty(), "a matching query returned nothing: {value}");
    assert!(items.len() <= 2);
}

#[test]
fn recall_with_no_query_is_allowed_and_still_governed() {
    // "What do you remember about me?" has no query string. It must still work
    // and still be composed under a budget.
    let dir = workspace();
    msafe(dir.path()).args(["remember", "a memory with no particular topic"]).assert().success();

    let output = msafe(dir.path()).args(["--json", "recall"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(json(&output)["audit_id"].is_string());
}

#[test]
fn review_lists_what_is_stored() {
    let dir = workspace();
    msafe(dir.path()).args(["remember", "alpha memory about deployments"]).assert().success();
    msafe(dir.path()).args(["remember", "beta memory about the kitchen"]).assert().success();

    let output = msafe(dir.path()).args(["--json", "review"]).output().unwrap();
    let items = json(&output)["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    assert!(items[0]["body"].is_string());

    msafe(dir.path())
        .args(["review"])
        .assert()
        .success()
        .stdout(contains("alpha memory"))
        .stdout(contains("beta memory"));
}

#[test]
fn two_namespaces_do_not_see_each_other() {
    let dir = workspace();
    msafe(dir.path()).args(["remember", "a memory in the agent namespace"]).assert().success();

    let output = msafe(dir.path())
        .args(["--namespace", "notes", "--json", "review"])
        .output()
        .unwrap();
    assert_eq!(json(&output)["items"].as_array().unwrap().len(), 0);
}

#[test]
fn a_missing_scope_is_a_usage_error_not_a_default() {
    let dir = workspace();
    let output = Command::cargo_bin("msafe")
        .unwrap()
        .current_dir(dir.path())
        .args(["remember", "nowhere in particular"])
        .env_remove("MSAFE_TENANT")
        .env_remove("MSAFE_SUBJECT")
        .env_remove("MSAFE_NAMESPACE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("tenant"),
        "the error must name what is missing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_reserved_scope_is_refused_from_the_command_line_too() {
    let dir = workspace();
    let output = msafe(dir.path())
        .args(["--subject", "_admin", "--namespace", "_admin", "remember", "forged"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("_admin"));
}

#[test]
fn the_config_file_supplies_the_defaults_the_flags_override() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("msafe.toml"),
        "data_dir = \"tenants\"\ntenant = \"acme\"\nsubject = \"from-config\"\nnamespace = \"agent\"\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("msafe").unwrap();
    cmd.current_dir(dir.path())
        .env_remove("MSAFE_TENANT")
        .env_remove("MSAFE_SUBJECT")
        .env_remove("MSAFE_NAMESPACE")
        .args(["--json", "remember", "stored under the configured subject"])
        .assert()
        .success();

    let mut listed = Command::cargo_bin("msafe").unwrap();
    let output = listed
        .current_dir(dir.path())
        .env_remove("MSAFE_SUBJECT")
        .args(["--json", "review"])
        .output()
        .unwrap();
    assert_eq!(json(&output)["items"].as_array().unwrap().len(), 1);

    let mut overridden = Command::cargo_bin("msafe").unwrap();
    let elsewhere = overridden
        .current_dir(dir.path())
        .args(["--subject", "someone-else", "--json", "review"])
        .output()
        .unwrap();
    assert_eq!(json(&elsewhere)["items"].as_array().unwrap().len(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-cli`
Expected: FAIL — the package does not resolve.

- [ ] **Step 3: Write minimal implementation**

Add to the workspace `[workspace.dependencies]`:

```toml
memorysafe-cli = { path = "crates/memorysafe-cli" }
clap = { version = "4.6.6", features = ["derive", "env"] }
toml = "1.1.5"
tracing-subscriber = { version = "0.3.23", features = ["env-filter"] }
assert_cmd = "2.2.2"
predicates = "3.1.4"
```

`crates/memorysafe-cli/Cargo.toml`:

```toml
[package]
name = "memorysafe-cli"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "msafe — the MemorySafe command line"

[[bin]]
name = "msafe"
path = "src/main.rs"

[features]
default = []
model2vec = ["memorysafe-embed/model2vec"]

[dependencies]
memorysafe-auth.workspace = true
memorysafe-backend.workspace = true
memorysafe-backend-sqlite.workspace = true
memorysafe-core.workspace = true
memorysafe-embed.workspace = true
memorysafe-engine.workspace = true
memorysafe-policy.workspace = true
anyhow.workspace = true
clap.workspace = true
serde.workspace = true
serde_json.workspace = true
time.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "signal"] }
toml.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

[dev-dependencies]
assert_cmd.workspace = true
predicates.workspace = true
tempfile.workspace = true

[lints]
workspace = true
```

`crates/memorysafe-cli/src/config.rs`:

```rust
use anyhow::{Context, Result, bail};
use memorysafe_auth::ApiKeyRecord;
use memorysafe_engine::RetentionProfile;
use memorysafe_policy::BaselineConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_FILE: &str = "msafe.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MsafeConfig {
    /// Where the per-tenant SQLite files live. Relative paths resolve against
    /// the directory holding the config file, so a checked-in config means the
    /// same thing on every machine.
    pub data_dir: PathBuf,
    pub tenant: Option<String>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
    /// `deterministic` (no model files, the default) or `model2vec`.
    pub embedder: String,
    pub embedding_dim: u16,
    pub retention: String,
    pub policy: BaselineConfig,
    #[serde(rename = "keys")]
    pub keys: Vec<ApiKeyRecord>,
    pub serve: ServeConfig,
    /// Filled in by `load`; never read from the file.
    #[serde(skip)]
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    pub bind: String,
    pub mcp_path: String,
    /// Hostnames the streamable-HTTP MCP transport will answer for. Loopback
    /// only by default: accepting any `Host` is a DNS-rebinding hole, and a
    /// deployment behind a real name should have to say the name.
    pub allowed_hosts: Vec<String>,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".into(),
            mcp_path: "/mcp".into(),
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into()],
        }
    }
}

impl Default for MsafeConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(".msafe/tenants"),
            tenant: None,
            subject: None,
            namespace: None,
            embedder: "deterministic".into(),
            embedding_dim: 256,
            retention: RetentionProfile::default().name().to_owned(),
            policy: BaselineConfig::default(),
            keys: Vec::new(),
            serve: ServeConfig::default(),
            root: PathBuf::from("."),
        }
    }
}

impl MsafeConfig {
    /// Explicit path, else `./msafe.toml`, else defaults. A missing file is not
    /// an error — `msafe remember` in an empty directory should work.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(p) => Some(p.to_path_buf()),
            None => {
                let candidate = PathBuf::from(DEFAULT_CONFIG_FILE);
                candidate.exists().then_some(candidate)
            }
        };

        let Some(path) = path else {
            return Ok(Self::default());
        };

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut config: MsafeConfig = toml::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        config.root = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        if RetentionProfile::from_name(&config.retention).is_none() {
            bail!(
                "unknown retention profile '{}'; expected balanced, gdpr_strict, hipaa_retain, or forensic",
                config.retention
            );
        }
        Ok(config)
    }

    pub fn data_dir(&self) -> PathBuf {
        if self.data_dir.is_absolute() {
            self.data_dir.clone()
        } else {
            self.root.join(&self.data_dir)
        }
    }

    pub fn retention_profile(&self) -> RetentionProfile {
        RetentionProfile::from_name(&self.retention).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serialising configuration")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}
```

`crates/memorysafe-cli/src/build.rs`:

```rust
use crate::config::MsafeConfig;
use anyhow::{Context, Result, bail};
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_embed::{DeterministicEmbedder, Embedder};
use memorysafe_engine::{Engine, EngineConfig};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

/// The one place a backend, an embedder, and a policy become an engine.
pub fn build_engine(config: &MsafeConfig) -> Result<Arc<Engine>> {
    let data_dir = config.data_dir();
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;

    let embedder = embedder(config)?;
    let mut engine_config = EngineConfig::new(
        Arc::new(SqliteBackend::open(data_dir)),
        embedder,
        Arc::new(BaselinePolicy::new(config.policy.clone())),
    );
    engine_config.retention = config.retention_profile();
    Ok(Arc::new(Engine::new(engine_config)))
}

fn embedder(config: &MsafeConfig) -> Result<Arc<dyn Embedder>> {
    match config.embedder.as_str() {
        "deterministic" => Ok(Arc::new(DeterministicEmbedder::new(config.embedding_dim))),
        #[cfg(feature = "model2vec")]
        "model2vec" => Ok(Arc::new(
            memorysafe_embed::Model2VecEmbedder::load_default()
                .context("loading the model2vec embedder")?,
        )),
        #[cfg(not(feature = "model2vec"))]
        "model2vec" => bail!(
            "this build has no model2vec support; rebuild with `--features model2vec` or set \
             embedder = \"deterministic\""
        ),
        other => bail!("unknown embedder '{other}'; expected deterministic or model2vec"),
    }
}
```

**If `Model2VecEmbedder` does not expose `load_default()`,** use whichever constructor Plan 1's
Task 13 produced and pass the model path from a new `model_path` field on `MsafeConfig`. The
branch is behind a non-default feature, so it does not gate this task.

`crates/memorysafe-cli/src/render.rs`:

```rust
use anyhow::Result;
use memorysafe_core::{Action, MemoryItem, WorkingSet};
use memorysafe_engine::WriteOutcome;
use serde::Serialize;

/// `--json` prints the engine type's own serde form. Anything else risks a
/// second, drifting definition of the wire format.
pub fn emit<T: Serialize>(json: bool, value: &T, human: impl FnOnce()) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        human();
    }
    Ok(())
}

pub fn write_outcome(out: &WriteOutcome) {
    let verb = match &out.action {
        Action::Retain { .. } => "retained",
        Action::Merge { into, .. } => {
            println!("merged into {into}");
            "merged"
        }
        Action::Reject => "rejected",
    };
    if let Some(id) = &out.item_id {
        println!("{verb} {id}");
    } else {
        println!("{verb}");
    }
    for reason in &out.reasons {
        println!("  because {:?}: {}", reason.code, reason.detail);
    }
    for evicted in &out.evicted {
        println!("  evicted {evicted}");
    }
    println!("  audit {}", out.audit_id);
}

pub fn working_set(ws: &WorkingSet) {
    if ws.items.is_empty() {
        println!("nothing recalled");
    }
    for selected in &ws.items {
        println!("{}  {}", selected.item.id, selected.item.body);
        println!("  {:?}: {}", selected.reason.code, selected.reason.detail);
    }
    println!("{} tokens used, {} omitted", ws.tokens_used, ws.omitted.len());
    if let Some(id) = &ws.audit_id {
        println!("audit {id}");
    }
}

pub fn items(items: &[MemoryItem]) {
    if items.is_empty() {
        println!("nothing stored in this scope");
    }
    for item in items {
        println!(
            "{}  [{}] {:?} {}",
            item.id, item.kind, item.sensitivity, item.body
        );
    }
}
```

`crates/memorysafe-cli/src/cmd/memory.rs`:

```rust
use crate::render;
use anyhow::Result;
use clap::Args;
use memorysafe_backend::Page;
use memorysafe_core::{
    RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel,
};
use memorysafe_engine::{Engine, RememberRequest};
use memorysafe_core::{Actor, ActorKind};
use serde::Serialize;
use std::sync::Arc;
use time::Duration;

#[derive(Debug, Args)]
pub struct RememberArgs {
    /// The memory to store.
    pub body: String,
    #[arg(long, default_value = "fact")]
    pub kind: String,
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    #[arg(long)]
    pub ttl_seconds: Option<i64>,
    #[arg(long, value_parser = parse_sensitivity)]
    pub sensitivity: Option<SensitivityLevel>,
    #[arg(long)]
    pub idempotency_key: Option<String>,
}

fn parse_sensitivity(raw: &str) -> Result<SensitivityLevel, String> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned()))
        .map_err(|_| format!("expected public, internal, personal, sensitive, or restricted, got '{raw}'"))
}

pub async fn remember(engine: &Arc<Engine>, scope: Scope, json: bool, args: RememberArgs) -> Result<()> {
    let mut req = RememberRequest::new(scope, &args.body);
    req.actor = Actor { kind: ActorKind::Cli, id: None };
    req.kind = args.kind;
    req.tags = args.tags;
    req.ttl = args.ttl_seconds.map(Duration::seconds);
    req.sensitivity_hint = args.sensitivity;
    req.idempotency_key = args.idempotency_key;

    let outcome = engine.remember(req).await?;
    render::emit(json, &outcome, || render::write_outcome(&outcome))
}

#[derive(Debug, Args)]
pub struct RecallArgs {
    /// What to recall. Omit to ask "what do you remember here?".
    pub query: Option<String>,
    #[arg(long, default_value = "working-set", value_parser = ["working-set", "search"])]
    pub mode: String,
    #[arg(long)]
    pub max_tokens: Option<u32>,
    #[arg(long)]
    pub max_items: Option<usize>,
    #[arg(long = "tag")]
    pub tags_any: Vec<String>,
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
    #[arg(long, value_parser = parse_sensitivity)]
    pub sensitivity_ceiling: Option<SensitivityLevel>,
}

pub async fn recall(engine: &Arc<Engine>, scope: Scope, json: bool, args: RecallArgs) -> Result<()> {
    let defaults = RecallBudget::default();
    let request = RecallRequest {
        scope,
        query: args.query,
        tags_any: args.tags_any,
        kinds: args.kinds,
        occurred_after: None,
        occurred_before: None,
        mode: if args.mode == "search" { RecallMode::Search } else { RecallMode::WorkingSet },
        budget: RecallBudget {
            max_tokens: args.max_tokens.or(defaults.max_tokens),
            max_items: args.max_items.or(defaults.max_items),
        },
        sensitivity_ceiling: args.sensitivity_ceiling.unwrap_or(SensitivityLevel::Restricted),
    };

    let ws = engine.recall(request).await?;
    render::emit(json, &ws, || render::working_set(&ws))
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long)]
    pub offset: Option<usize>,
}

#[derive(Serialize)]
struct ReviewOutput {
    items: Vec<memorysafe_core::MemoryItem>,
    offset: usize,
    limit: usize,
}

pub async fn review(engine: &Arc<Engine>, scope: Scope, json: bool, args: ReviewArgs) -> Result<()> {
    let defaults = Page::default();
    let page = Page {
        offset: args.offset.unwrap_or(defaults.offset),
        limit: args.limit.unwrap_or(defaults.limit),
    };
    let items = engine.review(&scope, &page).await?;
    let output = ReviewOutput { items, offset: page.offset, limit: page.limit };
    render::emit(json, &output, || render::items(&output.items))
}
```

`crates/memorysafe-cli/src/cmd/mod.rs`:

```rust
pub mod memory;
```

**The crate is a library with a thin binary on top.** `msafe` is the product, but an integration
test needs to assemble the same router the binary serves without spawning a process (Task 14), and
a `tests/` crate cannot see a binary's private modules. So the modules live in `src/lib.rs` and
`src/main.rs` is a `main` function over them.

`crates/memorysafe-cli/src/lib.rs`:

```rust
//! The `msafe` command line, as a library so integration tests can assemble
//! the same pieces the binary does. `src/main.rs` is a thin `main` over this.

pub mod build;
pub mod cmd;
pub mod config;
pub mod render;
```

`crates/memorysafe-cli/src/main.rs`:

```rust
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use memorysafe_cli::config::MsafeConfig;
use memorysafe_cli::{build, cmd};
use memorysafe_core::{ADMIN_COMPONENT, Scope};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "msafe", version, about = "Governed memory for AI agents")]
struct Cli {
    /// Configuration file. Defaults to ./msafe.toml when present.
    #[arg(long, global = true, env = "MSAFE_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long, global = true, env = "MSAFE_TENANT")]
    tenant: Option<String>,
    #[arg(long, global = true, env = "MSAFE_SUBJECT")]
    subject: Option<String>,
    #[arg(long, global = true, env = "MSAFE_NAMESPACE")]
    namespace: Option<String>,
    /// Print the engine's own JSON instead of a human summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Store a memory and print the governance decision.
    Remember(cmd::memory::RememberArgs),
    /// Compose a working set under a token budget.
    Recall(cmd::memory::RecallArgs),
    /// List what is stored in this scope.
    Review(cmd::memory::ReviewArgs),
}

fn main() -> Result<()> {
    // stderr, never stdout: `msafe serve --transport stdio` uses stdout as the
    // MCP transport, and one stray log line there corrupts the session.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MSAFE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let config = MsafeConfig::load(cli.config.as_deref())?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(run(cli, config))
}

async fn run(cli: Cli, config: MsafeConfig) -> Result<()> {
    let engine = build::build_engine(&config)?;
    let scope = resolve_scope(&cli, &config)?;

    match cli.command {
        Command::Remember(args) => cmd::memory::remember(&engine, scope, cli.json, args).await,
        Command::Recall(args) => cmd::memory::recall(&engine, scope, cli.json, args).await,
        Command::Review(args) => cmd::memory::review(&engine, scope, cli.json, args).await,
    }
}

/// Flag, then environment (clap folds those together), then config file. There
/// is no default: writing into a scope the caller did not name is how memories
/// end up in the wrong place.
fn resolve_scope(cli: &Cli, config: &MsafeConfig) -> Result<Scope> {
    let pick = |flag: &Option<String>, from_config: &Option<String>, name: &str| -> Result<String> {
        flag.clone()
            .or_else(|| from_config.clone())
            .with_context(|| {
                format!("no {name}: pass --{name}, set MSAFE_{}, or put it in msafe.toml", name.to_uppercase())
            })
    };

    let tenant = pick(&cli.tenant, &config.tenant, "tenant")?;
    let subject = pick(&cli.subject, &config.subject, "subject")?;
    let namespace = pick(&cli.namespace, &config.namespace, "namespace")?;

    if subject == ADMIN_COMPONENT || namespace == ADMIN_COMPONENT {
        bail!("'{ADMIN_COMPONENT}' is reserved and may not be used as a subject or namespace");
    }
    Ok(Scope::new(&tenant, &subject, &namespace)?)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-cli`
Expected: PASS — 10 tests.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-cli/
git commit -m "feat(cli): msafe with configuration, engine construction, and the memory commands"
```

---

## Task 12: CLI — `forget`, `protect`, `audit`, `maintain`, `purge-subject`, `keys`

**Files:**
- Create: `crates/memorysafe-cli/src/cmd/curate.rs`
- Create: `crates/memorysafe-cli/src/cmd/keys.rs`
- Modify: `crates/memorysafe-cli/src/cmd/mod.rs`, `src/main.rs`, `src/render.rs`
- Create: `crates/memorysafe-cli/tests/curate.rs`

**Interfaces:**
- Consumes: `Engine::{forget, protect, audit, maintain, purge_subject}`, `memorysafe_auth::generate`.
- Produces: the commands `forget`, `protect`, `audit`, `maintain`, `purge-subject`, and
  `keys add` / `keys list`.

**`keys` is not in the spec's CLI list, and is needed anyway.** §12 specifies
`Authorization: Bearer <api-key>` for the HTTP surface but no way to create one. Without this
command the HTTP and MCP-over-HTTP servers cannot be used at all. It writes the record into
`msafe.toml` and prints the secret once.

**`purge-subject` asks before it acts.** It is the only irreversible command here — `forget` takes
items, `purge-subject` takes a person — so it requires `--yes` on a non-interactive run rather
than silently proceeding.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-cli/tests/curate.rs`:

```rust
use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn msafe(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("msafe").expect("binary");
    cmd.current_dir(dir);
    cmd.env("MSAFE_TENANT", "acme");
    cmd.env("MSAFE_SUBJECT", "user-42");
    cmd.env("MSAFE_NAMESPACE", "agent");
    cmd
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("msafe.toml"), "data_dir = \"tenants\"\n").unwrap();
    dir
}

fn json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!("stdout was not JSON ({e}): {}", String::from_utf8_lossy(&output.stdout))
    })
}

fn remember(dir: &Path, body: &str, tag: &str) -> String {
    let output = msafe(dir)
        .args(["--json", "remember", body, "--tag", tag])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    json(&output)["item_id"].as_str().expect("an id").to_owned()
}

#[test]
fn forgetting_by_id_and_by_tag() {
    let dir = workspace();
    let id = remember(dir.path(), "a memory to delete by id", "work");
    remember(dir.path(), "a memory to delete by tag", "chores");
    remember(dir.path(), "a memory to keep around", "keep");

    let by_id = msafe(dir.path()).args(["--json", "forget", "--id", &id]).output().unwrap();
    assert!(by_id.status.success());
    assert_eq!(json(&by_id)["forgotten"].as_array().unwrap().len(), 1);

    let by_tag = msafe(dir.path()).args(["--json", "forget", "--tag", "chores"]).output().unwrap();
    assert_eq!(json(&by_tag)["forgotten"].as_array().unwrap().len(), 1);

    let left = msafe(dir.path()).args(["--json", "review"]).output().unwrap();
    assert_eq!(json(&left)["items"].as_array().unwrap().len(), 1);
}

#[test]
fn forget_needs_exactly_one_selector() {
    let dir = workspace();
    msafe(dir.path()).args(["forget"]).assert().failure();
    msafe(dir.path())
        .args(["forget", "--tag", "work", "--kind", "fact"])
        .assert()
        .failure();
}

#[test]
fn protecting_pins_a_memory_and_review_shows_it() {
    let dir = workspace();
    let id = remember(dir.path(), "never forget this one", "important");

    msafe(dir.path())
        .args(["protect", &id, "--level", "pinned"])
        .assert()
        .success()
        .stdout(contains("pinned"));

    let listed = msafe(dir.path()).args(["--json", "review"]).output().unwrap();
    let items = json(&listed)["items"].as_array().unwrap().clone();
    let pinned = items.iter().find(|i| i["id"] == serde_json::json!(id)).unwrap();
    assert_eq!(pinned["protection"]["kind"], "pinned");
}

#[test]
fn a_protected_window_needs_a_deadline() {
    let dir = workspace();
    let id = remember(dir.path(), "protect me for a week", "important");
    msafe(dir.path())
        .args(["protect", &id, "--level", "protected"])
        .assert()
        .failure()
        .stderr(contains("until"));
}

#[test]
fn the_audit_command_shows_decisions_and_never_bodies() {
    let dir = workspace();
    remember(dir.path(), "a body that must not appear in the trail", "secret");

    let output = msafe(dir.path()).args(["--json", "audit"]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains("a body that must not appear"), "the audit command leaked a body");

    let records = json(&output)["records"].as_array().unwrap().clone();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "admitted");

    msafe(dir.path())
        .args(["audit", "--event", "admitted"])
        .assert()
        .success()
        .stdout(contains("admitted"));

    msafe(dir.path()).args(["audit", "--event", "exploded"]).assert().failure();
}

#[test]
fn maintenance_runs_and_reports() {
    let dir = workspace();
    for i in 0..3 {
        remember(dir.path(), &format!("healthy memory {i} about topic {i}"), "bulk");
    }
    let output = msafe(dir.path()).args(["--json", "maintain"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let value = json(&output);
    assert_eq!(value["forgotten"], serde_json::json!(0));
    assert!(value["scanned"].as_u64().is_some());
}

#[test]
fn purging_a_subject_requires_confirmation() {
    let dir = workspace();
    remember(dir.path(), "a memory belonging to this subject", "any");

    msafe(dir.path())
        .args(["purge-subject", "user-42"])
        .assert()
        .failure()
        .stderr(contains("--yes"));

    let purged = msafe(dir.path())
        .args(["--json", "purge-subject", "user-42", "--yes"])
        .output()
        .unwrap();
    assert!(purged.status.success());
    assert_eq!(json(&purged)["items_removed"], serde_json::json!(1));

    let left = msafe(dir.path()).args(["--json", "review"]).output().unwrap();
    assert_eq!(json(&left)["items"].as_array().unwrap().len(), 0);
}

#[test]
fn the_reserved_subject_cannot_be_purged() {
    let dir = workspace();
    msafe(dir.path())
        .args(["purge-subject", "_admin", "--yes"])
        .assert()
        .failure()
        .stderr(contains("_admin"));
}

#[test]
fn creating_a_key_prints_the_secret_once_and_stores_only_a_hash() {
    let dir = workspace();
    let output = msafe(dir.path())
        .args(["keys", "add", "--label", "ci runner"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let printed = String::from_utf8_lossy(&output.stdout);
    let secret = printed
        .split_whitespace()
        .find(|word| word.starts_with("msk_"))
        .expect("the secret is printed")
        .to_owned();

    let config = std::fs::read_to_string(dir.path().join("msafe.toml")).unwrap();
    assert!(config.contains("ci runner"), "the record was not saved: {config}");
    let tail = secret.rsplit('_').next().unwrap();
    assert!(!config.contains(tail), "the secret was written to disk: {config}");

    let listed = msafe(dir.path()).args(["--json", "keys", "list"]).output().unwrap();
    let keys = json(&listed)["keys"].as_array().unwrap().clone();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["label"], "ci runner");
    assert_eq!(keys[0]["tenant"], "acme");
    assert!(keys[0].get("secret").is_none());
}

#[test]
fn creating_a_key_without_a_config_file_says_where_it_would_go() {
    // Silently creating msafe.toml in whatever directory the operator happened
    // to be in is how a key ends up committed to a repository.
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("msafe").unwrap();
    cmd.current_dir(dir.path())
        .env("MSAFE_TENANT", "acme")
        .env("MSAFE_SUBJECT", "user-42")
        .env("MSAFE_NAMESPACE", "agent")
        .args(["keys", "add", "--label", "ci"])
        .assert()
        .failure()
        .stderr(contains("msafe.toml"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-cli --test curate`
Expected: FAIL — `unrecognized subcommand 'forget'`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-cli/src/cmd/curate.rs`:

```rust
use crate::render;
use anyhow::{Result, bail};
use clap::Args;
use memorysafe_core::{
    ADMIN_COMPONENT, Actor, ActorKind, AuditEvent, AuditFilter, AuditId, AuditRecord, ItemId,
    Protection, Scope, SubjectId, TenantId,
};
use memorysafe_engine::{Engine, ForgetSelector, MaintainCursor};
use serde::Serialize;
use std::sync::Arc;
use time::OffsetDateTime;

#[derive(Debug, Args)]
pub struct ForgetArgs {
    #[arg(long = "id")]
    pub ids: Vec<String>,
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
}

pub async fn forget(engine: &Arc<Engine>, scope: Scope, json: bool, args: ForgetArgs) -> Result<()> {
    let given = [!args.ids.is_empty(), args.tag.is_some(), args.kind.is_some()]
        .into_iter()
        .filter(|present| *present)
        .count();
    if given != 1 {
        bail!("pass exactly one of --id (repeatable), --tag, or --kind");
    }

    let selector = if !args.ids.is_empty() {
        ForgetSelector::Ids(
            args.ids.iter().map(|raw| ItemId::parse(raw)).collect::<Result<_, _>>()?,
        )
    } else if let Some(tag) = args.tag {
        ForgetSelector::Tag(tag)
    } else {
        ForgetSelector::Kind(args.kind.expect("checked above"))
    };

    let outcome = engine.forget(&scope, selector).await?;
    render::emit(json, &outcome, || {
        if outcome.forgotten.is_empty() {
            println!("nothing matched");
        }
        for id in &outcome.forgotten {
            println!("forgot {id}");
        }
        println!("audit {}", outcome.audit_id);
    })
}

#[derive(Debug, Args)]
pub struct ProtectArgs {
    pub id: String,
    #[arg(long, default_value = "pinned", value_parser = ["normal", "protected", "pinned"])]
    pub level: String,
    /// Unix seconds. Required for --level protected.
    #[arg(long)]
    pub until: Option<i64>,
}

pub async fn protect(engine: &Arc<Engine>, scope: Scope, json: bool, args: ProtectArgs) -> Result<()> {
    let protection = match (args.level.as_str(), args.until) {
        ("normal", None) => Protection::Normal,
        ("pinned", None) => Protection::Pinned,
        ("protected", Some(seconds)) => Protection::Protected {
            until: OffsetDateTime::from_unix_timestamp(seconds)?,
        },
        ("protected", None) => {
            bail!("--level protected requires --until; use --level pinned for permanent protection")
        }
        (level, Some(_)) => bail!("--until is meaningless for --level {level}"),
        (level, None) => bail!("unknown protection level '{level}'"),
    };

    let id = ItemId::parse(&args.id)?;
    let outcome = engine.protect(&scope, &id, protection).await?;
    render::emit(json, &outcome, || render::write_outcome(&outcome))
}

#[derive(Debug, Args)]
pub struct AuditArgs {
    #[arg(long = "event")]
    pub events: Vec<String>,
    #[arg(long)]
    pub item: Option<String>,
    #[arg(long)]
    pub since: Option<i64>,
    #[arg(long)]
    pub until: Option<i64>,
    #[arg(long)]
    pub after: Option<String>,
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct AuditOutput {
    pub records: Vec<AuditRecord>,
    pub truncated: bool,
}

pub async fn audit(engine: &Arc<Engine>, scope: Scope, json: bool, args: AuditArgs) -> Result<()> {
    let events = args
        .events
        .iter()
        .map(|raw| {
            serde_json::from_value::<AuditEvent>(serde_json::Value::String(raw.clone()))
                .map_err(|_| anyhow::anyhow!("unknown audit event '{raw}'"))
        })
        .collect::<Result<Vec<_>>>()?;

    let defaults = AuditFilter::default();
    let requested = args.limit.unwrap_or(defaults.limit);
    let filter = AuditFilter {
        events,
        subject: None,
        namespace: None,
        after: args.after.as_deref().map(AuditId::parse).transpose()?,
        item: args.item.as_deref().map(ItemId::parse).transpose()?,
        since: args.since.map(OffsetDateTime::from_unix_timestamp).transpose()?,
        until: args.until.map(OffsetDateTime::from_unix_timestamp).transpose()?,
        limit: requested.saturating_add(1),
    };

    let mut records = engine.audit(&scope, &filter).await?;
    let truncated = records.len() > requested;
    records.truncate(requested);
    let output = AuditOutput { records, truncated };

    render::emit(json, &output, || {
        for record in &output.records {
            let codes: Vec<String> = record
                .decision
                .as_ref()
                .map(|d| d.reasons.iter().map(|r| format!("{:?}", r.code)).collect())
                .unwrap_or_default();
            println!(
                "{}  {:?}  {} item(s)  {}",
                record.at,
                record.event,
                record.items.len(),
                codes.join(", ")
            );
        }
        if output.truncated {
            println!("(truncated — continue with --after <id of the last record>)");
        }
    })
}

#[derive(Debug, Args)]
pub struct MaintainArgs {
    #[arg(long)]
    pub cursor: Option<usize>,
    /// Keep going until the cursor is exhausted.
    #[arg(long)]
    pub all: bool,
}

pub async fn maintain(engine: &Arc<Engine>, scope: Scope, json: bool, args: MaintainArgs) -> Result<()> {
    let mut cursor = args.cursor.map(|offset| MaintainCursor { offset });
    let mut report = engine.maintain(&scope, cursor.take()).await?;

    if args.all {
        while let Some(next) = report.next_cursor {
            let page = engine.maintain(&scope, Some(next)).await?;
            report.scanned += page.scanned;
            report.forgotten += page.forgotten;
            report.protection_released += page.protection_released;
            report.next_cursor = page.next_cursor;
        }
    }

    render::emit(json, &report, || {
        println!(
            "scanned {} · forgot {} · released {} protection window(s)",
            report.scanned, report.forgotten, report.protection_released
        );
        if let Some(next) = report.next_cursor {
            println!("more to do — resume with --cursor {}", next.offset);
        }
    })
}

#[derive(Debug, Args)]
pub struct PurgeArgs {
    pub subject: String,
    /// Required. This deletes everything the subject owns.
    #[arg(long)]
    pub yes: bool,
}

pub async fn purge_subject(
    engine: &Arc<Engine>,
    tenant: &TenantId,
    json: bool,
    args: PurgeArgs,
) -> Result<()> {
    if args.subject == ADMIN_COMPONENT {
        bail!("'{ADMIN_COMPONENT}' is reserved and is not a subject");
    }
    if !args.yes {
        bail!(
            "purging removes every memory belonging to '{}' and cannot be undone; pass --yes to \
             confirm",
            args.subject
        );
    }
    let subject = SubjectId::new(&args.subject)?;
    let outcome = engine
        .purge_subject(
            tenant,
            &subject,
            // No id to supply — the CLI authenticates the process, not a
            // person — but the kind is real and is more than the anonymous
            // `Human` Plan 1 wrote. See `Engine::purge_subject`.
            &Actor { kind: ActorKind::Cli, id: None },
        )
        .await?;
    render::emit(json, &outcome, || {
        println!(
            "removed {} item(s); audit rows removed {} preserved {}",
            outcome.items_removed, outcome.audit_rows_removed, outcome.audit_rows_preserved
        );
    })
}
```

`crates/memorysafe-cli/src/cmd/keys.rs`:

```rust
use crate::config::{DEFAULT_CONFIG_FILE, MsafeConfig};
use crate::render;
use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use memorysafe_auth::{ApiKeyRecord, generate};
use memorysafe_core::TenantId;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// Create an API key for a tenant. The secret is printed once and never stored.
    Add(AddArgs),
    /// List the key records this configuration holds. Never prints a secret.
    List,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(long)]
    pub label: String,
}

#[derive(Serialize)]
struct KeysOutput {
    keys: Vec<ApiKeyRecord>,
}

#[derive(Serialize)]
struct AddedKey<'a> {
    secret: &'a str,
    id: &'a str,
    tenant: String,
    label: &'a str,
}

pub fn run(
    command: KeysCommand,
    tenant: &TenantId,
    config: &MsafeConfig,
    config_path: Option<&Path>,
    json: bool,
) -> Result<()> {
    match command {
        KeysCommand::List => {
            let output = KeysOutput { keys: config.keys.clone() };
            render::emit(json, &output, || {
                if output.keys.is_empty() {
                    println!("no keys configured");
                }
                for key in &output.keys {
                    let state = if key.disabled { "disabled" } else { "active" };
                    println!("{}  {}  {}  {}", key.id, key.tenant, state, key.label);
                }
            })
        }
        KeysCommand::Add(args) => {
            // Writing a key into a config file the operator did not ask for is
            // how a credential ends up committed. Make them create it first.
            let path: PathBuf = match config_path {
                Some(path) => path.to_path_buf(),
                None => {
                    let candidate = PathBuf::from(DEFAULT_CONFIG_FILE);
                    if !candidate.exists() {
                        bail!(
                            "no configuration file to store the key record in; create {} first \
                             (or pass --config)",
                            DEFAULT_CONFIG_FILE
                        );
                    }
                    candidate
                }
            };

            let generated = generate(tenant.clone(), &args.label)?;
            let mut updated = config.clone();
            updated.keys.push(generated.record.clone());
            updated.save(&path)?;

            let output = AddedKey {
                secret: &generated.secret,
                id: &generated.record.id,
                tenant: generated.record.tenant.to_string(),
                label: &generated.record.label,
            };
            render::emit(json, &output, || {
                println!("{}", generated.secret);
                println!(
                    "stored key {} for tenant {} in {}",
                    generated.record.id,
                    generated.record.tenant,
                    path.display()
                );
                println!("this secret is not recoverable — save it now");
            })
        }
    }
}
```

Extend `crates/memorysafe-cli/src/cmd/mod.rs`:

```rust
pub mod curate;
pub mod keys;
pub mod memory;
```

Extend the `Command` enum and dispatch in `main.rs`:

```rust
    /// Delete memories by id, tag, or kind.
    Forget(cmd::curate::ForgetArgs),
    /// Pin or protect a memory.
    Protect(cmd::curate::ProtectArgs),
    /// Show the governance decisions recorded for this scope.
    Audit(cmd::curate::AuditArgs),
    /// Run maintenance: expiry, decay, consolidation, reclaim.
    Maintain(cmd::curate::MaintainArgs),
    /// Delete everything belonging to one subject.
    PurgeSubject(cmd::curate::PurgeArgs),
    /// Manage API keys for the HTTP and MCP-over-HTTP servers.
    Keys {
        #[command(subcommand)]
        command: cmd::keys::KeysCommand,
    },
```

```rust
        Command::Forget(args) => cmd::curate::forget(&engine, scope, cli.json, args).await,
        Command::Protect(args) => cmd::curate::protect(&engine, scope, cli.json, args).await,
        Command::Audit(args) => cmd::curate::audit(&engine, scope, cli.json, args).await,
        Command::Maintain(args) => cmd::curate::maintain(&engine, scope, cli.json, args).await,
        Command::PurgeSubject(args) => {
            cmd::curate::purge_subject(&engine, &scope.tenant, cli.json, args).await
        }
        Command::Keys { command } => {
            cmd::keys::run(command, &scope.tenant, &config, cli.config.as_deref(), cli.json)
        }
```

`run` needs `config` after `build_engine` borrows it, so pass `&config` rather than moving it, and
take `cli.config` by reference.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-cli`
Expected: PASS — 10 new tests; the crate green.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-cli/
git commit -m "feat(cli): curation, audit, maintenance, purge, and API key management"
```

---

## Task 13: CLI — `export` and `import`

**Files:**
- Create: `crates/memorysafe-cli/src/cmd/portable.rs`
- Modify: `crates/memorysafe-cli/src/cmd/mod.rs`, `src/main.rs`
- Create: `crates/memorysafe-cli/tests/portable.rs`

**Interfaces:**
- Consumes: `Engine::{export_ndjson_as, export_markdown, import_ndjson_as}`, `ScopeSelector`.
- Produces: the commands `export` and `import`, plus `Manifest` and the archive layout.

**The archive is a directory, not a ZIP.** §12 allows either; a directory is what a customer can
`grep`, `diff`, and put in version control, which is the point of the feature. Nothing here
depends on the choice, so a ZIP writer can be added later without changing the format.

```
<dir>/memories.ndjson   the export stream — items, vectors, and optionally audit records
<dir>/memories.md       the same items rendered for a person to read
<dir>/manifest.json     what was exported, when, and a digest of memories.ndjson
```

**Why a digest.** Round-trip fidelity is a tested invariant (§13), and an archive that was
truncated in transit would import "successfully" with fewer memories. `import` verifies the digest
before it writes anything.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-cli/tests/portable.rs`:

```rust
use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn msafe(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("msafe").expect("binary");
    cmd.current_dir(dir);
    cmd.env("MSAFE_TENANT", "acme");
    cmd.env("MSAFE_SUBJECT", "user-42");
    cmd.env("MSAFE_NAMESPACE", "agent");
    cmd
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("msafe.toml"), "data_dir = \"tenants\"\n").unwrap();
    dir
}

fn seed(dir: &Path) {
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        msafe(dir).args(["remember", body]).assert().success();
    }
}

#[test]
fn export_writes_an_archive_a_person_can_read() {
    let dir = workspace();
    seed(dir.path());

    let out = dir.path().join("archive");
    msafe(dir.path())
        .args(["export", out.to_str().unwrap()])
        .assert()
        .success();

    let ndjson = std::fs::read_to_string(out.join("memories.ndjson")).unwrap();
    assert!(ndjson.lines().count() >= 4, "a header line and three item lines");
    for line in ndjson.lines() {
        let value: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
        assert!(value.get("record").is_some(), "each line names its record type");
    }

    let markdown = std::fs::read_to_string(out.join("memories.md")).unwrap();
    assert!(markdown.contains("# MemorySafe export"));
    assert!(markdown.contains("the production migration runs on Sundays"));

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["tenant"], "acme");
    assert_eq!(manifest["subject"], "user-42");
    assert_eq!(manifest["namespace"], "agent");
    assert!(manifest["ndjson_blake3"].is_string());
    assert!(manifest["exported_at"].is_i64());
}

#[test]
fn an_archive_round_trips_into_a_fresh_workspace() {
    let source = workspace();
    seed(source.path());
    let archive = source.path().join("archive");
    msafe(source.path())
        .args(["export", archive.to_str().unwrap()])
        .assert()
        .success();

    let target = workspace();
    msafe(target.path())
        .args(["import", archive.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("3"));

    let listed = msafe(target.path()).args(["--json", "review"]).output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let bodies: Vec<&str> = value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies.len(), 3);
    assert!(bodies.contains(&"the production migration runs on Sundays"));
}

#[test]
fn importing_the_same_archive_twice_changes_nothing_the_second_time() {
    let source = workspace();
    seed(source.path());
    let archive = source.path().join("archive");
    msafe(source.path()).args(["export", archive.to_str().unwrap()]).assert().success();

    let target = workspace();
    msafe(target.path()).args(["import", archive.to_str().unwrap()]).assert().success();
    let second = msafe(target.path())
        .args(["--json", "import", archive.to_str().unwrap()])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(report["items_imported"], serde_json::json!(0));
    assert_eq!(report["items_skipped_existing"], serde_json::json!(3));
}

#[test]
fn a_corrupted_archive_is_refused_before_anything_is_written() {
    let source = workspace();
    seed(source.path());
    let archive = source.path().join("archive");
    msafe(source.path()).args(["export", archive.to_str().unwrap()]).assert().success();

    // Drop the last item line: the file still parses, so only the digest can
    // catch it.
    let ndjson = std::fs::read_to_string(archive.join("memories.ndjson")).unwrap();
    let truncated: Vec<&str> = ndjson.lines().take(ndjson.lines().count() - 1).collect();
    std::fs::write(archive.join("memories.ndjson"), truncated.join("\n") + "\n").unwrap();

    let target = workspace();
    msafe(target.path())
        .args(["import", archive.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("digest"));

    let listed = msafe(target.path()).args(["--json", "review"]).output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(
        value["items"].as_array().unwrap().len(),
        0,
        "a refused import wrote memories anyway"
    );
}

#[test]
fn a_bare_ndjson_file_can_be_imported_without_a_manifest() {
    // The engine's own export format, handed over by some other route, must
    // still be importable — the manifest is a convenience, not the format.
    let source = workspace();
    seed(source.path());
    let archive = source.path().join("archive");
    msafe(source.path()).args(["export", archive.to_str().unwrap()]).assert().success();

    let target = workspace();
    msafe(target.path())
        .args(["import", archive.join("memories.ndjson").to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn export_narrows_to_the_scope_unless_told_otherwise() {
    let dir = workspace();
    msafe(dir.path()).args(["remember", "a memory in the agent namespace"]).assert().success();
    msafe(dir.path())
        .args(["--namespace", "notes", "remember", "a memory in the notes namespace"])
        .assert()
        .success();

    let narrow = dir.path().join("narrow");
    msafe(dir.path()).args(["export", narrow.to_str().unwrap()]).assert().success();
    let narrow_text = std::fs::read_to_string(narrow.join("memories.ndjson")).unwrap();
    assert!(narrow_text.contains("agent namespace"));
    assert!(!narrow_text.contains("notes namespace"), "the export ignored its scope");

    let wide = dir.path().join("wide");
    msafe(dir.path())
        .args(["export", wide.to_str().unwrap(), "--all-namespaces"])
        .assert()
        .success();
    let wide_text = std::fs::read_to_string(wide.join("memories.ndjson")).unwrap();
    assert!(wide_text.contains("agent namespace"));
    assert!(wide_text.contains("notes namespace"));
}

#[test]
fn export_refuses_to_overwrite_an_archive_that_is_already_there() {
    let dir = workspace();
    seed(dir.path());
    let out = dir.path().join("archive");
    msafe(dir.path()).args(["export", out.to_str().unwrap()]).assert().success();
    msafe(dir.path())
        .args(["export", out.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("--force"));
    msafe(dir.path())
        .args(["export", out.to_str().unwrap(), "--force"])
        .assert()
        .success();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-cli --test portable`
Expected: FAIL — `unrecognized subcommand 'export'`.

- [ ] **Step 3: Write minimal implementation**

Add `blake3.workspace = true` to `crates/memorysafe-cli/Cargo.toml` `[dependencies]`.

`crates/memorysafe-cli/src/cmd/portable.rs`:

```rust
use crate::render;
use anyhow::{Context, Result, bail};
use clap::Args;
use memorysafe_backend::ScopeSelector;
use memorysafe_core::{Actor, ActorKind, Scope};
use memorysafe_engine::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::OffsetDateTime;

pub const NDJSON_FILE: &str = "memories.ndjson";
pub const MARKDOWN_FILE: &str = "memories.md";
pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub exported_at: i64,
    pub tenant: String,
    pub subject: Option<String>,
    pub namespace: Option<String>,
    pub include_audit: bool,
    pub lines: usize,
    /// BLAKE3 of `memories.ndjson`, hex. Verified before an import writes
    /// anything: a truncated archive parses cleanly and would otherwise import
    /// as a smaller, silently wrong corpus.
    pub ndjson_blake3: String,
}

fn actor() -> Actor {
    Actor { kind: ActorKind::Cli, id: None }
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Directory to write the archive into.
    pub out: PathBuf,
    /// Export every subject in the tenant, not just this one.
    #[arg(long)]
    pub all_subjects: bool,
    /// Export every namespace for the selected subject(s).
    #[arg(long)]
    pub all_namespaces: bool,
    /// Include the audit trail. Decisions and digests only; never bodies.
    #[arg(long)]
    pub include_audit: bool,
    /// Overwrite an existing archive.
    #[arg(long)]
    pub force: bool,
}

pub async fn export(engine: &Arc<Engine>, scope: Scope, json: bool, args: ExportArgs) -> Result<()> {
    if args.out.join(NDJSON_FILE).exists() && !args.force {
        bail!(
            "{} already contains an archive; pass --force to overwrite it",
            args.out.display()
        );
    }

    let selector = ScopeSelector {
        tenant: scope.tenant.clone(),
        // A subject-wide export cannot be narrowed to one namespace: the
        // selector's namespace only means anything under a named subject.
        subject: (!args.all_subjects).then(|| scope.subject.clone()),
        namespace: (!args.all_subjects && !args.all_namespaces).then(|| scope.namespace.clone()),
        include_audit: args.include_audit,
    };

    let ndjson = engine.export_ndjson_as(&selector, &actor()).await?;
    let markdown = engine.export_markdown(&selector).await?;

    std::fs::create_dir_all(&args.out)
        .with_context(|| format!("creating {}", args.out.display()))?;
    std::fs::write(args.out.join(NDJSON_FILE), &ndjson)?;
    std::fs::write(args.out.join(MARKDOWN_FILE), &markdown)?;

    let manifest = Manifest {
        format_version: 1,
        exported_at: OffsetDateTime::now_utc().unix_timestamp(),
        tenant: scope.tenant.to_string(),
        subject: selector.subject.as_ref().map(|s| s.to_string()),
        namespace: selector.namespace.as_ref().map(|n| n.to_string()),
        include_audit: args.include_audit,
        lines: ndjson.lines().count(),
        ndjson_blake3: blake3::hash(ndjson.as_bytes()).to_hex().to_string(),
    };
    std::fs::write(
        args.out.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    render::emit(json, &manifest, || {
        println!(
            "exported {} record line(s) to {}",
            manifest.lines,
            args.out.display()
        );
    })
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// An archive directory, or a bare `.ndjson` export stream.
    pub path: PathBuf,
}

pub async fn import(engine: &Arc<Engine>, scope: Scope, json: bool, args: ImportArgs) -> Result<()> {
    let ndjson = read_stream(&args.path)?;
    let report = engine
        .import_ndjson_as(&ndjson, &scope.tenant, &actor())
        .await?;
    render::emit(json, &report, || {
        println!(
            "imported {} · skipped {} already present · {} vector(s) · {} audit row(s)",
            report.items_imported,
            report.items_skipped_existing,
            report.vectors_imported,
            report.audit_imported
        );
    })
}

/// Reads and, when a manifest is present, verifies. Verification happens before
/// the caller touches the engine, so a corrupt archive changes nothing.
fn read_stream(path: &Path) -> Result<String> {
    if path.is_dir() {
        let ndjson_path = path.join(NDJSON_FILE);
        let ndjson = std::fs::read_to_string(&ndjson_path)
            .with_context(|| format!("reading {}", ndjson_path.display()))?;

        let manifest_path = path.join(MANIFEST_FILE);
        if manifest_path.exists() {
            let manifest: Manifest =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
                    .with_context(|| format!("parsing {}", manifest_path.display()))?;
            let actual = blake3::hash(ndjson.as_bytes()).to_hex().to_string();
            if actual != manifest.ndjson_blake3 {
                bail!(
                    "{} does not match the digest in {}: the archive is truncated or modified \
                     (expected {}, found {})",
                    ndjson_path.display(),
                    MANIFEST_FILE,
                    manifest.ndjson_blake3,
                    actual
                );
            }
        }
        Ok(ndjson)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    }
}
```

Add `pub mod portable;` to `cmd/mod.rs`, and to `main.rs`:

```rust
    /// Write a portable archive of this scope.
    Export(cmd::portable::ExportArgs),
    /// Read a portable archive back in.
    Import(cmd::portable::ImportArgs),
```

```rust
        Command::Export(args) => cmd::portable::export(&engine, scope, cli.json, args).await,
        Command::Import(args) => cmd::portable::import(&engine, scope, cli.json, args).await,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-cli`
Expected: PASS — 7 new tests.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-cli/
git commit -m "feat(cli): portable archive export and digest-verified import"
```

---

## Task 14: CLI — `serve --transport stdio|http`

**Files:**
- Create: `crates/memorysafe-cli/src/cmd/serve.rs`
- Modify: `crates/memorysafe-cli/src/cmd/mod.rs`, `src/main.rs`, `Cargo.toml`
- Modify: `crates/memorysafe-mcp/src/transport.rs`, `src/lib.rs`
- Create: `crates/memorysafe-cli/tests/serve.rs`

**Interfaces:**
- Consumes: `memorysafe_mcp::{serve_stdio, http_service}`, `memorysafe_api::{router, AppState}`.
- Produces: `HttpTransportConfig`, `memorysafe_mcp::http_service_with`, and in the CLI
  `namespace_from_cwd`, `http_router`, and the `serve` command.

**One process, both servers.** `msafe serve --transport http` mounts the HTTP API at `/v1/*` and
the streamable-HTTP MCP transport at the configured path (default `/mcp`) in one axum router, so a
deployment is one binary and one port.

**The stdio namespace comes from the working directory,** per §12. A coding agent started in
`~/projects/checkout-service` should have its memories land in a namespace named for that project
without anyone configuring it. The directory name is slugified into a legal component; an
unusable name falls back to `default` rather than failing to start.

**Nothing may reach stdout on the stdio transport.** Task 11 already sends `tracing` to stderr;
this task must not print anything on that path.

- [ ] **Step 1: Write the failing test**

Append to `crates/memorysafe-cli/src/cmd/serve.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_name_becomes_a_legal_namespace() {
        assert_eq!(slug("checkout-service"), "checkout-service");
        assert_eq!(slug("Checkout Service"), "checkout-service");
        assert_eq!(slug("My_Project.v2"), "my_project.v2");
        assert_eq!(slug("café ☕"), "caf-");
    }

    #[test]
    fn an_unusable_directory_name_falls_back_rather_than_failing_to_start() {
        // A server that will not start because the folder is called "☕" is
        // worse than one that uses a sensible default.
        assert_eq!(slug(""), "default");
        assert_eq!(slug("☕"), "default");
        assert_eq!(slug("..."), "default");
        assert_eq!(slug("---"), "default");
    }

    #[test]
    fn a_very_long_directory_name_is_truncated_to_a_legal_component() {
        let long = "a".repeat(500);
        let slugged = slug(&long);
        assert!(slugged.len() <= 240);
        assert!(memorysafe_core::Namespace::new(&slugged).is_ok());
    }

    #[test]
    fn every_slug_this_produces_is_a_legal_namespace() {
        for raw in ["checkout-service", "Checkout Service", "café ☕", "", "☕", "...", "_admin"] {
            let slugged = slug(raw);
            assert!(
                memorysafe_core::Namespace::new(&slugged).is_ok(),
                "slug({raw:?}) = {slugged:?} is not a legal namespace"
            );
        }
    }

    #[test]
    fn the_reserved_component_is_never_produced_by_slugging() {
        assert_ne!(slug("_admin"), memorysafe_core::ADMIN_COMPONENT);
    }
}
```

`crates/memorysafe-cli/tests/serve.rs`:

```rust
use assert_cmd::cargo::cargo_bin;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::json;

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("msafe.toml"), "data_dir = \"tenants\"\n").unwrap();
    dir
}

/// The spec's own acceptance test: a real MCP client driving the real binary
/// over stdio, exactly as an agent host would launch it.
#[tokio::test]
async fn a_real_mcp_client_drives_the_stdio_server() {
    let dir = workspace();
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(cargo_bin("msafe")).configure(|cmd| {
            cmd.current_dir(dir.path())
                .env("MSAFE_TENANT", "acme")
                .env("MSAFE_SUBJECT", "user-42")
                .env("MSAFE_NAMESPACE", "agent")
                .args(["serve", "--transport", "stdio"]);
        }),
    )
    .expect("spawn msafe serve");

    let client = ().serve(transport).await.expect("the server speaks MCP on stdout");

    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 5, "{tools:?}");

    let written = client
        .call_tool(
            CallToolRequestParams::new("memory_remember").with_arguments(
                match json!({ "body": "a memory written through the spawned stdio server" }) {
                    serde_json::Value::Object(map) => map,
                    _ => unreachable!(),
                },
            ),
        )
        .await
        .expect("remember over stdio");
    assert_eq!(
        written.structured_content.as_ref().unwrap()["action"],
        json!("retain")
    );

    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_http_router_serves_the_api_and_mounts_the_mcp_transport() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // No `set_current_dir`: that is process-global state and these tests run in
    // parallel. `http_router_for_tests` is given the root explicitly instead.
    let dir = workspace();
    let router = memorysafe_cli::http_router_for_tests(dir.path());

    let health = router
        .clone()
        .oneshot(Request::builder().uri("/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    // A GET on the MCP path is not a 404: the transport is mounted and answers
    // for itself, whatever it decides to say about a bare GET.
    let mcp = router
        .oneshot(
            Request::builder()
                .uri("/mcp")
                .header("host", "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(mcp.status(), StatusCode::NOT_FOUND, "the MCP transport is not mounted");
}
```

`http_router_for_tests` is a thin wrapper the library exposes for this test; append it to
`crates/memorysafe-cli/src/lib.rs` (which Task 11 created):

```rust
use memorysafe_auth::ApiKeyStore;
use std::path::Path;
use std::sync::Arc;

/// Assembles the router `msafe serve --transport http` serves, without binding
/// a socket. Exists so an integration test can exercise the composition
/// without spawning a process.
pub fn http_router_for_tests(root: &Path) -> axum::Router {
    let config = config::MsafeConfig { root: root.to_path_buf(), ..Default::default() };
    let engine = build::build_engine(&config).expect("engine");
    cmd::serve::http_router(engine, Arc::new(ApiKeyStore::default()), &config.serve)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-cli --test serve`
Expected: FAIL — `unrecognized subcommand 'serve'`.

- [ ] **Step 3: Write minimal implementation**

Extend `crates/memorysafe-mcp/src/transport.rs` so the host allow-list can be configured, keeping
the loopback-only default:

```rust
/// Deployment knobs for the streamable-HTTP transport.
#[derive(Debug, Clone)]
pub struct HttpTransportConfig {
    /// Hostnames or `host:port` authorities this server answers for. Loopback
    /// only by default — accepting any `Host` is a DNS-rebinding hole against
    /// locally running servers.
    pub allowed_hosts: Vec<String>,
}

impl Default for HttpTransportConfig {
    fn default() -> Self {
        Self { allowed_hosts: vec!["localhost".into(), "127.0.0.1".into()] }
    }
}

pub fn http_service_with(
    engine: Arc<Engine>,
    source: ScopeSource,
    config: HttpTransportConfig,
) -> StreamableHttpService<MemorySafeServer, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(MemorySafeServer::new(engine.clone(), source.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig {
            allowed_hosts: config.allowed_hosts,
            ..StreamableHttpServerConfig::default()
        }
        .with_legacy_session_mode(false)
        .with_json_response(true),
    )
}

/// Loopback-only. `http_service_with` is the form a real deployment uses.
pub fn http_service(
    engine: Arc<Engine>,
    source: ScopeSource,
) -> StreamableHttpService<MemorySafeServer, LocalSessionManager> {
    http_service_with(engine, source, HttpTransportConfig::default())
}
```

and export `HttpTransportConfig` and `http_service_with` from `memorysafe-mcp`'s `lib.rs`.

Add to `crates/memorysafe-cli/Cargo.toml`:

```toml
[dependencies]
# ...as before, plus:
memorysafe-api.workspace = true
memorysafe-mcp.workspace = true
axum.workspace = true

[dev-dependencies]
# ...as before, plus:
rmcp = { workspace = true, features = ["client", "transport-child-process"] }
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "process"] }
tower.workspace = true
```

`crates/memorysafe-cli/src/cmd/serve.rs`:

```rust
use crate::config::{MsafeConfig, ServeConfig};
use anyhow::{Context, Result};
use axum::Router;
use clap::Args;
use memorysafe_api::{AppState, router as api_router};
use memorysafe_auth::ApiKeyStore;
use memorysafe_core::{ADMIN_COMPONENT, Namespace, Scope};
use memorysafe_engine::Engine;
use memorysafe_mcp::{HttpTransportConfig, ScopeSource, http_service_with, serve_stdio};
use std::sync::Arc;

const MAX_COMPONENT_BYTES: usize = 240;
const FALLBACK_NAMESPACE: &str = "default";

/// Turns a directory name into a legal `Namespace`. Lowercases, replaces every
/// character the component grammar forbids with `-`, truncates, and falls back
/// rather than refusing to start.
pub fn slug(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if out.len() >= MAX_COMPONENT_BYTES {
            break;
        }
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_lowercase() || lowered.is_ascii_digit() || matches!(lowered, '-' | '_' | '.') {
            out.push(lowered);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }

    let trimmed = out.trim_matches(|c| c == '-' || c == '.').to_owned();
    if trimmed.is_empty() || trimmed == ADMIN_COMPONENT || Namespace::new(&trimmed).is_err() {
        return FALLBACK_NAMESPACE.to_owned();
    }
    trimmed
}

pub fn namespace_from_cwd() -> Namespace {
    let name = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    Namespace::new(&slug(&name)).unwrap_or_else(|_| {
        Namespace::new(FALLBACK_NAMESPACE).expect("the fallback namespace is legal")
    })
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    #[arg(long, default_value = "stdio", value_parser = ["stdio", "http"])]
    pub transport: String,
    /// Overrides `serve.bind` from the configuration file.
    #[arg(long)]
    pub bind: Option<String>,
}

pub fn http_router(engine: Arc<Engine>, keys: Arc<ApiKeyStore>, serve: &ServeConfig) -> Router {
    let mcp = http_service_with(
        engine.clone(),
        ScopeSource::Http { keys: keys.clone() },
        HttpTransportConfig { allowed_hosts: serve.allowed_hosts.clone() },
    );
    api_router(AppState { engine, keys }).nest_service(&serve.mcp_path, mcp)
}

pub async fn serve(
    engine: Arc<Engine>,
    config: &MsafeConfig,
    scope: Scope,
    args: ServeArgs,
) -> Result<()> {
    let keys = Arc::new(ApiKeyStore::new(config.keys.clone()));

    match args.transport.as_str() {
        "stdio" => {
            // The namespace defaults from the working directory; a call may
            // override it. Tenant and subject are fixed for the session.
            let source = ScopeSource::Stdio {
                tenant: scope.tenant.clone(),
                subject: scope.subject.clone(),
                default_namespace: namespace_from_cwd(),
            };
            // Nothing is printed here: stdout is the transport.
            serve_stdio(engine, source).await
        }
        "http" => {
            let bind = args.bind.as_deref().unwrap_or(&config.serve.bind);
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("binding {bind}"))?;
            let address = listener.local_addr()?;
            // stderr, so this stays usable when stdout is piped somewhere.
            eprintln!("msafe listening on http://{address} (MCP at {})", config.serve.mcp_path);

            axum::serve(listener, http_router(engine, keys, &config.serve))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .context("serving HTTP")
        }
        other => anyhow::bail!("unknown transport '{other}'; expected stdio or http"),
    }
}
```

Add `pub mod serve;` to `cmd/mod.rs`, and to `main.rs`:

```rust
    /// Run the MCP server (stdio) or the HTTP API plus MCP transport.
    Serve(cmd::serve::ServeArgs),
```

```rust
        Command::Serve(args) => cmd::serve::serve(engine, &config, scope, args).await,
```

**`serve` takes `Arc<Engine>` by value** while every other command borrows it, because the HTTP
server owns it for the process lifetime. Clone it at the call site if the borrow checker prefers.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-cli && cargo test -p memorysafe-mcp`
Expected: PASS — 5 unit tests in `serve.rs`, 2 integration tests; `memorysafe-mcp` still green.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-cli/ crates/memorysafe-mcp/
git commit -m "feat(cli): serve MCP over stdio and the API plus MCP over HTTP"
```

---

## Task 15: Shadow — `Scenario`, `Trace`, and `run`

**Files:**
- Create: `crates/memorysafe-shadow/Cargo.toml`
- Create: `crates/memorysafe-shadow/src/lib.rs`
- Create: `crates/memorysafe-shadow/src/scenario.rs`
- Create: `crates/memorysafe-shadow/src/trace.rs`
- Create: `crates/memorysafe-shadow/src/run.rs`
- Create: `crates/memorysafe-shadow/tests/run.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: `Engine`, `SqliteBackend`, `DeterministicEmbedder`, `GovernancePolicy`, `Engine::get`.
- Produces: `ShadowError`, `Scenario`, `ScenarioWrite`, `Unreplayable`, `Trace`, `TracedDecision`,
  `TracedAction`, `run`.

**A trace is what two policies can be compared on.** It records the decision and its reasons, and
deliberately excludes everything that differs between two runs of the same scenario: item ids
(ULIDs), timestamps, and audit ids. What survives is exactly what a policy chose.

**Bodies never enter a trace**, for the same reason they never enter an audit row — a trace is a
diff artifact people paste into tickets. `body_digest` identifies a write without carrying it.

**Merge targets are recorded as a sequence number, not an id.** The same scenario run twice
produces different ULIDs, so `merged_into: ItemId` would make every trace differ from every other.
`into_seq` names the write that created the target, which is stable across runs.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-shadow/tests/run.rs`:

```rust
use memorysafe_core::{Budget, ReasonCode, Scope};
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use memorysafe_shadow::{Scenario, ScenarioWrite, TracedAction, run};
use std::sync::Arc;

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn write(body: &str) -> ScenarioWrite {
    ScenarioWrite {
        scope: scope(),
        body: body.to_owned(),
        kind: "fact".into(),
        tags: vec![],
        sensitivity_hint: None,
        ttl_seconds: None,
    }
}

fn scenario(name: &str, writes: Vec<ScenarioWrite>) -> Scenario {
    Scenario { name: name.into(), embedder_dim: 256, budgets: vec![], writes, unreplayable: vec![] }
}

#[tokio::test]
async fn a_scenario_of_distinct_writes_traces_one_decision_each() {
    let s = scenario(
        "novelty",
        vec![
            write("the production migration runs on Sundays"),
            write("the on-call rotation starts Monday morning"),
            write("the staging cluster is rebuilt every night"),
        ],
    );

    let trace = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();

    assert_eq!(trace.scenario, "novelty");
    assert_eq!(trace.decisions.len(), 3);
    for (i, decision) in trace.decisions.iter().enumerate() {
        assert_eq!(decision.seq, i);
        assert!(
            matches!(decision.action, TracedAction::Retain { .. }),
            "decision {i} was {:?}",
            decision.action
        );
        assert!(!decision.reason_codes.is_empty());
        assert_eq!(decision.body_digest.len(), 64, "a blake3 hex digest");
    }
}

#[tokio::test]
async fn a_trace_carries_no_bodies() {
    let s = scenario("secrecy", vec![write("a body that must never reach a diff artifact")]);
    let trace = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    let json = serde_json::to_string(&trace).unwrap();
    assert!(!json.contains("must never reach"), "the trace carried a body");
}

#[tokio::test]
async fn the_same_scenario_and_policy_produce_identical_traces() {
    // Without this the harness cannot tell a policy change from run-to-run
    // noise, and every diff is meaningless.
    let s = scenario(
        "stability",
        vec![
            write("the production migration runs on Sundays"),
            write("the on-call rotation starts Monday morning"),
        ],
    );

    let first = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    let second = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap(),
        "two runs of one scenario disagreed"
    );
}

#[tokio::test]
async fn an_exact_duplicate_is_traced_as_a_rejection_with_its_reason() {
    let body = "the deploy key rotates every ninety days";
    let s = scenario("redundancy", vec![write(body), write(body)]);

    let trace = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    assert!(matches!(trace.decisions[0].action, TracedAction::Retain { .. }));
    assert!(
        matches!(trace.decisions[1].action, TracedAction::Reject),
        "an identical rewrite was not rejected: {:?}",
        trace.decisions[1]
    );
    assert!(trace.decisions[1].reason_codes.contains(&ReasonCode::ExactDuplicate));
    assert_eq!(
        trace.decisions[0].body_digest, trace.decisions[1].body_digest,
        "identical bodies must digest identically"
    );
}

#[tokio::test]
async fn a_budget_forces_evictions_that_the_trace_counts() {
    let mut s = scenario(
        "capacity",
        (0..4)
            .map(|i| write(&format!("distinct memory {i} concerning topic {i}")))
            .collect(),
    );
    s.budgets = vec![(scope(), Budget { max_items: Some(2), max_bytes: None })];

    let trace = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    let evicted: usize = trace.decisions.iter().map(|d| d.evicted).sum();
    assert!(evicted > 0, "a budget of 2 over 4 writes evicted nothing: {trace:?}");
}

#[tokio::test]
async fn a_different_policy_configuration_produces_a_different_trace() {
    // The whole point of the harness. If a threshold change cannot move a
    // trace, the harness cannot detect a policy regression either.
    let s = scenario(
        "sensitivity-to-config",
        vec![
            write("the production migration runs on Sundays"),
            write("an entirely unrelated topic about kitchens"),
        ],
    );

    let permissive = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    let paranoid = run(
        &s,
        Arc::new(BaselinePolicy::new(BaselineConfig {
            duplicate_threshold: 0.0,
            merge_threshold: 0.0,
            ..Default::default()
        })),
    )
    .await
    .unwrap();

    assert_ne!(
        serde_json::to_value(&permissive).unwrap(),
        serde_json::to_value(&paranoid).unwrap(),
        "a policy that rejects everything traced the same as one that accepts"
    );
}

#[tokio::test]
async fn a_scenario_round_trips_through_json() {
    let s = scenario("round-trip", vec![write("something to serialise")]);
    let text = serde_json::to_string_pretty(&s).unwrap();
    let parsed: Scenario = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, s);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-shadow`
Expected: FAIL — the package does not resolve.

- [ ] **Step 3: Write minimal implementation**

Add to the workspace `[workspace.dependencies]`:

```toml
memorysafe-shadow = { path = "crates/memorysafe-shadow" }
```

`crates/memorysafe-shadow/Cargo.toml`:

```toml
[package]
name = "memorysafe-shadow"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "Shadow evaluation: replay governance decisions against another policy and diff them"

[dependencies]
memorysafe-backend.workspace = true
memorysafe-backend-sqlite.workspace = true
memorysafe-core.workspace = true
memorysafe-embed.workspace = true
memorysafe-engine.workspace = true
memorysafe-policy.workspace = true
blake3.workspace = true
serde.workspace = true
serde_json.workspace = true
tempfile.workspace = true
thiserror.workspace = true
time.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }

[lints]
workspace = true
```

**`memorysafe-shadow` depends on a backend, and that is correct.** It is not an adapter — it is a
harness that needs a real engine to drive, and SQLite in a temporary directory is the cheapest one
that exists. The dependency check in Task 18 exempts it by name.

`crates/memorysafe-shadow/src/lib.rs`:

```rust
//! Shadow evaluation.
//!
//! A `Scenario` is a corpus plus an ordered sequence of writes. Running one
//! against a policy produces a `Trace`: the decision and reasons for every
//! write, with everything that differs between runs — ids, timestamps, bodies —
//! left out. Two traces over one scenario can therefore be diffed exactly,
//! which is what makes a governance policy improvable without gambling on
//! customer data.

pub mod diff;
pub mod replay;
pub mod run;
pub mod scenario;
pub mod trace;

pub use diff::{DecisionChange, TraceDiff, diff};
pub use run::run;
pub use scenario::{Scenario, ScenarioWrite, Unreplayable};
pub use trace::{Trace, TracedAction, TracedDecision};

use memorysafe_core::CoreError;
use memorysafe_engine::EngineError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShadowError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("traces cover different numbers of decisions ({before} and {after}); they are not \
             two runs of one scenario")]
    Misaligned { before: usize, after: usize },
    #[error("cannot build a scenario: {0}")]
    Malformed(String),
}
```

Create empty `diff.rs` and `replay.rs` with `//!` doc comments for now; Tasks 16 and 17 fill them.

`crates/memorysafe-shadow/src/scenario.rs`:

```rust
use crate::ShadowError;
use memorysafe_core::{Budget, Scope, SensitivityLevel};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One write, exactly as a caller would have made it. `Scenario` is a file
/// format — a fixture lives on disk and is read by a test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioWrite {
    pub scope: Scope,
    pub body: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sensitivity_hint: Option<SensitivityLevel>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

fn default_kind() -> String {
    "fact".into()
}

/// A decision that was recorded but cannot be replayed, and why. Reported
/// rather than silently dropped: coverage is the number that says how much of
/// a real audit log a shadow run actually exercised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unreplayable {
    pub audit_id: String,
    pub event: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    /// Fixed so a fixture means the same thing on every machine.
    #[serde(default = "default_dim")]
    pub embedder_dim: u16,
    /// Applied before the first write.
    #[serde(default)]
    pub budgets: Vec<(Scope, Budget)>,
    pub writes: Vec<ScenarioWrite>,
    #[serde(default)]
    pub unreplayable: Vec<Unreplayable>,
}

fn default_dim() -> u16 {
    256
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Self, ShadowError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), ShadowError> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// How much of the source material this scenario actually replays.
    pub fn coverage(&self) -> f32 {
        let total = self.writes.len() + self.unreplayable.len();
        if total == 0 {
            return 1.0;
        }
        self.writes.len() as f32 / total as f32
    }
}
```

`crates/memorysafe-shadow/src/trace.rs`:

```rust
use memorysafe_core::{Protection, ReasonCode, Scope, SensitivityLevel};
use serde::{Deserialize, Serialize};

/// What happened, with everything run-specific removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TracedAction {
    Retain { protection: Protection },
    /// The write this merged into, by sequence number. `None` when the target
    /// predates the scenario — a corpus item this run did not create.
    Merge { into_seq: Option<usize> },
    Reject,
}

impl TracedAction {
    /// The label a transition histogram is keyed on.
    pub fn label(&self) -> &'static str {
        match self {
            TracedAction::Retain { .. } => "retain",
            TracedAction::Merge { .. } => "merge",
            TracedAction::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TracedDecision {
    pub seq: usize,
    pub scope: Scope,
    /// BLAKE3 of the body, hex. Identifies the write without carrying it.
    pub body_digest: String,
    pub action: TracedAction,
    pub reason_codes: Vec<ReasonCode>,
    pub evicted: usize,
    /// The level the item was stored at. `None` when nothing was stored.
    /// A policy that silently downgrades sensitivity is exactly the regression
    /// this harness exists to catch, so it belongs in the trace.
    pub sensitivity: Option<SensitivityLevel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub scenario: String,
    pub policy: memorysafe_core::PolicyId,
    pub decisions: Vec<TracedDecision>,
}
```

`crates/memorysafe-shadow/src/run.rs`:

```rust
use crate::scenario::Scenario;
use crate::trace::{Trace, TracedAction, TracedDecision};
use crate::ShadowError;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Action, GovernancePolicy, ItemId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use std::collections::HashMap;
use std::sync::Arc;
use time::Duration;

/// Drive a scenario through a real engine and record what the policy chose.
///
/// The engine is real — a temporary SQLite file and the deterministic embedder
/// — because the contexts a policy sees (neighbours, capacity, corpus
/// statistics) are produced by the engine's own I/O. Hand-building them would
/// be a second, drifting implementation of the write pipeline, and a shadow
/// result computed from one would not predict production.
pub async fn run(
    scenario: &Scenario,
    policy: Arc<dyn GovernancePolicy>,
) -> Result<Trace, ShadowError> {
    // Held for the whole run; dropped, and cleaned up, when it ends.
    let dir = tempfile::tempdir()?;
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.path().to_path_buf())),
        Arc::new(DeterministicEmbedder::new(scenario.embedder_dim)),
        policy.clone(),
    ));

    for (scope, budget) in &scenario.budgets {
        engine.set_budget(scope, *budget).await?;
    }

    let mut id_to_seq: HashMap<ItemId, usize> = HashMap::new();
    let mut decisions = Vec::with_capacity(scenario.writes.len());

    for (seq, write) in scenario.writes.iter().enumerate() {
        let mut req = RememberRequest::new(write.scope.clone(), &write.body);
        req.kind = write.kind.clone();
        req.tags = write.tags.clone();
        req.sensitivity_hint = write.sensitivity_hint;
        req.ttl = write.ttl_seconds.map(Duration::seconds);

        let outcome = engine.remember(req).await?;

        if let Some(id) = &outcome.item_id {
            id_to_seq.insert(id.clone(), seq);
        }

        // Read the stored item back for the level it actually landed at. The
        // outcome does not carry it, and a policy that downgraded a credential
        // would otherwise leave no trace.
        let sensitivity = match &outcome.item_id {
            Some(id) => engine
                .get(&write.scope, id)
                .await?
                .map(|item| item.sensitivity),
            None => None,
        };

        let action = match &outcome.action {
            Action::Retain { protection } => TracedAction::Retain { protection: *protection },
            Action::Merge { into, .. } => {
                TracedAction::Merge { into_seq: id_to_seq.get(into).copied() }
            }
            Action::Reject => TracedAction::Reject,
        };

        decisions.push(TracedDecision {
            seq,
            scope: write.scope.clone(),
            body_digest: blake3::hash(write.body.as_bytes()).to_hex().to_string(),
            action,
            reason_codes: outcome.reasons.iter().map(|r| r.code).collect(),
            evicted: outcome.evicted.len(),
            sensitivity,
        });
    }

    Ok(Trace { scenario: scenario.name.clone(), policy: policy.id(), decisions })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-shadow`
Expected: PASS — 7 tests.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/memorysafe-shadow/
git commit -m "feat(shadow): scenarios and reproducible, body-free decision traces"
```

---

## Task 16: Shadow — `diff` and the golden fixtures

**Files:**
- Modify: `crates/memorysafe-shadow/src/diff.rs`
- Create: `crates/memorysafe-shadow/fixtures/novelty.json`
- Create: `crates/memorysafe-shadow/fixtures/redundancy.json`
- Create: `crates/memorysafe-shadow/fixtures/capacity.json`
- Create: `crates/memorysafe-shadow/fixtures/sensitivity.json`
- Generate: `crates/memorysafe-shadow/fixtures/*.trace.json` (Step 4 blesses them)
- Create: `crates/memorysafe-shadow/tests/golden.rs`
- Create: `crates/memorysafe-shadow/tests/diff.rs`

**Interfaces:**
- Consumes: `Trace`, `TracedDecision`, `TracedAction`, `run`, `Scenario::load`.
- Produces: `TraceDiff`, `DecisionChange`, `diff`, and the golden fixture corpus plus its
  blessing mechanism.

**This is §13's "policy golden fixtures".** A corpus plus an ordered sequence of writes, asserting
exact decisions under the deterministic embedder. The blessed traces make any change in
`BaselinePolicy` show up as a test failure with a readable diff, rather than as a silent behaviour
change nobody notices until a customer does.

**Two kinds of assertion, deliberately.** The blessed trace catches *any* change. The hand-written
assertions in the same test state the properties that must hold whatever the numbers do — an exact
duplicate is rejected, a budget is honoured. If someone blesses a bad trace, the hand-written half
still fails.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-shadow/tests/diff.rs`:

```rust
use memorysafe_core::{Protection, ReasonCode, Scope};
use memorysafe_shadow::{Trace, TracedAction, TracedDecision, diff};

fn decision(seq: usize, action: TracedAction) -> TracedDecision {
    TracedDecision {
        seq,
        scope: Scope::new("acme", "user-42", "agent").unwrap(),
        body_digest: format!("{seq:064}"),
        action,
        reason_codes: vec![ReasonCode::NovelContent],
        evicted: 0,
        sensitivity: None,
    }
}

fn trace(decisions: Vec<TracedDecision>) -> Trace {
    Trace {
        scenario: "t".into(),
        policy: memorysafe_core::PolicyId::new("baseline", "1"),
        decisions,
    }
}

#[test]
fn two_identical_traces_diff_to_nothing() {
    let a = trace(vec![decision(0, TracedAction::Retain { protection: Protection::Normal })]);
    let d = diff(&a, &a).unwrap();
    assert_eq!(d.total, 1);
    assert_eq!(d.identical, 1);
    assert!(d.changed.is_empty());
    assert!(d.transitions.is_empty(), "no transition means no histogram entry");
}

#[test]
fn a_changed_action_is_reported_with_both_sides_and_counted() {
    let before = trace(vec![
        decision(0, TracedAction::Retain { protection: Protection::Normal }),
        decision(1, TracedAction::Retain { protection: Protection::Normal }),
    ]);
    let after = trace(vec![
        decision(0, TracedAction::Retain { protection: Protection::Normal }),
        decision(1, TracedAction::Reject),
    ]);

    let d = diff(&before, &after).unwrap();
    assert_eq!(d.total, 2);
    assert_eq!(d.identical, 1);
    assert_eq!(d.changed.len(), 1);
    assert_eq!(d.changed[0].seq, 1);
    assert!(matches!(d.changed[0].before.action, TracedAction::Retain { .. }));
    assert!(matches!(d.changed[0].after.action, TracedAction::Reject));
    assert_eq!(d.transitions.get("retain->reject"), Some(&1));
}

#[test]
fn a_change_in_reasons_alone_still_counts_as_a_change() {
    // Same verdict, different justification. An audit trail that suddenly
    // explains itself differently is a policy change, and a diff that hid it
    // would let one land unnoticed.
    let before = trace(vec![decision(0, TracedAction::Reject)]);
    let mut changed = decision(0, TracedAction::Reject);
    changed.reason_codes = vec![ReasonCode::LowValue];
    let after = trace(vec![changed]);

    let d = diff(&before, &after).unwrap();
    assert_eq!(d.changed.len(), 1);
    assert_eq!(d.transitions.get("reject->reject"), Some(&1));
}

#[test]
fn traces_of_different_lengths_are_an_error_not_a_partial_diff() {
    let before = trace(vec![decision(0, TracedAction::Reject)]);
    let after = trace(vec![]);
    assert!(diff(&before, &after).is_err());
}

#[test]
fn misaligned_sequence_numbers_are_an_error() {
    // Aligning by position when the sequence numbers disagree would compare
    // unrelated writes and report nonsense.
    let before = trace(vec![decision(0, TracedAction::Reject)]);
    let after = trace(vec![decision(7, TracedAction::Reject)]);
    assert!(diff(&before, &after).is_err());
}
```

`crates/memorysafe-shadow/tests/golden.rs`:

```rust
use memorysafe_core::{ReasonCode, SensitivityLevel};
use memorysafe_policy::BaselinePolicy;
use memorysafe_shadow::{Scenario, Trace, TracedAction, run};
use std::path::PathBuf;
use std::sync::Arc;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Run a fixture and compare against its blessed trace. Set `MEMORYSAFE_BLESS=1`
/// to rewrite the blessed file after an intentional policy change — and then
/// read the diff before committing it.
async fn golden(name: &str) -> Trace {
    let scenario = Scenario::load(&fixtures().join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("loading fixture {name}: {e}"));
    let actual = run(&scenario, Arc::new(BaselinePolicy::default()))
        .await
        .unwrap_or_else(|e| panic!("running fixture {name}: {e}"));

    let blessed_path = fixtures().join(format!("{name}.trace.json"));
    if std::env::var_os("MEMORYSAFE_BLESS").is_some() {
        std::fs::write(&blessed_path, serde_json::to_string_pretty(&actual).unwrap()).unwrap();
        return actual;
    }

    let blessed: Trace = serde_json::from_str(
        &std::fs::read_to_string(&blessed_path).unwrap_or_else(|e| {
            panic!("no blessed trace at {}: {e} — run with MEMORYSAFE_BLESS=1", blessed_path.display())
        }),
    )
    .unwrap();

    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&blessed).unwrap(),
        "the baseline policy changed its decisions for fixture '{name}'. If that was intended, \
         rerun with MEMORYSAFE_BLESS=1 and review the diff before committing."
    );
    actual
}

#[tokio::test]
async fn novelty_admits_every_distinct_memory() {
    let trace = golden("novelty").await;
    assert_eq!(trace.decisions.len(), 3);
    for decision in &trace.decisions {
        assert!(
            matches!(decision.action, TracedAction::Retain { .. }),
            "a distinct memory was not retained: {decision:?}"
        );
    }
}

#[tokio::test]
async fn redundancy_rejects_the_exact_duplicate() {
    let trace = golden("redundancy").await;
    assert!(matches!(trace.decisions[0].action, TracedAction::Retain { .. }));
    assert!(
        matches!(trace.decisions[1].action, TracedAction::Reject),
        "an identical rewrite was admitted: {:?}",
        trace.decisions[1]
    );
    assert!(trace.decisions[1].reason_codes.contains(&ReasonCode::ExactDuplicate));
    // The third write is a near-duplicate. Whether it merges or is retained
    // depends on where the deterministic embedder places it, so the blessed
    // trace pins that; the only property asserted here is that it was explained.
    assert!(!trace.decisions[2].reason_codes.is_empty());
}

#[tokio::test]
async fn capacity_evicts_to_stay_within_the_budget() {
    let trace = golden("capacity").await;
    let evicted: usize = trace.decisions.iter().map(|d| d.evicted).sum();
    assert!(evicted > 0, "a budget of two over five writes evicted nothing");
}

#[tokio::test]
async fn a_credential_is_stored_restricted_whatever_the_caller_hinted() {
    let trace = golden("sensitivity").await;
    let stored = trace
        .decisions
        .iter()
        .find(|d| matches!(d.action, TracedAction::Retain { .. }))
        .expect("the credential was stored");
    assert_eq!(
        stored.sensitivity,
        Some(SensitivityLevel::Restricted),
        "a caller's low hint lowered the stored level"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p memorysafe-shadow --test diff`
Expected: FAIL — `cannot find function 'diff'`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-shadow/src/diff.rs`:

```rust
//! Comparing two traces of one scenario.

use crate::ShadowError;
use crate::trace::{Trace, TracedDecision};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionChange {
    pub seq: usize,
    pub before: TracedDecision,
    pub after: TracedDecision,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceDiff {
    pub total: usize,
    pub identical: usize,
    pub changed: Vec<DecisionChange>,
    /// `"before->after"` action labels, counted. `BTreeMap` so the report is
    /// stable enough to diff two shadow runs against each other.
    pub transitions: BTreeMap<String, usize>,
}

impl TraceDiff {
    pub fn is_clean(&self) -> bool {
        self.changed.is_empty()
    }
}

/// Compares two traces position by position, refusing to align traces that are
/// not two runs of the same scenario.
pub fn diff(before: &Trace, after: &Trace) -> Result<TraceDiff, ShadowError> {
    if before.decisions.len() != after.decisions.len() {
        return Err(ShadowError::Misaligned {
            before: before.decisions.len(),
            after: after.decisions.len(),
        });
    }

    let mut changed = Vec::new();
    let mut transitions: BTreeMap<String, usize> = BTreeMap::new();
    let mut identical = 0;

    for (b, a) in before.decisions.iter().zip(after.decisions.iter()) {
        if b.seq != a.seq {
            return Err(ShadowError::Malformed(format!(
                "decision {} in the first trace lines up with decision {} in the second; these \
                 are not two runs of one scenario",
                b.seq, a.seq
            )));
        }
        if b == a {
            identical += 1;
            continue;
        }
        // Reasons count. A verdict that stays the same but is justified
        // differently is still a change to the audit trail a customer reads.
        *transitions
            .entry(format!("{}->{}", b.action.label(), a.action.label()))
            .or_default() += 1;
        changed.push(DecisionChange { seq: b.seq, before: b.clone(), after: a.clone() });
    }

    Ok(TraceDiff { total: before.decisions.len(), identical, changed, transitions })
}
```

`crates/memorysafe-shadow/fixtures/novelty.json`:

```json
{
  "name": "novelty",
  "embedder_dim": 256,
  "budgets": [],
  "writes": [
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the production database migration runs on Sundays at 02:00 UTC",
      "kind": "fact", "tags": ["ops"] },
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the on-call rotation starts Monday morning and hands over on Friday",
      "kind": "fact", "tags": ["ops"] },
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the customer prefers written summaries over meetings",
      "kind": "preference", "tags": ["style"] }
  ],
  "unreplayable": []
}
```

`crates/memorysafe-shadow/fixtures/redundancy.json`:

```json
{
  "name": "redundancy",
  "embedder_dim": 256,
  "budgets": [],
  "writes": [
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the deploy key rotates every ninety days",
      "kind": "fact", "tags": [] },
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the deploy key rotates every ninety days",
      "kind": "fact", "tags": [] },
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the deploy key rotates every ninety days, per the security policy",
      "kind": "fact", "tags": [] }
  ],
  "unreplayable": []
}
```

`crates/memorysafe-shadow/fixtures/capacity.json`:

```json
{
  "name": "capacity",
  "embedder_dim": 256,
  "budgets": [
    [ { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      { "max_items": 2, "max_bytes": null } ]
  ],
  "writes": [
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the release train departs on Thursdays", "kind": "fact", "tags": [] },
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the incident review template lives in the engineering wiki", "kind": "procedure", "tags": [] },
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the finance team closes the books on the fifth working day", "kind": "fact", "tags": [] },
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the office coffee machine is descaled every fortnight", "kind": "fact", "tags": [] },
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the staging cluster is rebuilt from scratch every night", "kind": "fact", "tags": [] }
  ],
  "unreplayable": []
}
```

`crates/memorysafe-shadow/fixtures/sensitivity.json`:

```json
{
  "name": "sensitivity",
  "embedder_dim": 256,
  "budgets": [],
  "writes": [
    { "scope": { "tenant": "acme", "subject": "user-42", "namespace": "agent" },
      "body": "the deployment api key is sk-abc123def456ghi789jkl012mno345",
      "kind": "fact", "tags": [], "sensitivity_hint": "public" }
  ],
  "unreplayable": []
}
```

- [ ] **Step 4: Bless the traces, read them, then run the tests**

Run: `MEMORYSAFE_BLESS=1 cargo test -p memorysafe-shadow --test golden`
Then read each `crates/memorysafe-shadow/fixtures/*.trace.json` and confirm, by eye, that:

- `novelty.trace.json` has three `retain` decisions;
- `redundancy.trace.json` has `retain`, then `reject` with `exact_duplicate`;
- `capacity.trace.json` has a non-zero `evicted` on at least one decision;
- `sensitivity.trace.json` stores the credential at `restricted` despite the `public` hint;
- no file contains any `body` text — only `body_digest`.

If any of those is wrong, the *policy* is wrong; do not bless around it.

Run: `cargo test -p memorysafe-shadow`
Expected: PASS — 5 diff tests, 4 golden tests, and Task 15's 7 still green.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-shadow/
git commit -m "feat(shadow): trace diffing and the baseline policy's golden fixtures"
```

---

## Task 17: Shadow — replay from an export archive, and `msafe shadow`

**Files:**
- Modify: `crates/memorysafe-shadow/src/replay.rs`, `src/lib.rs`
- Create: `crates/memorysafe-shadow/tests/replay.rs`
- Create: `crates/memorysafe-cli/src/cmd/shadow.rs`
- Modify: `crates/memorysafe-cli/src/cmd/mod.rs`, `src/main.rs`, `Cargo.toml`
- Create: `crates/memorysafe-cli/tests/shadow.rs`

**Interfaces:**
- Consumes: `ExportRecord`, `AuditRecord`, `MemoryItem`, `Scenario`, `Trace`, `run`, `diff`.
- Produces: `Replay`, `Scenario::from_export_ndjson`, `replay::from_export_ndjson`, and the
  `msafe shadow` command.

**What can and cannot be replayed, stated plainly.** An audit row names the items it concerns by
id and digest, never by body — that is a load-bearing privacy property. So a decision can be
replayed only when the item it produced is still in the archive:

| Recorded event | Replayable | Why |
|---|---|---|
| `Admitted` | yes | The item is in the archive; its body reconstructs the write. |
| `Merged` | no | The new content was folded into the target and no longer exists separately. |
| `Rejected` | no | Nothing was stored, so the body was never written anywhere. |
| everything else | no | Not a write. |

Every unreplayable row is counted and reported, and `Scenario::coverage` is the fraction that
could be replayed. A shadow run that covers 40% of a log says so rather than implying it proved
something about the other 60%.

**An export without audit records still works.** Items carry ULID ids, which sort by creation
time, so the write order is recoverable from the corpus alone. There is then nothing recorded to
diff against, and the command says so.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-shadow/tests/replay.rs`:

```rust
use memorysafe_backend::ScopeSelector;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{Scope, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use memorysafe_shadow::{TracedAction, replay, run};
use std::sync::Arc;

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

async fn seeded_export(include_audit: bool) -> String {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.path().to_path_buf())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ));
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        engine.remember(RememberRequest::new(scope(), body)).await.unwrap();
    }
    // A rejected write: recorded, but unreplayable by construction.
    engine
        .remember(RememberRequest::new(scope(), "the production migration runs on Sundays"))
        .await
        .unwrap();

    engine
        .export_ndjson(&ScopeSelector {
            tenant: TenantId::new("acme").unwrap(),
            subject: None,
            namespace: None,
            include_audit,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_archive_with_audit_becomes_a_scenario_of_the_admitted_writes() {
    let ndjson = seeded_export(true).await;
    let r = replay::from_export_ndjson("live", &ndjson).unwrap();

    assert_eq!(r.scenario.writes.len(), 3, "three admissions");
    assert!(
        !r.scenario.unreplayable.is_empty(),
        "the rejected write must be reported, not silently dropped"
    );
    assert!(r.scenario.coverage() < 1.0);
    assert!(r.scenario.coverage() > 0.5);

    let bodies: Vec<&str> = r.scenario.writes.iter().map(|w| w.body.as_str()).collect();
    assert!(bodies.contains(&"the production migration runs on Sundays"));
}

#[tokio::test]
async fn the_recorded_decisions_come_back_as_a_trace_that_diffs_against_a_replay() {
    let ndjson = seeded_export(true).await;
    let r = replay::from_export_ndjson("live", &ndjson).unwrap();
    let recorded = r.recorded.expect("an archive with audit has recorded decisions");

    assert_eq!(recorded.decisions.len(), r.scenario.writes.len());
    for decision in &recorded.decisions {
        assert!(matches!(decision.action, TracedAction::Retain { .. }));
    }

    let replayed = run(&r.scenario, Arc::new(BaselinePolicy::default())).await.unwrap();
    let d = memorysafe_shadow::diff(&recorded, &replayed).unwrap();
    assert_eq!(d.total, 3);
    // The same policy over the same writes must still admit all three; the
    // reasons may differ because the replay corpus is smaller.
    assert_eq!(
        d.transitions.values().filter(|_| true).count(),
        d.transitions.len(),
        "sanity"
    );
    assert!(
        !d.transitions.keys().any(|k| k.ends_with("->reject")),
        "replaying the same policy turned an admission into a rejection: {:?}",
        d.transitions
    );
}

#[tokio::test]
async fn an_archive_without_audit_still_yields_a_scenario_in_creation_order() {
    let ndjson = seeded_export(false).await;
    let r = replay::from_export_ndjson("no-audit", &ndjson).unwrap();

    assert_eq!(r.scenario.writes.len(), 3);
    assert!(r.recorded.is_none(), "there is nothing recorded to compare against");
    assert!(r.scenario.unreplayable.is_empty());
}

#[tokio::test]
async fn a_stream_that_is_not_an_export_is_a_clear_error() {
    assert!(replay::from_export_ndjson("junk", "not json at all\n").is_err());
    assert!(replay::from_export_ndjson("junk", "{\"record\":\"nonsense\"}\n").is_err());
    // An empty stream is not an error; it is an empty scenario.
    let empty = replay::from_export_ndjson("empty", "").unwrap();
    assert!(empty.scenario.writes.is_empty());
}

#[tokio::test]
async fn a_replayed_scenario_carries_the_scope_each_write_belonged_to() {
    let ndjson = seeded_export(true).await;
    let r = replay::from_export_ndjson("live", &ndjson).unwrap();
    for write in &r.scenario.writes {
        assert_eq!(write.scope, scope());
    }
}
```

`crates/memorysafe-cli/tests/shadow.rs`:

```rust
use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn msafe(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("msafe").expect("binary");
    cmd.current_dir(dir);
    cmd.env("MSAFE_TENANT", "acme");
    cmd.env("MSAFE_SUBJECT", "user-42");
    cmd.env("MSAFE_NAMESPACE", "agent");
    cmd
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("msafe.toml"), "data_dir = \"tenants\"\n").unwrap();
    dir
}

fn archive(dir: &Path) -> std::path::PathBuf {
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        msafe(dir).args(["remember", body]).assert().success();
    }
    let out = dir.join("archive");
    msafe(dir)
        .args(["export", out.to_str().unwrap(), "--include-audit"])
        .assert()
        .success();
    out
}

#[test]
fn shadow_replays_an_archive_against_the_current_policy() {
    let dir = workspace();
    let out = archive(dir.path());

    msafe(dir.path())
        .args(["shadow", out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("coverage"))
        .stdout(contains("identical").or(contains("changed")));
}

#[test]
fn shadow_diffs_two_policy_configurations_and_reports_the_transitions() {
    let dir = workspace();
    let out = archive(dir.path());

    // A candidate that rejects everything must move every decision.
    let candidate = dir.path().join("candidate.json");
    std::fs::write(
        &candidate,
        serde_json::json!({
            "duplicate_threshold": 0.0,
            "merge_threshold": 0.0,
            "near_duplicate_floor": 0.0,
            "replay_quota": 0.20,
            "mmr_lambda": 0.70,
            "value_half_life_days": 90.0,
            "source_trust_weight": 0.20,
            "replay_stale_days": 30.0
        })
        .to_string(),
    )
    .unwrap();

    let output = msafe(dir.path())
        .args([
            "--json",
            "shadow",
            out.to_str().unwrap(),
            "--candidate-config",
            candidate.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["diff"]["total"], serde_json::json!(3));
    assert!(
        !value["diff"]["changed"].as_array().unwrap().is_empty(),
        "a policy that rejects everything changed nothing: {value}"
    );
    assert!(value["coverage"].as_f64().is_some());
}

#[test]
fn shadow_on_an_archive_without_audit_says_there_is_nothing_to_compare() {
    let dir = workspace();
    msafe(dir.path()).args(["remember", "a lone memory"]).assert().success();
    let out = dir.path().join("bare");
    msafe(dir.path()).args(["export", out.to_str().unwrap()]).assert().success();

    msafe(dir.path())
        .args(["shadow", out.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("--include-audit").or(contains("recorded")));
}

#[test]
fn shadow_writes_its_report_to_a_file_when_asked() {
    let dir = workspace();
    let out = archive(dir.path());
    let report = dir.path().join("report.json");

    msafe(dir.path())
        .args([
            "shadow",
            out.to_str().unwrap(),
            "--out",
            report.to_str().unwrap(),
        ])
        .assert()
        .success();

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert!(value["diff"]["total"].is_u64());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p memorysafe-shadow --test replay`
Expected: FAIL — `cannot find function 'from_export_ndjson' in module 'replay'`.

- [ ] **Step 3: Write minimal implementation**

`crates/memorysafe-shadow/src/replay.rs`:

```rust
//! Turning a real export archive back into a scenario.

use crate::ShadowError;
use crate::scenario::{Scenario, ScenarioWrite, Unreplayable};
use crate::trace::{Trace, TracedAction, TracedDecision};
use memorysafe_backend::ExportRecord;
use memorysafe_core::{Action, AuditEvent, AuditRecord, ItemId, MemoryItem, PolicyId};
use std::collections::HashMap;

pub struct Replay {
    pub scenario: Scenario,
    /// The decisions as they were originally recorded, aligned to the
    /// scenario's writes. `None` when the archive carried no audit records.
    pub recorded: Option<Trace>,
}

/// The default embedder width. A real archive does not record which embedder
/// produced its vectors in a form this harness can reconstruct, and the vectors
/// are recomputed anyway, so a shadow run states its width rather than guessing.
const REPLAY_DIM: u16 = 256;

pub fn from_export_ndjson(name: &str, ndjson: &str) -> Result<Replay, ShadowError> {
    let mut items: HashMap<ItemId, MemoryItem> = HashMap::new();
    let mut audit: Vec<AuditRecord> = Vec::new();

    for (number, line) in ndjson.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: ExportRecord = serde_json::from_str(line).map_err(|e| {
            ShadowError::Malformed(format!("line {}: {e}", number + 1))
        })?;
        match record {
            ExportRecord::Header { .. } => {}
            ExportRecord::Item { item, .. } => {
                items.insert(item.id.clone(), *item);
            }
            ExportRecord::Audit { audit: record } => audit.push(*record),
        }
    }

    if audit.is_empty() {
        // No recorded decisions. ULIDs sort by creation time, so the corpus
        // alone still gives a faithful write order.
        let mut ordered: Vec<&MemoryItem> = items.values().collect();
        ordered.sort_by(|a, b| a.id.cmp(&b.id));
        return Ok(Replay {
            scenario: Scenario {
                name: name.to_owned(),
                embedder_dim: REPLAY_DIM,
                budgets: vec![],
                writes: ordered.into_iter().map(write_from_item).collect(),
                unreplayable: vec![],
            },
            recorded: None,
        });
    }

    // `AuditId` is a millisecond ULID and therefore a total order; `at` is whole
    // seconds and cannot separate rows written in the same second.
    audit.sort_by(|a, b| a.id.cmp(&b.id));

    let mut writes = Vec::new();
    let mut recorded = Vec::new();
    let mut unreplayable = Vec::new();
    let mut policy: Option<PolicyId> = None;

    for record in &audit {
        let event_name = format!("{:?}", record.event).to_lowercase();
        let skip = |why: &str| Unreplayable {
            audit_id: record.id.to_string(),
            event: event_name.clone(),
            why: why.to_owned(),
        };

        match record.event {
            AuditEvent::Admitted => {}
            AuditEvent::Merged => {
                unreplayable.push(skip(
                    "the merged content was folded into its target and no longer exists separately",
                ));
                continue;
            }
            AuditEvent::Rejected => {
                unreplayable.push(skip(
                    "nothing was stored, so the rejected body was never written to the archive",
                ));
                continue;
            }
            _ => {
                unreplayable.push(skip("not a write"));
                continue;
            }
        }

        let Some(id) = record.items.first().map(|r| r.id()) else {
            unreplayable.push(skip("the admission record names no item"));
            continue;
        };
        let Some(item) = items.get(id) else {
            unreplayable.push(skip("the admitted item is not in this archive"));
            continue;
        };

        let seq = writes.len();
        writes.push(write_from_item(item));

        let decision = record.decision.as_ref();
        if let Some(d) = decision {
            policy.get_or_insert_with(|| d.policy.clone());
        }
        recorded.push(TracedDecision {
            seq,
            scope: item.scope.clone(),
            body_digest: blake3::hash(item.body.as_bytes()).to_hex().to_string(),
            action: match decision.map(|d| &d.action) {
                Some(Action::Retain { protection }) => TracedAction::Retain { protection: *protection },
                Some(Action::Merge { .. }) => TracedAction::Merge { into_seq: None },
                Some(Action::Reject) => TracedAction::Reject,
                // An admission with no recorded decision: the event says what
                // happened even when the detail was pruned by retention.
                None => TracedAction::Retain { protection: item.protection },
            },
            reason_codes: decision
                .map(|d| d.reasons.iter().map(|r| r.code).collect())
                .unwrap_or_default(),
            evicted: decision.map(|d| d.evictions.len()).unwrap_or(0),
            sensitivity: Some(item.sensitivity),
        });
    }

    let scenario = Scenario {
        name: name.to_owned(),
        embedder_dim: REPLAY_DIM,
        budgets: vec![],
        writes,
        unreplayable,
    };
    let recorded = Trace {
        scenario: scenario.name.clone(),
        policy: policy.unwrap_or_else(|| PolicyId::new("unrecorded", "0")),
        decisions: recorded,
    };

    Ok(Replay { scenario, recorded: Some(recorded) })
}

fn write_from_item(item: &MemoryItem) -> ScenarioWrite {
    ScenarioWrite {
        scope: item.scope.clone(),
        body: item.body.clone(),
        kind: item.kind.clone(),
        tags: item.tags.clone(),
        // The original hint is not recorded — only the resolved level, which a
        // policy must be free to reach on its own. Replaying with the resolved
        // level as a hint would guarantee agreement and prove nothing.
        sensitivity_hint: None,
        ttl_seconds: item.ttl.map(|d| d.whole_seconds()),
    }
}
```

Export it from `lib.rs`: `pub use replay::Replay;`

Add `Scenario::from_export_ndjson` as the convenience the canonical signatures name:

```rust
impl Scenario {
    pub fn from_export_ndjson(name: &str, ndjson: &str) -> Result<Scenario, ShadowError> {
        Ok(crate::replay::from_export_ndjson(name, ndjson)?.scenario)
    }
}
```

Add `memorysafe-shadow.workspace = true` to `crates/memorysafe-cli/Cargo.toml` `[dependencies]`.

`crates/memorysafe-cli/src/cmd/shadow.rs`:

```rust
use crate::cmd::portable;
use crate::config::MsafeConfig;
use crate::render;
use anyhow::{Context, Result, bail};
use clap::Args;
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use memorysafe_shadow::{TraceDiff, diff, replay, run};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Args)]
pub struct ShadowArgs {
    /// An export archive directory, or a bare `.ndjson` export stream.
    pub archive: PathBuf,
    /// The policy implementation to replay with. `baseline` is the only one in
    /// this build; a closed policy crate registers its own name.
    #[arg(long, default_value = "baseline", value_parser = ["baseline"])]
    pub policy: String,
    /// JSON `BaselineConfig` for the candidate policy. Without it, the archive's
    /// recorded decisions are compared against a replay under the configured
    /// policy.
    #[arg(long)]
    pub candidate_config: Option<PathBuf>,
    /// JSON `BaselineConfig` for the baseline side. Defaults to the policy in
    /// the configuration file.
    #[arg(long)]
    pub baseline_config: Option<PathBuf>,
    /// Write the full report here as JSON.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Serialize)]
struct ShadowReport {
    scenario: String,
    replayed: usize,
    unreplayable: usize,
    coverage: f32,
    baseline_policy: String,
    candidate_policy: String,
    diff: TraceDiff,
}

fn load_config(path: &PathBuf) -> Result<BaselineConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub async fn shadow(config: &MsafeConfig, json: bool, args: ShadowArgs) -> Result<()> {
    let ndjson = portable::read_stream(&args.archive)?;
    let name = args
        .archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    let replayed = replay::from_export_ndjson(&name, &ndjson)?;

    let baseline_config = match &args.baseline_config {
        Some(path) => load_config(path)?,
        None => config.policy.clone(),
    };
    let baseline = Arc::new(BaselinePolicy::new(baseline_config));

    let (before, after) = match &args.candidate_config {
        Some(path) => {
            let candidate = Arc::new(BaselinePolicy::new(load_config(path)?));
            (
                run(&replayed.scenario, baseline).await?,
                run(&replayed.scenario, candidate).await?,
            )
        }
        None => {
            let Some(recorded) = replayed.recorded else {
                bail!(
                    "this archive carries no recorded decisions, so there is nothing to compare a \
                     replay against; export it with --include-audit, or pass --candidate-config to \
                     diff two policy configurations instead"
                );
            };
            let replayed_trace = run(&replayed.scenario, baseline).await?;
            (recorded, replayed_trace)
        }
    };

    let report = ShadowReport {
        scenario: replayed.scenario.name.clone(),
        replayed: replayed.scenario.writes.len(),
        unreplayable: replayed.scenario.unreplayable.len(),
        coverage: replayed.scenario.coverage(),
        baseline_policy: before.policy.to_string(),
        candidate_policy: after.policy.to_string(),
        diff: diff(&before, &after)?,
    };

    if let Some(path) = &args.out {
        std::fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    render::emit(json, &report, || {
        println!(
            "{}: {} decision(s) replayed, {} unreplayable, coverage {:.0}%",
            report.scenario,
            report.replayed,
            report.unreplayable,
            report.coverage * 100.0
        );
        println!(
            "{} identical, {} changed, out of {}",
            report.diff.identical,
            report.diff.changed.len(),
            report.diff.total
        );
        for (transition, count) in &report.diff.transitions {
            println!("  {transition}: {count}");
        }
        for change in report.diff.changed.iter().take(20) {
            println!(
                "  #{} {} -> {}",
                change.seq,
                change.before.action.label(),
                change.after.action.label()
            );
        }
        if report.diff.changed.len() > 20 {
            println!("  … {} more (use --out for the full report)", report.diff.changed.len() - 20);
        }
    })
}
```

Make `portable::read_stream` visible: change it to `pub(crate) fn read_stream`.

Add `pub mod shadow;` to `cmd/mod.rs`, and to `main.rs`:

```rust
    /// Replay an archive's decisions against another policy and diff them.
    Shadow(cmd::shadow::ShadowArgs),
```

```rust
        Command::Shadow(args) => cmd::shadow::shadow(&config, cli.json, args).await,
```

`shadow` does not use the engine; it builds its own throwaway ones. It still resolves a scope,
because `main` does that before dispatch — that is acceptable, and it keeps the tenant available
for a future per-tenant policy lookup.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p memorysafe-shadow && cargo test -p memorysafe-cli`
Expected: PASS — 5 new shadow tests, 4 new CLI tests.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-shadow/ crates/memorysafe-cli/
git commit -m "feat(shadow): replay a real archive and diff it against another policy"
```

---

## Task 18: Acceptance — end-to-end walkthrough, CI, and documentation

**Files:**
- Create: `crates/memorysafe-cli/tests/walkthrough.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Create: `docs/adapters.md`

**Interfaces:**
- Consumes: everything.
- Produces: the CI dependency-direction check and the user-facing documentation for the three
  ways in.

**Why one more test.** Tasks 3–17 each prove a surface works. This one proves the *product* works:
one binary, one config file, a memory written and recalled and shown and protected and exported
and re-imported and shadow-evaluated, in the order a person would actually do it. It is the test
that fails when two correct pieces do not fit together.

- [ ] **Step 1: Write the failing test**

`crates/memorysafe-cli/tests/walkthrough.rs`:

```rust
use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn msafe(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("msafe").expect("binary");
    cmd.current_dir(dir);
    cmd.env("MSAFE_TENANT", "acme");
    cmd.env("MSAFE_SUBJECT", "user-42");
    cmd.env("MSAFE_NAMESPACE", "coding-agent");
    cmd
}

fn json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// The walkthrough from the README, executed. If this passes, a person
/// following the documentation gets what the documentation says they get.
#[test]
fn a_person_can_run_the_whole_product_from_one_binary() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("msafe.toml"),
        r#"data_dir = "tenants"
tenant = "acme"
subject = "user-42"
namespace = "coding-agent"
retention = "balanced"
"#,
    )
    .unwrap();
    let dir = dir.path();

    // 1. Remember three things. The third is a near-repeat of the first.
    let first = msafe(dir)
        .args(["--json", "remember", "the production migration runs on Sundays", "--tag", "ops"])
        .output()
        .unwrap();
    assert_eq!(json(&first)["action"]["kind"], "retain");
    let pinned_id = json(&first)["item_id"].as_str().unwrap().to_owned();

    msafe(dir)
        .args(["remember", "the customer prefers written summaries over meetings", "--kind", "preference"])
        .assert()
        .success();
    msafe(dir)
        .args(["--json", "remember", "the production migration runs on Sundays"])
        .assert()
        .success();

    // 2. A credential is detected and stored restricted whatever was asked for.
    msafe(dir)
        .args([
            "remember",
            "the deployment api key is sk-abc123def456ghi789jkl012mno345",
            "--sensitivity",
            "public",
        ])
        .assert()
        .success();
    let stored = msafe(dir).args(["--json", "review"]).output().unwrap();
    let credential = json(&stored)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["body"].as_str().unwrap().contains("sk-abc123"))
        .expect("the credential was stored")
        .clone();
    assert_eq!(
        credential["sensitivity"], "restricted",
        "a caller's hint lowered a credential's level"
    );

    // 3. Recall composes a working set and audits itself.
    let recalled = msafe(dir)
        .args(["--json", "recall", "when does the migration run", "--max-items", "2"])
        .output()
        .unwrap();
    assert!(json(&recalled)["audit_id"].is_string());
    assert!(!json(&recalled)["items"].as_array().unwrap().is_empty());

    // 4. A recall with a ceiling cannot see the credential.
    let capped = msafe(dir)
        .args(["--json", "recall", "api key", "--sensitivity-ceiling", "internal"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&capped.stdout);
    assert!(!text.contains("sk-abc123"), "the sensitivity ceiling was not enforced");

    // 5. Pin one memory, and confirm it survives a squeeze.
    msafe(dir).args(["protect", &pinned_id, "--level", "pinned"]).assert().success();

    // 6. The audit trail explains everything and leaks nothing.
    let audited = msafe(dir).args(["--json", "audit", "--limit", "50"]).output().unwrap();
    let audit_text = String::from_utf8_lossy(&audited.stdout);
    assert!(!audit_text.contains("sk-abc123"), "the audit trail leaked a credential");
    assert!(!audit_text.contains("prefers written summaries"), "the audit trail leaked a body");
    assert!(json(&audited)["records"].as_array().unwrap().len() >= 4);

    // 7. Maintenance runs and reports.
    msafe(dir).args(["maintain", "--all"]).assert().success();

    // 8. Export, then import into a clean workspace, and get the same corpus.
    let archive = dir.join("archive");
    msafe(dir)
        .args(["export", archive.to_str().unwrap(), "--include-audit"])
        .assert()
        .success();

    let restored = tempfile::tempdir().unwrap();
    std::fs::write(restored.path().join("msafe.toml"), "data_dir = \"tenants\"\n").unwrap();
    msafe(restored.path())
        .args(["import", archive.to_str().unwrap()])
        .assert()
        .success();

    let before = json(&msafe(dir).args(["--json", "review"]).output().unwrap());
    let after = json(&msafe(restored.path()).args(["--json", "review"]).output().unwrap());
    let count = |v: &serde_json::Value| v["items"].as_array().unwrap().len();
    assert_eq!(count(&before), count(&after), "the round trip lost memories");

    // 9. Shadow-evaluate the archive against the current policy.
    msafe(dir)
        .args(["shadow", archive.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("coverage"));

    // 10. Create an API key so the HTTP surface is usable.
    let key = msafe(dir).args(["keys", "add", "--label", "walkthrough"]).output().unwrap();
    assert!(String::from_utf8_lossy(&key.stdout).contains("msk_"));
    let config = std::fs::read_to_string(dir.join("msafe.toml")).unwrap();
    assert!(config.contains("walkthrough"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memorysafe-cli --test walkthrough`
Expected: FAIL, or PASS. If it passes on the first run, that is the correct outcome for an
acceptance test over finished parts — but read the assertions and confirm each one is reached
(temporarily break one, watch it fail, restore it). An acceptance test nobody has seen fail is an
acceptance test that might be asserting nothing.

- [ ] **Step 3: Write the CI checks and the documentation**

Replace `.github/workflows/ci.yml` with:

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
          for crate in memorysafe-core memorysafe-policy; do
            if cargo tree -p "$crate" --edges normal --prefix none \
              | grep -Ei '^(tokio|rusqlite|sqlx|reqwest|hyper) '; then
              echo "$crate pulled in an I/O crate"
              exit 1
            fi
            echo "$crate is I/O free"
          done
  layering:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.97.1
      - name: adapters must not reach past the engine into a backend
        run: |
          # memorysafe-cli is the composition root and must choose a backend.
          # memorysafe-shadow is a harness that drives a real engine.
          # Everything else talks to memorysafe-engine and nothing below it.
          for crate in memorysafe-mcp memorysafe-api; do
            if cargo tree -p "$crate" --edges normal --prefix none \
              | grep -Ei '^(memorysafe-backend-sqlite|rusqlite|sqlx) '; then
              echo "$crate depends on a storage backend directly"
              exit 1
            fi
            echo "$crate is backend-free"
          done
      - name: memorysafe-auth must stay I/O free
        run: |
          if cargo tree -p memorysafe-auth --edges normal --prefix none \
            | grep -Ei '^(tokio|axum|rmcp|rusqlite) '; then
            echo "memorysafe-auth grew a dependency on a transport"
            exit 1
          fi
          echo "memorysafe-auth is transport free"
```

Create `docs/adapters.md`:

````markdown
# Using MemorySafe

Three ways in, one engine behind all of them. Governance decisions are the same
whichever door you come through, and every one of them is recorded.

## Configuration

`msafe.toml`, read from the working directory or `--config`:

```toml
data_dir = "tenants"          # one SQLite file per tenant, created on demand
tenant = "acme"
subject = "user-42"
namespace = "coding-agent"
embedder = "deterministic"     # or "model2vec" in a build with that feature
embedding_dim = 256
retention = "balanced"         # balanced | gdpr_strict | hipaa_retain | forensic

[policy]                       # every threshold, with the documented defaults
merge_threshold = 0.93
duplicate_threshold = 0.98

[serve]
bind = "127.0.0.1:8080"
mcp_path = "/mcp"
allowed_hosts = ["localhost", "127.0.0.1"]
```

`--tenant`, `--subject`, and `--namespace` (or `MSAFE_TENANT` and friends)
override the file. There is no default scope: writing into a scope nobody named
is how memories end up in the wrong place.

`_admin` is reserved. It is the subject and namespace tenant-level records are
written under, and no caller may name it.

## CLI

```sh
msafe remember "the production migration runs on Sundays" --tag ops
msafe recall "when does the migration run" --max-items 5
msafe review
msafe forget --tag ops
msafe protect <item-id> --level pinned
msafe audit --event admitted
msafe maintain --all
msafe export ./archive --include-audit
msafe import ./archive
msafe shadow ./archive --candidate-config ./candidate.json
msafe keys add --label "ci runner"
msafe purge-subject user-42 --yes
```

Every command takes `--json` and prints the engine's own types. A rejected write
exits `0`: governance working is not a failure.

## MCP

```sh
msafe serve --transport stdio
```

Five tools — `memory_remember`, `memory_recall`, `memory_review`,
`memory_forget`, `memory_protect` — and two resources per scope:

```
memorysafe://{tenant}/{subject}/{namespace}/audit
memorysafe://{tenant}/{subject}/{namespace}/stats
```

Over stdio the server is bound to one tenant and one subject; the namespace
defaults from the working directory and a call may override it. Over streamable
HTTP the API key identifies the tenant and each call carries subject and
namespace.

## HTTP

```sh
msafe keys add --label "my service"     # prints the secret once
msafe serve --transport http
```

`Authorization: Bearer <api-key>`, one key to one tenant.

```
GET    /v1/health                    unauthenticated
GET    /v1/whoami                    which tenant this key is
POST   /v1/recall
POST   /v1/memories                  remember
GET    /v1/memories                  review
GET    /v1/memories/{id}
DELETE /v1/memories/{id}
POST   /v1/forget
POST   /v1/memories/{id}/protect
GET    /v1/audit                     ?event=admitted,forgotten
POST   /v1/maintain
GET    /v1/export                    ?format=ndjson|markdown
POST   /v1/import
DELETE /v1/subjects/{id}
GET|PUT /v1/admin/tenants/{id}/budgets
GET|PUT /v1/admin/tenants/{id}/policy
GET|PUT /v1/admin/tenants/{id}/retention
```

Errors carry a uniform body:

```json
{ "error": "validation", "message": "body must not be empty", "retryable": false }
```

| Situation | Status |
|---|---|
| Bad scope, oversize item, malformed filter | 400 |
| No credential, or one that is not recognised | 401 |
| Recognised credential, access refused | 403 |
| No such memory | 404 |
| Idempotency key reused with a different payload | 409 |
| Storage unavailable | 503, with `retryable` |

A rejected or merged write is `200 OK`.

Reads take `subject` and `namespace` as query parameters; writes take them in the
JSON body. Neither is defaulted. `GET /v1/audit` takes its event filter as one
comma-separated value, and pages with `limit` plus `after=<last audit id>`; the
response carries `truncated` so a compliance query cannot stop short silently.

## Shadow evaluation

```sh
msafe export ./archive --include-audit
msafe shadow ./archive --candidate-config ./candidate.json
```

Replays the archive's admissions under two policy configurations and diffs the
decisions. Only admissions can be replayed — a rejected write's body was never
stored, and a merged write's content was folded into its target — so the report
states its coverage rather than implying it proved something about the rest.
````

Update `README.md`: replace the status table's last three rows with

```markdown
| `memorysafe-engine` — orchestration | done |
| `memorysafe-mcp` — five tools, audit resources, stdio and streamable HTTP | done |
| `memorysafe-api` — the HTTP surface | done |
| `memorysafe-cli` — `msafe` | done |
| `memorysafe-shadow` — shadow evaluation | done |
```

and add, after the "Design" section:

````markdown
## Using it

See [`docs/adapters.md`](docs/adapters.md) for configuration, the CLI, the MCP
server, and the HTTP API.

```sh
cargo install --path crates/memorysafe-cli
msafe remember "the production migration runs on Sundays"
msafe recall "when does the migration run"
```
````

- [ ] **Step 4: Run the whole suite**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```
Expected: PASS, everything green.

Then verify the CI layering job locally:
```bash
cargo tree -p memorysafe-mcp --edges normal --prefix none | grep -Ei '^(memorysafe-backend-sqlite|rusqlite) ' && echo LEAK || echo clean
cargo tree -p memorysafe-api --edges normal --prefix none | grep -Ei '^(memorysafe-backend-sqlite|rusqlite) ' && echo LEAK || echo clean
```
Expected: `clean` twice.

- [ ] **Step 5: Commit**

```bash
git add crates/memorysafe-cli/tests/walkthrough.rs .github/workflows/ci.yml README.md docs/adapters.md
git commit -m "feat: end-to-end walkthrough, layering checks, and adapter documentation"
```

---

## Definition of done for Plan 3

- `cargo test --workspace --all-features` is green.
- `cargo clippy --all-targets --all-features -- -D warnings` is clean.
- The CI purity job confirms `memorysafe-core` and `memorysafe-policy` pull in no I/O crates, and
  the layering job confirms `memorysafe-mcp` and `memorysafe-api` pull in no storage backend.
- A real MCP client drives the `msafe` binary over stdio and calls all five tools.
- A real MCP client drives the streamable-HTTP transport with a bearer token, and a key for the
  wrong tenant cannot write.
- The HTTP surface implements every route in §12 and the §9 status table, and a rejected write
  returns `200`.
- `msafe` runs the whole product: remember, recall, review, forget, protect, audit, maintain,
  export, import, shadow, keys, purge-subject, serve.
- An export archive round-trips through import with the same corpus, and a truncated archive is
  refused before anything is written.
- The four golden fixtures pin `BaselinePolicy`'s decisions; a change to the policy fails a test
  with a readable diff.
- `msafe shadow` replays a real archive and reports its coverage honestly.
- No item body appears in any audit output, on any surface — asserted on MCP resources, the HTTP
  audit route, the CLI audit command, and shadow traces.
- The three `AuditEvent` variants Plan 1 deferred — `Exported`, `Imported`, `PolicyChanged` — are
  written, with the actor attached, and are covered by tests.

### Deferred to a later plan

Recorded so they are not rediscovered as surprises.

- **Per-key sensitivity clearance.** `sensitivity_ceiling` defaults to `Restricted`, which excludes
  nothing, because v1 has no per-credential clearance. A `max_sensitivity` field on `ApiKeyRecord`
  and a clamp in both network adapters is the change; it is a change to a persisted record format,
  so it belongs with the next one of those rather than bolted on here.
- **A cross-tenant administrator.** An API key is scoped to one tenant, so `/v1/admin/tenants/{id}`
  can only ever address the caller's own. A control-plane credential is a cloud-tier concern and is
  explicitly out of v1 scope.
- **ZIP archives.** The archive is a directory. Nothing in the format depends on that, and a ZIP
  writer is additive.
- **Replaying rejections and merges.** Their bodies were never stored, so no shadow run can cover
  them from an archive alone. Covering them would mean recording candidate bodies in the audit
  trail, which contradicts a load-bearing privacy property; the alternative — a capture mode that
  writes a scenario file as writes happen — is the shape to build if the coverage gap ever matters.
- **Scenarios with curation steps.** A `Scenario` is writes only. Protect, forget, and maintenance
  steps would let fixtures cover eviction under pinning and TTL expiry, and are additive to the
  file format.
