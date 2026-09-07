# MemorySafe Hosted Deployment — Design

**Date:** 2026-09-07
**Status:** Approved design, ready for implementation planning
**Scope:** The commercial hosted service — how MemorySafe is packaged, provisioned, deployed, and operated for paying and evaluating clients.

> **Where this file lives.** This document describes **closed, commercial** infrastructure: the
> hosting topology, the control plane, the tenant provisioning model, and the operational
> contract of the hosted service. It sits in `.superpowers/closed-tier/` alongside
> `2026-09-05-postgres-backend.md` (Plan 2) for the same reason and under the same terms — when
> Plan 2's Task 1 creates the `memorysafe-cloud` repository, **both files move into it** and must
> not remain here. The open-source repository is published; this content is the hosted tier's
> substance.

---

## 1. What this covers, and what it does not

MemorySafe today is seven library crates on `master`, all complete and tested. There is no
binary, no server, and no deployable artifact. Two plans already exist to change that:

| Plan | Subject | State |
|---|---|---|
| Plan 1 | `memorysafe-engine` + `memorysafe-backend-sqlite` | Merged to `master` |
| Plan 2 | `memorysafe-backend-postgres` (closed tier) | Planned, not implemented. Plan doc at `.superpowers/closed-tier/2026-09-05-postgres-backend.md` on branch `sdd/postgres-backend` |
| Plan 3 | Adapters — MCP, HTTP API, `msafe` CLI, shadow harness | In progress on branch `worktree-sdd-plan3-resume`. `memorysafe-mcp` exists and its five-tool surface has landed (through Task 4 of 18); `memorysafe-api`, `memorysafe-cli`, and `memorysafe-shadow` not yet started |

**This document is the fourth piece.** It specifies the hosted layer that sits on top of Plans 2
and 3: the control plane, the deployable artifact, the cluster topology, and the operational
contract. It assumes Plans 2 and 3 land as written.

**It does not** re-specify the Postgres backend (Plan 2 owns that), the MCP/HTTP/CLI adapters
(Plan 3 owns those), or the governance engine (Plan 1, done). Where this document depends on a
guarantee from one of those plans, it cites it rather than restating it.

**Explicitly out of scope for the first deployment:** billing and payment, usage metering for
invoicing, password or email authentication, team/invite management, multi-region, high
availability, point-in-time recovery, and EU-specific data residency. Each is a deliberate
deferral recorded in §2, not an oversight.

---

## 2. Decisions

Every row here was decided during brainstorming and is settled. The rationale column exists so a
future reader can tell which decisions were load-bearing and which were preference.

| # | Decision | Rationale |
|---|---|---|
| D1 | The hosted product exposes **both** an MCP endpoint and a REST API from one binary, with **MCP as the headline** | Both are thin adapters over the same `Engine` calls, so the second surface is nearly free. MCP is what makes the product adoptable in an afternoon. |
| D2 | **Shared tenancy now**, dedicated instances for large clients later | Marginal cost per client approaches zero; the dedicated path is already designed (§8.4). |
| D3 | Storage is **PostgreSQL**, via Plan 2's `memorysafe-backend-postgres` | Replaces file-per-tenant SQLite for the hosted tier. SQLite remains the embedded/offline story. |
| D4 | Tenant isolation via **`PgLayout::SharedPartitioned`** — one schema, tables `PARTITION BY HASH (tenant_id)`, RLS enforced | Already Plan 2's default layout. Hash partitioning bounds the working set an HNSW scan touches; RLS makes isolation independent of query text. |
| D5 | **Open core.** Engine crates and adapters stay public under Apache-2.0; Postgres backend, control plane, and infrastructure are closed | An auditable governance policy is a sales asset for a product sold on privacy. The moat is the hosted service, not the scoring algorithm. |
| D6 | **Self-serve signup, GitHub OAuth only, no billing** | Developer audience already has GitHub. No password store means no recovery, verification, or reset surface to build or secure. |
| D7 | Tenants are **per GitHub user and per GitHub org** (both), derived from **numeric** GitHub ids | Personal tenants keep evaluation friction near zero; org tenants match how businesses buy. Numeric ids because logins are renameable and reusable. |
| D8 | Hosting is a **single VPS running k3s**, self-hosted Postgres in-cluster. Host: **repurpose the existing OVH box `can-1`**, subject to the preconditions in §13.0; fallback is Hetzner CX33 at ~$10.45/mo | One bill, and zero marginal cost on hardware already paid for. Self-hosting pins PostgreSQL 17 + pgvector 0.8.6, exactly the versions Plan 2 was validated against. |
| D9 | **k3s over Docker Compose** | Compose → Kubernetes is a rewrite; k3s → multi-node or managed Kubernetes is incremental. Also gives CronJob, Job, probes, and rolling deploys as primitives. |
| D10 | **No PITR, no replica, no EU residency requirement** — but a **nightly logical backup is mandatory** | Deferred deliberately for cost. A backup is not optional: the product *is* customer memory, and it is not reconstructible from any other source. |

