# MemorySafe Engine — Design

**Date:** 2026-09-05
**Status:** Approved for planning
**Scope:** v1 of the MemorySafe memory engine, its backends, and its MCP/HTTP/CLI adapters.

---

## 1. Product thesis

MemorySafe is **governed memory infrastructure for AI**. It is not primarily a retrieval
architecture; it is admission and eviction control for memory.

Every memory that enters the system is assessed, a policy decides what happens to it, and the
reason is recorded. Every recall is composed by the same policy under an explicit budget, and that
composition is recorded too.

```
assess  →  value, fragility, sensitivity, redundancy  (item)  +  capacity  (scope)
apply   →  retain | protect | replay | merge | forget
record  →  a structured, queryable reason for every decision
```

The lineage is continual-learning replay-buffer selection, generalised to agent memory. The
differentiator is not "we store memories" but "we decide, defensibly and auditably, which memories
are worth keeping and which are worth surfacing."

### Who it is for

An **embeddable backend for agent builders**. Customers are companies building agent products;
memory is scoped to *their* end-users. Three ways in:

- an in-process Rust crate,
- an MCP server (stdio and streamable HTTP),
- an HTTP API.

A solo developer running it on a laptop and a company running the hosted tier use the same engine.

### Business boundary

Open core, applied at two seams:

| Component | Licence | Rationale |
|---|---|---|
| Core types, traits, engine, SQLite backend, baseline policy, adapters | Open source | The substrate. Must be genuinely good, not a crippled demo. |
| `memorysafe-backend-postgres` | Commercial (separate repo) | The scaling tier. |
| `memorysafe-policy-governed` | Commercial (separate repo) | The proprietary scorer. The crown jewel. |

The free tier's ceiling is SQLite on a single machine. That is the monetisation line, and it is
deliberate. It places real weight on the SQLite backend being excellent.

---

## 2. Goals and non-goals

### Goals

1. A memory engine that runs in-process with no server, no network, and no LLM in the write path.
2. Governance decisions that are cheap enough to run inline on every write.
3. Structured, queryable audit for every mutation and every recall.
4. Tenant isolation that is structural, not a code invariant.
5. Two storage backends behind one trait, both verified by a shared conformance suite.
6. A policy seam that third parties can implement and that the proprietary scorer plugs into.
7. Portable export and import, so a customer's memory is genuinely theirs.

### Non-goals for v1

- The proprietary policy implementation (separate repo; ships against the finished trait).
- ANN vector indexes.
- Cloud control plane, billing, provisioning, web UI.
- The continual-learning product line.
- Cross-subject search, shared multi-agent memory, or org-wide knowledge.
- LLM-based extraction from raw conversation transcripts.

---

## 3. Decision log