---

## 3. Repository and artifact structure

Two repositories, as Plan 2 already specifies.

### 3.1 `memorysafe` — public, Apache-2.0

The seven engine crates plus Plan 3's adapters:

```
memorysafe-core, memorysafe-embed, memorysafe-backend,
memorysafe-backend-sqlite, memorysafe-policy, memorysafe-engine,
memorysafe-auth, memorysafe-mcp, memorysafe-api,
memorysafe-cli, memorysafe-shadow
```

Note the consequence of D5: **the MCP server and the REST API are open source.** Only the
scaling backend and the hosted layer are not. A developer can self-host MemorySafe on SQLite and
read exactly what the API does — which is the trust argument the product is sold on — while the
commercial tier remains closed.

**Required fix:** the repository has no `LICENSE` file despite the README carrying an Apache-2.0
badge. Add one before the repository is published.

### 3.2 `memorysafe-cloud` — private

Contains `memorysafe-backend-postgres` (Plan 2), the control plane, the deployable binary, and
the k3s manifests. It vendors the public repository as a git submodule at `vendor/memorysafe`
and consumes it through path dependencies.

```
memorysafe-cloud/
  vendor/memorysafe/                  # git submodule, read-only
  crates/
    memorysafe-backend-postgres/      # Plan 2
    memorysafe-cloud-control/         # control plane: OAuth, tenants, keys, dashboard
    memorysafe-cloud-server/          # the binary: composition root
  deploy/
    base/                             # kustomize base
    overlays/production/
  Cargo.toml
```

### 3.3 The hosted binary is a new composition root

Plan 3 constrains adapters to depend on `memorysafe-engine` and never on a backend —
`memorysafe-cli` is the only crate permitted to choose one. That constraint is what makes this
clean: `memorysafe-cloud-server` is a *second* composition root that wires
`PostgresBackend` + embedder + `BaselinePolicy` into an `Arc<Engine>` and mounts the
open-source adapters over it, without modifying them. The public `msafe` CLI keeps choosing
SQLite.

If `memorysafe-cloud-server` ever needs to change an adapter to work, that is a signal the
adapter leaked a backend assumption, and the fix belongs in the public repository under Plan 3's
review — not a patch in the closed one.

---

## 4. Runtime architecture

One container image, one process, three routers on one port:

| Path | Served by | Notes |
|---|---|---|
| `/mcp` | `memorysafe-mcp` streamable-HTTP | The headline surface |
| `/v1` | `memorysafe-api` | REST routes |
| `/` | `memorysafe-cloud-control` (new) | OAuth, dashboard, key management |

Behind it, one PostgreSQL 17 instance holding two schemas:

- **the tenant schema** — Plan 2's DDL. RLS on with `FORCE ROW LEVEL SECURITY`, tables hash-partitioned 16 ways by `tenant_id`, pool connecting as a `NOSUPERUSER NOBYPASSRLS` role.
- **`control`** — GitHub identity, tenant registry, API keys. No RLS, no partitioning.

### 4.1 The app tier is stateless

This is the largest single consequence of moving from SQLite to Postgres, and it shapes
everything downstream. No volumes on the app, no node affinity, no sticky routing. Scaling is
"run more replicas"; deploying is an ordinary rolling replacement.

The one exception is the embedding model: `model2vec` weights are baked into the image at build
time. There is no download at boot and no network call on the write path, which preserves the
engine's offline-write guarantee.

### 4.2 Why transaction-mode pooling is safe

Plan 2 sets `memorysafe.tenant_id` with `set_config(..., is_local => true)`, so the GUC dies with
the transaction and cannot leak to the next borrower of a pooled connection. PgBouncer in
transaction mode is therefore safe. Session-mode pooling would have forced a connection per
tenant, which does not scale past a few hundred clients. This is a property to preserve, not an
accident: any future code path that sets a session-level GUC breaks it.