| # | Decision | Rationale |
|---|---|---|
| D1 | Embeddable backend for agent builders; multi-tenant from day one | Memory is about the customer's end-users, so isolation and tenancy cannot be retrofitted. |
| D2 | Governance policy is the product; it lives behind a trait | Enables OSS substrate + proprietary scorer + third-party policies from one interface. |
| D3 | Unit of memory is a caller-authored discrete item | No model in the write path. Fast, offline-capable, predictable cost. Governance needs granularity that page-level units cannot give. |
| D4 | "Vulnerability" splits into two axes: **fragility** and **sensitivity** | They pull in opposite directions (a rare medical detail is both maximally fragile and maximally sensitive). Collapsing them makes conflicts invisible and audit unexplainable. |
| D5 | Scope is `tenant → subject → namespace` | Tenant is the isolation unit, subject the delete/export unit, namespace the budget/retrieval unit. |
| D6 | Governance shapes reads as well as writes ("governed working set") | Makes "governed memory" true on both paths. Gives `replay` a meaning in agent memory: an item earning a context slot to stay live. |
| D7 | Library-first workspace; adapters are thin | Only shape that delivers true in-process embedding. Lets governance be tested with no server, no network, no protocol. |
| D8 | SQLite for the OSS backend, one database file per tenant | "Reviewable rather than invisible" is a product claim; a file the customer can open in any tool is its strongest expression. Isolation becomes structural. |
| D9 | Postgres backend is in v1, as a commercial crate | A conformance suite with one implementation proves nothing, and the trait rots within a month. |
| D10 | `Store` and `Index` merge into one `Backend` trait | pgvector searches inside the database; a separate index trait would bake the SQLite shape into the interface. |
| D11 | Policies are pure; the engine performs all I/O | Enables fixture testing, shadow evaluation, and a verifiable "the closed crate has no I/O capability" claim. |
| D12 | Hard filters execute in the backend query, below the policy | A policy can only narrow the candidate set, never widen it. A bug in the closed scorer cannot become a data leak. |
| D13 | No ANN index in v1; brute-force SIMD over int8-quantised vectors | Scope-bounded queries keep corpora small. Exact results, nothing to rebuild on every write, nothing to corrupt. |
| D14 | Audit retention is configurable per tenant, via named profiles | Customers face conflicting regimes (GDPR vs HIPAA). Named profiles keep "configurable" from meaning an untestable matrix. |
| D15 | Portable export/import is a v1 feature, not a published spec | Captures memoryfield's anti-lock-in payoff without freezing a format before the policy's needs are known. |

### Rejected alternatives

- **`fjall` / `redb` as the OSS backend.** Excellent engines, but opaque to the customer and
  optimised for write throughput this workload does not have. Agent memory is a few writes per
  turn, not a firehose.
- **`wedb_embed`.** First published 2026-08-27; 188 lifetime downloads; single author; ~39k LOC
  against 189 comment lines. Not a foundation for regulated buyers.
- **Object-storage sharding (shard-per-subject on S3).** Designed and discarded. Postgres covers
  the scaling tier without inventing a distributed system, and the data shape that would justify
  sharding is not yet known.
- **LLM extraction from transcripts.** Puts a model, its latency, and its cost in the write path,
  and undermines the offline local story.
- **Postgres + pgvector as the *only* backend.** Loses the in-process embedding story and the
  inspectable-file property that carries the "reviewable" claim.
- **Spec-first (define the format, then implement).** Governance metadata is far harder to specify
  portably than markdown prose, and the policy's real requirements are not yet known.

---

## 4. Architecture

### Workspace layout

```
memorysafe/                          cargo workspace
  crates/
    memorysafe-core                  types + traits, no I/O
    memorysafe-backend               Backend trait + conformance suite
    memorysafe-backend-sqlite        OSS backend
    memorysafe-embed                 Embedder trait + model2vec / fastembed / test impls
    memorysafe-policy                BaselinePolicy (OSS)
    memorysafe-engine                orchestration: write / read / maintenance pipelines
    memorysafe-mcp                   rmcp adapter, stdio + streamable HTTP
    memorysafe-api                   axum HTTP adapter
    memorysafe-cli                   `msafe` binary
    memorysafe-shadow                shadow-evaluation harness

  separate closed repositories:
    memorysafe-backend-postgres
    memorysafe-policy-governed
```

### Dependency direction

```
core  ←  backend  ←  backend-sqlite
  ↑         ↑
  |         └────────  backend-postgres  (closed)
  |
  ├──  embed
  ├──  policy            ←  policy-governed  (closed)
  └──  engine  ──────────────┐
                             ├──  mcp
                             ├──  api
                             ├──  cli
                             └──  shadow
```

`memorysafe-core` depends on nothing but serde, time, and error plumbing. Adapters depend only on
`memorysafe-engine`. No adapter may reach past the engine into a backend.

### Technology choices