---

## 5. Control plane

### 5.1 Authentication

- `GET /login` redirects to GitHub's OAuth authorize endpoint.
- `GET /auth/callback` exchanges the code, fetches `/user` and `/user/orgs`, and sets a signed session cookie.
- No session table. No password store. No recovery, verification, or reset flow.

### 5.2 Tenants

Tenant ids derive from GitHub's **numeric** id, never the login:

- `u_<github_user_id>` — personal tenant, created on first login.
- `o_<github_org_id>` — organisation tenant, selectable if the user belongs to the org.

**Logins must never be used as identity.** GitHub logins can be renamed and the freed name
re-registered by someone else. A tenant keyed on a login would silently become a different
tenant on rename, or inherit a recycled name's data — a cross-customer leak. The login may be
cached for display, and must be refreshed rather than trusted.

**Org membership is re-fetched from GitHub per session, never stored.** Storing it means
revoking someone's GitHub org access does not revoke their access to that tenant's memories.
The staleness window must be a session, not a cache TTL.

> **Correction, from the Plan 4 implementation review.** The paragraph above does not survive
> contact with §5.1's own design, and stating it as written would have shipped a false guarantee.
> A signed session cookie with a seven-day `MAX_AGE` **is** a cache with a seven-day TTL. Membership
> is read from GitHub once, at login, and then carried in the cookie until it expires — so removing
> someone from a GitHub org leaves them able to act as that org's tenant for up to a week.
>
> Worse, and this is the part that makes it more than a staleness question: **an API key minted
> during that window has no expiry and no link to the membership that authorised it.** Revocation
> from the GitHub org therefore leaves the key working *permanently*, not for a week. The blast
> radius of a stale session is not one session; it is every credential created from it.
>
> Three ways to close it, in ascending cost, to be decided before login is exposed to real users:
> 1. **Shorten the session** — minutes, not days — and accept a GitHub round trip per renewal. Bounds the window but does not touch keys already minted.
> 2. **Re-verify membership at mint time**, so a key for an org tenant is only ever created against a membership confirmed against GitHub in that request. Closes the permanent-key path, which is the serious half.
> 3. **Bind keys to the membership** and re-check on use, or expire org-tenant keys. Correct, and the most expensive.
>
> Option 2 is the minimum that removes the permanent grant, and it is cheap — one API call on a
> path that already talks to GitHub. Nothing in Plan 4 implements any of them; the code is
> consistent with §5.1, and it is this paragraph that was wrong.

### 5.3 Provisioning is free

Under `SharedPartitioned`, a new tenant is just rows with a new `tenant_id`. No `CREATE SCHEMA`,
no DDL, no migration, no partition management — the hash partitions already exist. Signup is:
insert one `control` row, mint one key.

This is a direct payoff from D4 and is what makes self-serve viable at this cost. Under
`SchemaPerTenant` (§8.4) provisioning would require DDL, which is exactly why that layout is
reserved for the small number of dedicated clients.

### 5.4 Dashboard

Pick tenant, list/create/revoke API keys, view audit aggregates.

Audit aggregates are the right source for "what has my agent been storing": the
`audit_aggregates` table is keyed by policy name and version, event class, and day bucket, with
**no subject and no namespace column**, and is designed to survive `purge_subject`. It gives
useful operational visibility without ever exposing memory bodies.

---

## 6. The API key persistence gap

`memorysafe-auth` today is:

```rust
pub struct ApiKeyStore { by_id: HashMap<String, ApiKeyRecord> }   // private field
impl ApiKeyStore {
    pub fn new(records: Vec<ApiKeyRecord>) -> Self;
    pub fn records(&self) -> impl Iterator<Item = &ApiKeyRecord>;
    pub fn authenticate(&self, presented: &str) -> Result<Authenticated, AuthError>;
}
```

Lookup is **already** `O(1)` — `authenticate` parses the id out of the presented key, hits the
`HashMap`, and does a constant-time hash comparison. Performance is not the problem.

**The problem is freshness.** The store is built from a `Vec` at construction and never changes.
A key created in the dashboard would not authenticate until the process restarted, and with two
replicas a key minted on one would be invisible to the other.

### 6.1 Why the obvious fix is not available

Reading a key row straight from Postgres and constructing an `Authenticated` from it **cannot be
done from the closed repository**, by design:

- `key::parse_presented` and `key::hash_presented` are `pub(crate)`. `lib.rs` exports only `ApiKeyRecord`, `GeneratedKey`, `KEY_PREFIX`, and `generate`.
- `Authenticated`'s fields are private and it has no public constructor. Its doc states the invariant plainly: *"The only way to obtain one is `ApiKeyStore::authenticate`, so a handler that holds one cannot have skipped the check."*

That is a security-by-construction property worth keeping, not an oversight to route around.

### 6.2 Resolution: persist the records, keep the store

`memorysafe-cloud-control` owns persistence of `ApiKeyRecord` rows in `control.api_keys`. It does
**not** reimplement authentication. Instead it holds an `ArcSwap<ApiKeyStore>` and rebuilds the
store from the table:

- at startup;
- on `LISTEN`/`NOTIFY` from Postgres, published by whichever instance mutated a key;
- on a periodic refresh (30 s) as a backstop in case a notification is missed across a reconnect.

Authentication remains `ApiKeyStore::authenticate` verbatim. `memorysafe-auth` is unchanged, its
invariant intact, and the hot path stays a `HashMap` hit with no database round-trip.

Memory is a non-issue: an `ApiKeyRecord` is on the order of 150 bytes, so 100 000 keys is a few
megabytes.

**The trade-off, stated explicitly.** Revocation is eventually consistent across replicas — bounded
by notification latency, or by the 30 s refresh if a notification is lost. Creation is likewise
not instantaneous on other replicas. For an API key that is acceptable, and it must be documented
rather than discovered. If a hard, immediate revocation guarantee is required later, *that* is the
moment to propose a trait extraction in `memorysafe-auth` — in the public repository, with its own
review, and not before there is a reason.

---

## 7. Data model additions

New tables, `control` schema only. Tenant-schema DDL is Plan 2's and unchanged.

| Table | Purpose | Notes |
|---|---|---|
| `control.tenants` | Tenant registry | `tenant_id` PK (`u_*`/`o_*`), `github_id`, `kind`, cached `login`, `created_at` |
| `control.api_keys` | Persistent API keys | `key_id` PK, `tenant_id`, BLAKE3 hash, `label`, `created_at`, `disabled_at`, `last_used_at` |

No memberships table (§5.2), no sessions table (§5.1), no users table — a GitHub identity *is*
the user.

---

## 8. Hosting and cluster topology

### 8.1 Provider selection criteria

The design imposes three hard requirements. Cost is the tiebreaker among options that satisfy
all three, not a criterion that can override them.

1. **Root access sufficient to run our own PostgreSQL container.** Plan 2 requires creating a `NOSUPERUSER NOBYPASSRLS` role, `CREATE EXTENSION vector`, and `FORCE ROW LEVEL SECURITY`. A managed Postgres that restricts role creation or extension installation breaks the isolation model outright.
2. **At least 4 GB RAM** (§8.3).
3. **One bill.** Compute and database as separate billable services is explicitly rejected (D8).

Requirements dropped by D10 and therefore *not* selection criteria: EU region, self-serve DPA,
PITR, managed backups, SOC 2 posture inheritance.

Because we run our own Postgres image, pgvector version and role permissions — the two filters
most likely to disqualify a managed provider — stop being provider risks entirely. We pin
PostgreSQL 17 and pgvector 0.8.6, the exact versions Plan 2's DDL, RLS policies, partitioned
foreign key, generated `tsvector` column, and HNSW planner behaviour were validated against.

### 8.2 What actually consumes memory

Sizing questions about this workload usually start from the wrong number, so the relevant facts,
all from Plan 2's DDL and `memorysafe-embed`:

- **Vectors are stored int8-quantized.** `vectors.q BYTEA` is the canonical copy; export and exact scoring both read it. `vectors.embedding vector(<vector_dim>)` is *derived* and exists solely so the HNSW index can generate candidates. There is no `halfvec`-versus-int8 decision outstanding — the design already made it, and round-trip fidelity does not depend on floats surviving pgvector.
- **Embedding width is deployment configuration**, a `u16` probed from the model at load time. The conformance suite pins 256. Nothing in the codebase implies 768, and a wider model is a deliberate configuration change with a known cost, not a surprise.
- **Exact reranking happens in Rust**, via `QuantizedVector::dot` — the same function the SQLite backend scores with, which is what makes the two backends order identically. Postgres does approximate candidate generation only.

At 256 dimensions the derived `vector` column is ~1 KB per row before HNSW graph overhead, so a
corpus in the low hundreds of thousands of vectors is a few hundred megabytes of index — a real
line in the memory budget, but not the dominant one at design-partner scale. The 4 GB floor in
§8.4 is set by k3s plus Postgres plus `maintenance_work_mem` during index builds, not by vector
storage.

### 8.3 Cluster layout

Single-node k3s. Manifests in `deploy/`, applied with `kubectl apply -k`. Kustomize overlays, no
Helm at this size, no GitOps controller until there is a second environment.

| Object | Kind | Notes |
|---|---|---|
| `memorysafe-cloud` | Deployment | Stateless. 1 replica initially, `RollingUpdate`, readiness + liveness probes, resource limits |
| `postgres` | **StatefulSet** | `pgvector/pgvector:pg17`, 1 replica, PVC via k3s `local-path` |
| ingress | Traefik | Bundled with k3s; terminates TLS via Let's Encrypt |
| `bootstrap` | Job | One-shot migration, gated on `SCHEMA_VERSION`, runs before rollout |
| `pg-backup` | CronJob | Nightly `pg_dump` to object storage |
| `ndjson-export` | CronJob | Weekly engine-level export (§9.3) |

### 8.4 Four k3s constraints that must not be got wrong

1. **Postgres is a StatefulSet, never a `RollingUpdate` Deployment.** With a node-local `local-path` PVC, a rolling update briefly runs two pods against one data directory. Best case the new pod fails on the lock; worst case the restore path in §9.3 is exercised for real. A `Deployment` with `strategy: Recreate` is an acceptable alternative; the default is not.
2. **4 GB RAM is the floor.** k3s's control plane costs roughly 500 MB before any workload. Postgres, the Rust binary, and pgvector's HNSW index builds — governed by `maintenance_work_mem` and genuinely memory-hungry — do not fit comfortably in 2 GB alongside it. This is a real cost consequence of D9 and is accepted deliberately.
3. **Start k3s with `--secrets-encryption`.** k3s Secrets are not encrypted at rest by default. The GitHub OAuth client secret and the cookie signing key live there.
4. **A single-node cluster is not high availability.** One box, one disk, shared fate between app and database — identical to Compose in availability terms. What k3s buys is declarative configuration, real deploy primitives, and an incremental path to multi-node. It should never be described internally or externally as redundancy.

### 8.5 The path to dedicated instances

When a client warrants isolation (D2), the move is:

1. Export the tenant with the engine's ndjson export.
2. Provision a namespace with its own `postgres` StatefulSet, using `PgLayout::SchemaPerTenant`.
3. Import, cut the tenant's routing over, purge from the shared instance.

Both layouts run the same conformance suite, so this is a configuration change rather than a
data-model change. Scheduling it on a second joined node instead of the same box is a node
selector, which is the concrete benefit D9 was chosen for.

---

## 9. Operations

### 9.1 Migrations

`bootstrap.rs` (Plan 2) is idempotent and handles extension, role, schema, partitions, grants,
and RLS. It runs as an explicit k8s `Job` gated on `SCHEMA_VERSION`, **before** the rolling
deploy — not on application boot. Idempotency survives a race, but partition creation and index
builds racing across two starting replicas is not a situation worth entering.

### 9.2 Quotas

Self-serve with no billing and no limits is an open invitation to run up a hosting bill. No new
machinery is required: the engine already has `Budget { max_items, max_bytes }` and `set_budget`,
and capacity pressure is a first-class concept the policy already acts on. **Provisioning sets a
default free-tier budget**, and the product's own capacity governance becomes the free-tier
limiter.

A per-key request rate limit at the app layer covers the orthogonal abuse case (volume rather
than volume-of-data).