| Concern | Choice | Notes |
|---|---|---|
| OSS storage | `rusqlite` (bundled SQLite), WAL mode | One database file per tenant. |
| Commercial storage | `sqlx` + Postgres + `pgvector` | Partitioned by `tenant_id` with RLS by default. |
| Keyword search | FTS5 (SQLite) / `tsvector` + GIN (Postgres) | Each engine's native facility. No third search system. |
| Vector search | Brute-force SIMD over int8-quantised vectors, f32 rerank (SQLite); pgvector HNSW with the same quantise-then-rerank strategy (Postgres) | Sub-millisecond at ~10k vectors, single-digit ms at ~100k. |
| Embeddings (default) | `model2vec-rs` | Static distilled embeddings, pure Rust, no ONNX runtime, microsecond-scale encoding. |
| Embeddings (quality tier) | `fastembed` with `nomic-embed-text-v1.5` | Opt-in feature flag. Memoryfield-compatible. |
| Embeddings (test) | Deterministic hash-based embedder | Tests must not require model files. |
| In-process cache | `moka` | Hot vector blocks per scope, recent assessments, composed working sets. |
| MCP | `rmcp` v3 | stdio and streamable HTTP. |
| HTTP | `axum` | |
| IDs | ULID | Sortable, no coordination. |
| Errors | `thiserror` in libraries, `anyhow` at binary edges | |

---

## 5. Data model

All types live in `memorysafe-core`.

### Scope

```rust
pub struct TenantId(String);    // isolation unit
pub struct SubjectId(String);   // delete / export unit
pub struct Namespace(String);   // budget / retrieval-default unit

pub struct Scope {
    pub tenant: TenantId,
    pub subject: SubjectId,
    pub namespace: Namespace,
}
```

### Memory item

```rust
pub struct MemoryItem {
    pub id: ItemId,                          // ULID
    pub scope: Scope,
    pub body: String,
    pub kind: String,                        // free-form; conventions documented, not enforced
    pub source: Source,                      // agent | session | tool | human
    pub occurred_at: Option<Timestamp>,      // when the thing happened
    pub created_at: Timestamp,               // when it was written
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, Value>,      // caller-defined structured metadata
    pub sensitivity_hint: Option<SensitivityLevel>,
    pub ttl: Option<Duration>,
    pub protection: Protection,              // engine-maintained; see below
}
```

`kind` is a free string rather than an enum. A taxonomy fixed now would be wrong by v2; the policy
can key off documented conventions (`fact`, `preference`, `event`, `procedure`, `entity`) without
the type system freezing them.

`protection` is **engine-maintained state, not a caller-supplied field**. On admission it is set
from `Action::Retain { protection }`. It changes afterwards only through `memory_protect` — an
explicit human or agent intent, recorded as its own audit event — or through a `maintain` decision
that expires a `Protected { until }` window. A `remember` call may not set it directly. This keeps
a single writer for the field and makes "why is this pinned?" answerable from the audit trail
alone.

`Pinned` is absolute: no policy may evict a pinned item, and the engine refuses any decision that
tries (§9). `Protected { until }` is a time-boxed exemption from capacity eviction that expires on
its own.

### Assessment

Item-level dimensions are separate from capacity, which is a property of the scope.

```rust
pub struct Assessment {
    pub value: Score,                        // f32 in [0,1]
    pub fragility: Score,                    // rare, atypical, costly to recover
    pub sensitivity: SensitivityAssessment,
    pub redundancy: RedundancyAssessment,
    pub features: FeatureMap,                // the numbers behind the scores
    pub assessor: AssessorId,                // policy name + version
}

pub struct SensitivityAssessment {
    pub level: SensitivityLevel,             // Public | Internal | Personal | Sensitive | Restricted
    pub categories: Vec<SensitivityCategory>,// Pii | Health | Financial | Credential | Legal | Other
    pub confidence: f32,
}

pub struct RedundancyAssessment {
    pub score: Score,
    pub near_duplicates: Vec<(ItemId, f32)>,
}

pub struct CapacityState {
    pub budget: Budget,                      // max_items and/or max_bytes for the namespace
    pub used_items: u64,
    pub used_bytes: u64,
    pub pressure: f32,                       // 0..1
}
```

### Decision