> **Correction, from the Plan 4 implementation review.** "Provisioning sets a default free-tier
> budget" is true and insufficient, and as written it invites exactly the mistake Plan 4 made:
> provisioning sets a budget on **one scope**, `tenant/default/default`.
>
> But a `Scope` is tenant → subject → namespace, and `Authenticated::scope(subject, namespace)`
> takes both of those **from the request**, rejecting only reserved words. An unbudgeted scope
> carries `Budget::UNLIMITED`. So a caller who simply passes any other namespace writes into an
> unmetered scope, and the free tier bounds one namespace out of infinitely many. The limiter does
> not limit.
>
> This is a design error, not an implementation slip — the sentence above describes a per-tenant
> guarantee while the mechanism it names is per-scope. Two ways to reconcile them, to be settled
> before the API surface is exposed:
> 1. **Budget at the tenant level**, so capacity is a property of the customer rather than of one
>    namespace they happened to use first. This is what the prose already promises and requires
>    checking whether the engine's capacity model supports a tenant-wide budget or only per-scope.
> 2. **Provision on first use of each scope**, applying the free-tier budget whenever a caller
>    reaches a scope that has none. Keeps the per-scope model but removes the unmetered default.
>
> Option 1 matches the intent; option 2 is achievable without touching the engine. Either way the
> per-key rate limit above is the only thing currently bounding a free tenant's *volume*, and
> nothing bounds their *storage* beyond the single default namespace.

### 9.3 Backups

- **Nightly `pg_dump`** to object storage, as a CronJob. Mandatory (D10).
- **Weekly ndjson export** using the engine's own conformance-tested portability format. Provider- and schema-independent, and it doubles as the customer-facing "export my data" feature.
- **A restore must be rehearsed before the first client is onboarded.** An untested backup is not a backup. The rehearsal is a checklist item in §11, not an aspiration.

No PITR, no replica, no standby (D10).

### 9.4 Logging

The audit design's central promise is that records carry ids, BLAKE3 digests, and feature numbers
but never memory bodies. **Logs must hold the same line.** No bodies, no key secrets, no query
text that may contain a body, at any log level including debug.

This gets an automated redaction test, not a code-review convention. It is the one operational
mistake that would directly falsify the product's primary claim, and code review does not catch
a `tracing` field added six months from now.

### 9.5 Secrets

GitHub OAuth client id and secret, session cookie signing key, database URL. k3s Secrets with
`--secrets-encryption` enabled. Never baked into the image, never logged.

---

## 10. Testing

**Inherited, and the strongest guarantee in the stack:** Plan 2's Postgres backend must pass the
conformance suite unmodified, against a real PostgreSQL via testcontainers, under both layouts.

> **Correction to Plan 2 required.** Plan 2 states it must pass a *"frozen 49-test conformance
> suite"* and tabulates it as isolation 5 / atomicity 6 / retrieval 13 / capacity 4 /
> lifecycle 21. The authoritative `run!` invocation in
> `crates/memorysafe-backend/src/conformance/mod.rs` on `master` now runs **55** tests —
> isolation 5, atomicity 8, retrieval 14, capacity 5, lifecycle 23. Six were added after Plan 2
> was written. Plan 2's count and task acceptance criteria must be updated to 55 before
> implementation starts, and the six newer tests brought into scope. Plan 2's own instruction
> applies: *"Recount, do not adjust by a difference."*

New tests the hosted layer requires:

| Test | Why |
|---|---|
| Control-plane integration against a fake GitHub OAuth endpoint | Covers callback, org listing, session issuance |
| Tenant derivation from numeric id | Proves a renamed login neither changes nor collides with a tenant id (§5.2) |
| API key freshness across replicas | A key created against one connection authenticates on another once the `NOTIFY` or the 30 s refresh lands (§6.2), and a revoked one stops authenticating within the same bound |
| Log redaction | No memory body or key secret reaches any log sink at any level (§9.4) |
| RLS enforcement without predicates | A query with its `tenant_id` predicate removed returns zero rows — Plan 2's constraint, asserted at the hosted composition root with its real pool role |
| Post-deploy smoke test | Provision tenant, mint key, remember, recall, read audit |

---

## 11. Rollout

1. Land Plan 3 (adapters) and Plan 2 (Postgres backend, with the corrected test count).
2. Stand up `memorysafe-cloud`: submodule, composition root, control plane, key persistence.
3. **Audit `can-1` against §13.0** — RAM, disk, root access, existing workloads, architecture, DNS. Decide repurpose-or-fallback on the evidence before any deployment work targets it.
4. Install k3s with `--secrets-encryption`.
5. Apply manifests; run `bootstrap` Job; deploy one replica.
6. **Rehearse a restore from the nightly dump.** Gate: no client is onboarded before this passes.
7. Onboard design partners through self-serve signup.
8. Add a second replica when a client's uptime depends on it — a config change, not a project.

---

## 12. Known risks