```rust
pub enum Action {
    Retain { protection: Protection },
    Merge  { into: ItemId, strategy: MergeStrategy },
    Reject,
}

pub struct Decision {
    pub action: Action,
    pub evictions: Vec<Eviction>,            // items to forget to fit capacity
    pub reasons: Vec<Reason>,
    pub policy: PolicyId,                    // name + version
}

pub struct Reason {
    pub code: ReasonCode,                    // enum, machine-queryable
    pub detail: String,                      // human-readable
    pub evidence: FeatureMap,                // the numbers behind it
}
```

`ReasonCode` variants at v1: `NovelContent`, `HighValue`, `HighRedundancy`, `ExactDuplicate`,
`CapacityPressure`, `ProtectedFragile`, `SensitivityCap`, `SensitivityConflict`, `TtlExpired`,
`Pinned`, `LowValue`, `ReplayDue`, `DiversityCut`, `BudgetExhausted`, `PolicyInvalid`.

### Audit

```rust
pub struct AuditRecord {
    pub id: AuditId,
    pub at: Timestamp,
    pub scope: Scope,
    pub event: AuditEvent,   // Admitted | Rejected | Merged | Forgotten | Recalled
                             // | Exported | Imported | SubjectPurged | Reembedded
                             // | PolicyChanged | MaintenanceRun
    pub items: Vec<ItemRef>, // id + content digest, never the body
    pub assessment: Option<Assessment>,
    pub decision: Option<Decision>,
    pub actor: Actor,        // api key / agent id / session id / cli
}
```

Audit rows never store item bodies — only ids, content digests, and feature numbers.

### Working set

```rust
pub struct WorkingSet {
    pub items: Vec<SelectedItem>,   // each with reason + score breakdown
    pub budget_used: Budget,
    pub omitted: Vec<OmittedItem>,  // considered and cut, with reason; capped at 50
    pub audit_id: AuditId,
}
```

---

## 6. The governance contract

```rust
pub trait GovernancePolicy: Send + Sync {
    fn id(&self) -> PolicyId;

    /// Score a candidate. No I/O — everything needed is in ctx.
    fn assess(&self, cand: &Candidate, ctx: &AssessContext) -> Result<Assessment>;

    /// Decide what happens to an assessed candidate under capacity pressure.
    fn admit(&self, assessed: &Assessed, ctx: &AdmitContext) -> Result<Decision>;

    /// Compose the working set for a read, under a budget.
    fn compose(
        &self,
        req: &RecallRequest,
        candidates: &[ScoredCandidate],
        ctx: &ComposeContext,
    ) -> Result<WorkingSet>;

    /// Periodic maintenance: decay, re-scoring, consolidation, reclaim.
    fn maintain(&self, ctx: &MaintainContext) -> Result<Vec<Decision>>;
}
```

**Policies are pure. The engine performs all I/O.** Near-duplicate neighbours, capacity state,
scope statistics, and the clock are supplied through the context structs. Three properties follow:

1. Policies are unit-testable against fixtures with no database.
2. An audit log can be replayed against a new policy version to diff what would have changed —
   shadow evaluation, which is what makes a governance product improvable without gambling on
   customer data.
3. The proprietary crate has no I/O capability at all. For a closed component inside a self-hosted
   privacy product, that is an unusually strong and *verifiable* trust claim.

### Baseline policy (OSS)

Simple, documented, and defensible — not deliberately crippled. The proprietary version earns its
keep with learned scorers and cross-tenant calibration, not by the baseline being bad.

| Dimension | Baseline computation |
|---|---|
| `value` | Weighted combination of content specificity (inverse token frequency against the scope corpus), source trust weight, explicit caller weight, and recency. |
| `fragility` | Neighbourhood sparsity in embedding space — an item with few near neighbours is atypical and expensive to relearn — combined with access-recovery cost. |
| `sensitivity` | Pattern detectors (credential shapes, government/account id formats) plus category lexicons (health, financial, legal), unioned with the caller's `sensitivity_hint`. The hint may only *raise* the level, never lower it. |
| `redundancy` | Maximum cosine similarity over retrieved neighbours. Above `merge_threshold` (default 0.93) → `Merge`; above `duplicate_threshold` (default 0.98) → `Reject` as exact duplicate. |
| `capacity` | Under pressure, evict ascending by `value × (1 − fragility)`. Never evict `Pinned`. Honour `Protected { until }` windows. |
| `compose` | Maximal Marginal Relevance over fused hybrid scores, weighted by `value`, with a replay quota (default 20% of budget) reserved for high-fragility or long-unaccessed items, packed to the token budget. |

**Fragility/sensitivity conflicts** (an item that is both highly fragile and highly sensitive) are
resolved explicitly: the baseline retains it, applies `Protected`, and records
`ReasonCode::SensitivityConflict` with both scores as evidence. The decision is visible rather than
implicit.

All thresholds are configurable per tenant, with the defaults above.

---

## 7. Data flow

### Write — `remember`

1. **Validate.** Scope exists, body within size limit, `idempotency_key` checked. Assign ULID.
2. **Embed.** `Embedder::embed(body)`, cached by content hash.
3. **Gather context.** One backend round trip: k nearest neighbours in scope, capacity state for
   the namespace, scope statistics.
4. **Assess.** `policy.assess(candidate, ctx)` — pure.
5. **Admit.** `policy.admit(assessed, ctx)` — pure.
6. **Validate the decision** (see §9).
7. **Apply.** One backend transaction: insert or merge item + vector, execute evictions, write the
   audit record. Atomic, read-your-writes.
8. **Invalidate.** Drop affected cache entries for the scope.

```rust
pub struct WriteOutcome {
    pub item_id: ItemId,
    pub action: Action,
    pub reasons: Vec<Reason>,
    pub merged_into: Option<ItemId>,
    pub evicted: Vec<ItemId>,
    pub audit_id: AuditId,
}
```

An agent learning that its memory was rejected as redundant, or merged into an existing item, is
the product working. This surfaces through MCP and HTTP as a **successful** response.

### Read — `recall`

1. **Validate.** Scope, budget, caller's sensitivity clearance.
2. **Embed the query**, cached.
3. **Retrieve candidates.** `backend.retrieve_candidates` — hybrid vector + keyword, fused,
   over-fetching 5–10× the budget. Hard filters (scope, tags, time bounds, sensitivity ceiling)
   execute **in the backend query**.
4. **Compose.** `policy.compose` packs the working set under the token budget.
5. **Record.** Write the recall audit record: what was returned, what was omitted and why, budget
   used.
6. **Update access statistics** asynchronously. These feed future fragility and value scoring and
   must never block the read.

**Hard filters run below the policy.** The policy can only narrow the candidate set, never widen
it. A restricted item never enters process memory. A bug in a closed scorer cannot become a data
leak.

### Maintenance

An **explicit, resumable job with a cursor** — not a background thread that quietly mutates state.
`msafe maintain` locally; a scheduler in the hosted tier.

Steps: TTL expiry → decay re-scoring (value decays; fragility can *rise* as neighbours are removed)
→ consolidation of merge candidates among existing items → capacity reclaim for over-budget
namespaces → re-embedding migrations.

`policy.maintain` returns decisions applied through the same atomic, audited path. Nothing changes
without an audit record; nothing runs unobservably.

### Concurrency and correctness

- **SQLite:** WAL mode; one writer per tenant file, many readers. Writes serialised through a
  per-tenant queue in the tenant manager. Connections held in an LRU pool so N tenants do not mean
  N open files.
- **Postgres:** normal transactions.
- **Capacity accounting:** a per-namespace accounting row is locked for the duration of an admit
  transaction (`SELECT … FOR UPDATE` on Postgres, serialised writes on SQLite). Without this, two
  concurrent writes both conclude there is room and the budget is silently exceeded.