| Risk | Mitigation | Accepted? |
|---|---|---|
| Single box: app and database share fate | Nightly dump + rehearsed restore | Yes (D10) |
| No PITR — data written since the last dump is lost on disk failure | Up to 24h loss accepted at design-partner scale | Yes (D10) |
| Free self-serve with no billing invites cost abuse | Per-key rate limit bounds request volume. Storage is **not** bounded — the free-tier `Budget` covers only the default namespace, see §9.2's correction | **Open** |
| A GitHub org member removed from the org keeps access, permanently via keys minted first | Unresolved; three options in §5.2's correction, of which re-verifying membership at key-mint time is the minimum | **Open** |
| pgvector filtered-ANN recall degradation as the shared corpus grows | `hnsw.iterative_scan` (pgvector 0.8+) and 16-way hash partitioning, both already Plan 2 defaults | Mitigated |
| RLS silently inactive if the pool ever connects as owner or superuser | Non-superuser role, `FORCE ROW LEVEL SECURITY`, plus the predicate-removal test in §10 | Mitigated |
| Backup retention vs. erasure: `purge_subject` does not reach dumps | Document the retention window; revisit if a client requires a DPA | Deferred |
| API key revocation is eventually consistent across replicas (§6.2) | Bounded by `NOTIFY` latency or a 30 s refresh; documented, not discovered. Revisit only if a hard-immediate guarantee is required | Accepted |
| k3s control-plane overhead squeezes Postgres on an undersized box | 4 GB floor; CX33 provides 8 GB; `--disable metrics-server` available if tight | Mitigated |
| HNSW index build is the memory spike, not steady-state serving | 8 GB chosen over the 4 GB floor for this reason (§13); `halfvec` amendment (§13.1) would halve index memory if adopted | Mitigated |

---

## 13. Provider selection

**Decision: repurpose the existing OVH host `can-1`.** Marginal hosting cost is zero — the box is
already paid for — which beats every option in the comparison below. The comparison is retained
because it establishes the replacement cost if `can-1` proves unsuitable, and because it is the
benchmark for what a purpose-provisioned box would cost.

**Fallback, and the benchmark: Hetzner Cloud CX33** — 4 vCPU x86, 8 GB RAM, 80 GB NVMe, 20 TB
traffic, at €8.49/mo plus the €0.50 mandatory IPv4 surcharge = €8.99/mo (~$10.45).

### 13.0 Preconditions on `can-1`

`can-1`'s specification was not available when this document was written, so these are gates on
the rollout (§11), not assumptions. Each has a defined outcome if it fails.

| Check | Requirement | If it fails |
|---|---|---|
| RAM | ≥ 4 GB free after existing workloads (§8.4). 8 GB gives comfortable headroom for HNSW builds | Adopt the `halfvec` amendment (§13.1) to halve index memory, or move to the CX33 fallback |
| Disk | ≥ 40 GB free, SSD/NVMe. Postgres on spinning disk is not acceptable for this workload | CX33 fallback |
| Root access | Full root: k3s installation, and a Postgres container with superuser (§8.1) | Disqualifying — CX33 fallback |
| Existing workloads | Either none, or ones that tolerate co-tenancy with k3s | See below |
| Public IPv4 + DNS | A record for the service hostname, ports 80/443 reachable for Let's Encrypt | Resolve before deploy; Traefik cannot issue a certificate otherwise |

**On co-tenancy.** If `can-1` currently runs something else, note that §8.4's fourth constraint
gets worse, not merely unchanged: the app, the database, *and* an unrelated workload now share
fate on one box, and a memory spike during an HNSW index build can take down a neighbour that has
nothing to do with MemorySafe. Set memory and CPU limits on the k3s workloads, and treat "what
else runs here" as a documented fact rather than something rediscovered during an incident.

**On the ARM/x86 question.** If `can-1` is one of OVH's ARM offerings, the Rust binary must be
built for `aarch64` and `pgvector/pgvector:pg17` pulled for arm64. Both are available; it is a
build-configuration item, not a blocker. Confirm the architecture before the first image build.

Options that satisfy all three hard requirements in §8.1 — own Postgres container with
superuser, ≥ 4 GB RAM, one bill:

| Option | $/mo | vCPU / RAM / disk | Bandwidth | Billing basis |
|---|---|---|---|---|
| **Hetzner CX33 (x86)** | **10.45** | 4 / 8 GB / 80 GB NVMe | 20 TB | True monthly |
| Hetzner CAX21 (ARM64) | 12.78 | 4 / 8 GB / 80 GB NVMe | 20 TB | True monthly |
| Hetzner CX23 (x86) | 6.97 | 2 / 4 GB / 40 GB NVMe | 20 TB | True monthly |
| Hetzner CAX11 (ARM64) | 7.55 | 2 / 4 GB / 40 GB NVMe | 20 TB | True monthly |
| Netcup VPS 1000 | 12.06 incl. VAT | 4 / 8 GB / 256 GB NVMe | Unmetered | True monthly |
| Netcup VPS 500 | 6.87 incl. VAT | 2 / 4 GB / 128 GB NVMe | Unmetered | True monthly |
| OVH VPS-2 | 8.50 | 4 / 8 GB / 75 GB NVMe | Unmetered | **Annual prepay** |
| OVH VPS-1 | 4.54 | 2 / 4 GB / 40 GB NVMe | Unmetered | **Annual prepay** |
| Contabo entry | ~6.40 | 4 / 8 GB | Included | True monthly |
| Fly.io app + unmanaged Postgres | ~28.60 | 2 / 4 GB + app machine | $0.02/GB | Usage |

Hetzner prices are net EUR at EU locations, converted at ~1.163, with the IPv4 surcharge
included. Netcup's are VAT-inclusive (≈$10.13 net with a VAT ID, which makes VPS 1000 and CX33
effectively neck-and-neck — Netcup offers 3× the disk, Hetzner the better-documented network).

**Why CX33 and not something cheaper.** OVH VPS-1 at $4.54 is the lowest number in the table but
it is annual prepay, so it is not comparable to a true monthly price and it commits capital
before the first client exists. Among true-monthly options, CX23 at $6.97 is viable and CX33 at
$10.45 is comfortable; the $3.48 difference buys the headroom that matters, because **the memory
spike in this workload is the HNSW index build, not steady-state serving** (§8.2). Paying $3.48
to avoid tuning `maintenance_work_mem` against an OOM on a box that also runs the database is
the right trade at this stage.

**Why x86 rather than ARM.** ARM64 is entirely workable — the Rust binary is ours to compile and
`pgvector/pgvector:pg17` publishes arm64 — but at Hetzner's current pricing the x86 CX line
*undercuts* the ARM CAX line ($10.45 vs $12.78 for the same 4/8/80 spec). ARM buys no discount
here, so there is no reason to take on the cross-compilation surface.

**Fly.io was considered and rejected on price**, at roughly $28.60/mo for an app machine plus an
unmanaged Postgres machine — around 2.7× the chosen option for a database we would still be
operating ourselves. Its genuine advantage is low ops (no OS to maintain, `flyctl deploy`), which
is worth revisiting if operational load rather than cost becomes the binding constraint.

**Backups** go to object storage from the §9.3 CronJob. Hetzner Storage Box keeps everything on
one invoice; Cloudflare R2 has no egress charge, which matters on restore. Confirm current
pricing at purchase — this document's figures were checked on 2026-09-07 and provider pricing
moves (an earlier draft of this section carried Hetzner prices that were roughly two years stale,
including a CX22 SKU that no longer exists).

### 13.1 Proposed amendment to Plan 2: `halfvec` for the derived column

Plan 2's DDL declares `vectors.embedding vector(<vector_dim>)` with
`CREATE INDEX ... USING hnsw (embedding vector_ip_ops)`. Changing the derived column to
`halfvec(<vector_dim>)` with `halfvec_ip_ops` roughly halves index memory (≈1,342 → ≈676 bytes
per vector including HNSW overhead; at 2M vectors, ≈2.7 GB → ≈1.4 GB).

**The architecture makes this nearly free.** The `vector` column exists only for ANN candidate
generation; `q BYTEA` is canonical and exact reranking runs in Rust on the int8 vector (§8.2).
Any additional approximation introduced by half-precision candidate generation is corrected by
the rerank pass, and published benchmarks show recall parity at `ef_search ≥ 40`.

**This is a proposal, not a decision.** Plan 2's DDL was validated against live PostgreSQL 17.11
and pgvector 0.8.6, and this change has not been. It should be adopted only after re-running that
validation plus the retrieval conformance group.

**Related, and worth stating explicitly since it is easy to mistake for a bug:** pgvector has no
int8 vector type through 0.8.6, so a `vector`/`halfvec` copy is stored *alongside* the canonical
`q BYTEA`. That duplication is intentional and is the cost of ANN indexing. Storage budgeting
should assume both copies exist.