- **Idempotency:** `remember` takes an `idempotency_key`. A retried write returns the original
  outcome rather than admitting a duplicate and evicting something to make room for it. Reuse of a
  key with a *different* payload is a `Conflict`.

### Embedding model changes

Vectors from different embedders are not comparable. Every vector row records `embedder_id` and
`dim`. The backend refuses cross-model comparison. Changing embedder is an explicit, resumable
re-embedding migration, audited as `Reembedded` — never a silent recall degradation.

---

## 8. Backend contract

```rust
#[async_trait]
pub trait Backend: Send + Sync {
    async fn retrieve_candidates(
        &self,
        scope: &Scope,
        query: &CandidateQuery,   // embedding, text, hard filters, over-fetch limit
    ) -> Result<Vec<ScoredCandidate>>;

    async fn neighbours(&self, scope: &Scope, embedding: &Embedding, k: usize)
        -> Result<Vec<ScoredCandidate>>;

    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState>;
    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats>;

    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite>;  // atomic
    async fn record_recall(&self, record: AuditRecord) -> Result<AuditId>;

    async fn get(&self, scope: &Scope, id: &ItemId) -> Result<Option<MemoryItem>>;
    async fn list(&self, scope: &Scope, page: &Page) -> Result<Vec<MemoryItem>>;
    async fn audit(&self, scope: &Scope, filter: &AuditFilter) -> Result<Vec<AuditRecord>>;

    async fn purge_subject(&self, tenant: &TenantId, subject: &SubjectId) -> Result<PurgeReport>;
    async fn export(&self, scope: &ScopeSelector) -> Result<ExportStream>;
    async fn import(&self, destination: &TenantId, stream: ImportStream) -> Result<ImportReport>;
}
```

The trait is `async` throughout. Postgres is network I/O, and a synchronous trait retrofitted to
async is a rewrite. The SQLite implementation wraps blocking calls in `spawn_blocking`.

`Store` and `Index` are deliberately **one trait**. In SQLite they are separable — brute-force
search over memory-mapped quantised vectors in Rust. In Postgres they are not: pgvector searches
inside the database and hybrid ranking wants to be one query. A separate index trait would force
the Postgres implementation to fake an in-process index.

### Tenant layout

- **SQLite:** one database file per tenant. Backup is `cp`; tenant deletion is `rm`; per-tenant
  encryption is a key per file.
- **Postgres:** shared schema with declarative partitioning by `tenant_id` plus row-level security
  by default; schema-per-tenant available as a configuration option for regulated customers. Both
  behind one implementation.

### Conformance suite

Lives in `memorysafe-backend` and runs against every backend in CI. It is the load-bearing artifact
of the two-backend design — without it the implementations drift within a month and the trait
becomes a lie.

Covered behaviours: cross-tenant and cross-subject isolation; atomicity of admit + evict + audit;
hard-filter semantics including the sensitivity ceiling; ranking tie-breaking; pagination
stability; idempotency-key semantics; capacity accounting under concurrent writes; deletion
completeness for `purge_subject`; export→import round-trip fidelity; cross-model vector rejection.

---

## 9. Error handling

**A policy decision is not a failure.** A rejected or merged write returns `Ok(WriteOutcome)`.
`Err` is reserved for things that actually went wrong.

| Error | HTTP | Notes |
|---|---|---|
| `Validation` | 400 | Bad scope, oversize item, malformed filter. |
| `Auth` | 401 / 403 | Unknown key; scope not permitted for key. |
| `NotFound` | 404 | |
| `Conflict` | 409 | Idempotency key reused with a different payload; concurrent modification. |
| `Backend` | 503 | Carries a `retryable` flag. |
| `Embedder` | — | Degrades rather than fails; see below. |
| `Policy` | 500 or degraded | See below. |

### Misbehaving policies

The policy seam is pluggable and one implementation is closed-source, so the engine **validates
every returned decision before applying it**: evictions must be in-scope and unpinned, scores must
be in range, merge targets must exist, and the composed working set must contain only candidates
that were supplied. Policy calls run under `catch_unwind`.

An invalid decision is refused and audited with `ReasonCode::PolicyInvalid`, then handled per the
configured stance:

- `fail_closed` — reject the write / return an empty working set.
- `fail_safe` (default) — fall back to `BaselinePolicy` for that call.

### Unavailable embedder

A missing or failed model must never cost a user their memory. The item is admitted with a
`pending_embedding` flag and queued for backfill. It is excluded from vector retrieval until
backfilled but remains available to keyword retrieval and to `review`.

---

## 10. Security and isolation

- Tenant isolation is structural (file per tenant / partition + RLS), not a query-layer invariant.
- API keys are scoped to a tenant. Subject and namespace are supplied per request and validated
  against the key's tenant.
- Sensitivity ceilings are enforced in the backend query, below the policy.
- Audit rows never contain item bodies.
- `purge_subject` is a first-class backend operation, not a scan-and-delete loop, and returns a
  `PurgeReport` counting what was removed.
- The proprietary policy crate is compiled without I/O capability; its inputs arrive entirely
  through context structs.

---

## 11. Audit and retention

Audit retention is configured **per tenant** through named profiles. Free-form overrides are
permitted but unsupported; the profiles are what is tested and documented.

```rust
pub struct AuditRetention {
    pub detail: RetentionSpan,        // Forever | Days(n) | UntilSubjectPurge
    pub purge_cascade: PurgeCascade,  // Cascade | Preserve
    pub aggregate: RetentionSpan,     // Forever | Days(n)
}
```

| Profile | `detail` | `purge_cascade` | `aggregate` | Intent |
|---|---|---|---|---|
| `balanced` (default) | `UntilSubjectPurge` | `Cascade` | `Forever` | Decision detail dies with the subject; non-identifying aggregates persist. |
| `gdpr_strict` | `Days(90)` | `Cascade` | `Days(365)` | Minimal retention; everything expires. |
| `hipaa_retain` | `Days(2190)` | `Preserve` | `Forever` | Six-year detail retention; purge does not cascade. |
| `forensic` | `Forever` | `Preserve` | `Forever` | Maximum governance evidence. |

**Aggregates** are counts, rates, and score distributions grouped by policy version — never
identifying. They are what makes policy behaviour evaluable over time regardless of profile.

`PolicyChanged` audit events record the transition between policy versions so that any decision
can be attributed to the policy that made it.

---

## 12. Interfaces

### MCP — five tools

Serial tool calls are where agent memory dies, so the surface stays small and each call does real
work.

| Tool | Purpose |
|---|---|
| `memory_recall` | Primary call. Returns a governed working set under a token budget. A `mode` parameter switches to raw ranked search — still filtered, still audited. |
| `memory_remember` | Writes an item; returns the governance decision. |
| `memory_forget` | Explicit deletion by id or query. |
| `memory_review` | Lists what is stored and why. This is where "reviewable rather than invisible" becomes something an agent can show a user. |
| `memory_protect` | Pins or protects an existing item. |

The audit trail and scope statistics are exposed as MCP **resources**
(`memorysafe://{tenant}/{subject}/{namespace}/audit`), so clients can display them without spending
a tool call.

**Scope resolution by transport:**

- **stdio** — the server is configured with tenant and subject; namespace defaults from the working
  directory and may be overridden per call.
- **streamable HTTP** — the API key identifies the tenant; the request carries subject and
  namespace.

### HTTP

A thin axum mirror of the engine.

```
POST   /v1/recall
POST   /v1/memories                 remember
GET    /v1/memories                 review
GET    /v1/memories/:id
DELETE /v1/memories/:id
POST   /v1/forget                   by query
POST   /v1/memories/:id/protect
GET    /v1/audit
POST   /v1/maintain
GET    /v1/export
POST   /v1/import
DELETE /v1/subjects/:id             purge
GET|PUT /v1/admin/tenants/:id/budgets
GET|PUT /v1/admin/tenants/:id/policy
GET|PUT /v1/admin/tenants/:id/retention
```

Authentication is `Authorization: Bearer <api-key>`, one key to one tenant.

### CLI — `msafe`

```
msafe serve --transport stdio|http
msafe remember / recall / review / forget / protect
msafe audit
msafe maintain
msafe export / import
msafe shadow <audit-log> --policy <impl>     replay and diff decisions
```

### Portable export/import

A directory or ZIP archive containing items, assessments, decisions, and audit records as
newline-delimited JSON, plus a rendered markdown view of the items for human reading. Round-trip
fidelity is a tested invariant. This delivers memoryfield's anti-lock-in payoff without publishing
a format specification before the policy's needs are understood.

---

## 13. Testing strategy

### Invariants (proptest)

These five properties define correctness:

1. Capacity is never exceeded after any sequence of writes.
2. `Pinned` items are never evicted.
3. The sensitivity ceiling is never violated by a recall result.
4. Every mutation produces exactly one audit record.
5. Export → import round-trips exactly.

### Layers

- **Backend conformance suite** — run against SQLite and Postgres in CI (§8).
- **Policy golden fixtures** — a corpus plus an ordered sequence of writes, asserting exact
  decisions under a fixed clock and the deterministic embedder.
- **Deterministic hash-based test embedder** — a prerequisite for everything above. Tests must not
  require model files or network access.
- **Isolation tests** — cross-tenant and cross-subject access must fail, on both backends.
- **MCP integration tests** — drive the stdio server with a real MCP client.
- **Shadow evaluation harness** (`memorysafe-shadow`) — replay an audit log against a different
  policy version and diff the decisions. Built in v1 even though nothing proprietary exists yet to
  evaluate; it is both a test tool and, later, a product feature.

Development follows TDD: tests before implementation at every layer.

---

## 14. v1 scope

### In

- `memorysafe-core`: types and traits.
- `memorysafe-backend`: trait and conformance suite.
- `memorysafe-backend-sqlite`: the OSS backend.
- `memorysafe-backend-postgres`: the commercial backend (separate repo, same suite).
- `memorysafe-policy`: `BaselinePolicy`, all four methods.
- `memorysafe-embed`: model2vec default, deterministic test embedder, fastembed behind a feature.
- `memorysafe-engine`: remember, recall, forget, review, protect, maintain, export, import,
  purge_subject.
- `memorysafe-mcp`: five tools, audit resources, stdio and streamable HTTP.
- `memorysafe-api`: the HTTP surface above.
- `memorysafe-cli`: `msafe`.
- `memorysafe-shadow`: the evaluation harness.
- Audit with the four retention profiles.
- Portable export/import.

### Out

- `memorysafe-policy-governed` — the proprietary scorer. Separate repo, ships against the finished
  trait.
- ANN vector indexes.
- Cloud control plane, billing, provisioning, web UI.
- The continual-learning product line.
- Cross-subject search, shared multi-agent memory, org-wide knowledge.
- LLM extraction from raw transcripts.

---

## 15. Deferred decisions

Recorded so they are not rediscovered as surprises. Each has a v1 default that does not foreclose
the alternative.

| Question | v1 default | Revisit when |
|---|---|---|
| Where terabytes accumulate — many small subjects, enterprise whales, or archive | Postgres scaling tier; no sharding | Real customer data shapes are known. |
| Whether a single subject can exceed brute-force viability | `Backend::retrieve_candidates` boundary permits a per-scope ANN index | A scope exceeds ~10^5 items. |
| Cross-subject / org-wide retrieval | Not supported | A customer needs shared knowledge across end-users. |
| Merge strategy sophistication | Concatenate-and-dedupe with provenance union | Consolidation quality becomes a complaint. |
| Publishing the export format as a spec | Documented but unversioned | The format has survived two policy generations. |
| Postgres tenant layout default | Partition + RLS; schema-per-tenant configurable | A regulated customer requires it contractually. |
