# One MCP Endpoint, Local or Hosted — Design

**Date:** 2026-09-07
**Status:** Approved design, ready for implementation planning
**Scope:** The client-facing contract of the MCP adapter, so that one MCP client configuration behaves identically against a local `msafe` and against the hosted service.

> **Where this file lives.** This document specifies **public** architecture: the scope contract of
> `memorysafe-mcp`, `memorysafe-api`, `memorysafe-auth`, and `memorysafe-cli`. It references the
> hosted service where the two meet, but does not restate hosted commercial detail — that belongs
> to `.superpowers/closed-tier/2026-09-07-hosted-deployment-design.md` (henceforth **HD**), which
> moves to `memorysafe-cloud` per its own instruction. Where this design depends on a decision of
> HD's, it cites it rather than repeating it.

---

## 1. What this covers, and what it does not

HD §3.3 already settles the *server-side* question: `memorysafe-cloud-server` is a second
composition root that wires `PostgresBackend` into an `Arc<Engine>` and mounts the open-source
adapters over it unmodified. That decision stands and is not reopened here.

**This document covers the client-facing half:** what a person puts in `.mcp.json`, and what an
agent observes at the tool boundary, when the endpoint is local versus hosted. The goal is that
neither the config nor the agent has to know which.

**It does not cover:** the governance engine (Plan 1), the Postgres backend (Plan 2), the tool
surface itself (Plan 3, Tasks 3-5), hosting topology or the control plane's internals (HD), or
the design of the OAuth authorization server beyond the contract it must satisfy (§10, deferred
to its own plan).

---

## 2. The problem

The MCP adapter already has two transports and a `ScopeSource` enum that distinguishes them:
stdio is configured with tenant and subject and defaults the namespace from the working
directory; streamable HTTP takes the tenant from an API key and requires subject and namespace
per call.

That is a reasonable split for two *deployments*. It is the wrong split for one *agent*, because
the divergence is invisible at the tool boundary:

- Every tool declares `subject: Option<String>` and `namespace: Option<String>`
  (`crates/memorysafe-mcp/src/dto.rs`). **The JSON schema is byte-identical on both transports.**
- The runtime requirement is not. `ScopeSource::resolve`'s `Http` arm fails with
  `'subject' is required over HTTP`; the `Stdio` arm accepts the field only when it names the
  configured subject, and otherwise refuses.
- The only signal a model gets is the field's own doc comment — *"Required over HTTP; over stdio
  it must match the configured subject"* — which asks the model to know which transport it is
  connected over. It has no way to know.

So `memory_remember{text: "..."}` — schema-valid, and the obvious call — succeeds locally and
returns `invalid_params` against the hosted service. An agent's learned usage does not transfer,
and neither does a project's configuration.

There is a second defect in the same place, independent of the first. `Authenticated::scope`
takes `subject` from the request and validates only the tenant and a reserved-word list. **Any
API key can therefore write as any subject within its tenant.** Nothing above it narrows this;
`memorysafe-api`'s `ScopeParams` has the same shape.

---

## 3. Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | Unification is **client-side**: one config entry, one agent-visible contract | HD §3.3 already unified the server side. The remaining gap is what the client and the model see. |
| D2 | Target clients are those with **real remote-HTTP support** (Claude Code and equivalents) | Removes any need for `msafe` to proxy stdio-to-remote. The hosted endpoint is a genuine remote MCP server. |
| D3 | **Tenant and subject always come from the credential**; neither is ever nameable by a caller | Makes the two transports behave identically *and* closes §2's second defect. `subject` leaves the tool schema entirely. |
| D4 | **One credential, one subject** — no delegating credentials for now | A credential that writes on behalf of many end-users is a real future case (SaaS built on MemorySafe) but not a current one. §12 records what it would cost to add. |
| D5 | `ScopeSource`'s enum becomes a **`ScopeResolver` trait** | The enum is `pub` with two variants and no extension point, so `memorysafe-cloud` cannot add a resolver without patching the public crate — which HD §3.3 names as the signal of a leaked assumption. |
| D6 | Hosted auth is **OAuth for humans, API keys for CI**, both presented as `Authorization: Bearer` | OAuth removes the secret from config and supplies identity directly. Keys stay for programmatic use, and their lifecycle already exists in `memorysafe-cloud-control`. |
| D7 | The namespace is carried in a **header**, never in the URL path | Path-scoping fragments the OAuth audience — see §7, which is the one place a plausible design was rejected on evidence. |
| D8 | This lands as **two plans**: the public scope contract now, the OAuth authorization server next | The first already reaches the goal with API keys. The second is a project in its own right and should not gate the first. |

---

## 4. The scope rule

One rule, stated once, true on both transports:

- **tenant** — from the credential. Never nameable by a caller.
- **subject** — from the credential. Never nameable by a caller.
- **namespace** — the first of:
  1. the call's `namespace` argument;
  2. the connection's declared namespace (stdio: the working-directory slug; HTTP: the
     `MemorySafe-Namespace` header, else the credential's own default namespace);
  3. the literal `default`.

An agent calling `memory_remember{text}` therefore behaves identically against either endpoint,
and a call that names a namespace overrides it identically on both. Nothing in the schema asks
the model which transport it is on, because after D3 nothing depends on the answer.

`check_reserved` keeps its role, now applied to the namespace only — subject is no longer
caller-supplied, so there is no caller-supplied subject left to reject. The startup check in
`transport::reject_reserved_configuration` still applies to operator-configured values and gains
the API key records as a second source (§9).

---

## 5. The seam

```rust
/// How a call's scope and actor are established, once per deployment shape.
pub trait ScopeResolver: Send + Sync + 'static {
    /// The scope a client sees before it names one. `None` where scope is a
    /// property of the request rather than of the server.
    fn default_scope(&self) -> Option<Scope>;

    /// Resolve one call. `namespace` is the only caller-supplied component.
    fn resolve(&self, ext: &Extensions, namespace: Option<&str>)
        -> Result<Resolved, ErrorData>;
}
```

`subject` is absent from the signature. That absence is the enforcement mechanism: D3 is not a
rule reviewers must remember, it is a parameter that does not exist.

The public crate ships two implementations:

- **`FixedScope { tenant, subject, default_namespace }`** — today's `Stdio` variant, renamed
  because it describes a *binding*, not a transport. A local `msafe serve --transport http` uses
  it too when serving a single developer.
- **`ApiKeyScope { keys: Arc<ApiKeyStore> }`** — today's `Http` variant, except that subject now
  comes off the authenticated key record rather than the request.

`memorysafe-cloud-control` implements a third, `CloudScope`, in the closed repository:

- it reads its live `ArcSwap<ApiKeyStore>` per request rather than holding a snapshot;
- it accepts either credential kind (§6);
- it requires no change to the public crate.

**This also fixes a freshness bug that would otherwise have been discovered in production.**
`ScopeSource::Http` holds `Arc<ApiKeyStore>`, a store built from a `Vec` at construction and
never mutated. Mounting today's adapter over the hosted service would hand it a frozen snapshot,
so a key minted in the dashboard would never authenticate against that process — defeating the
entire `LISTEN`/`NOTIFY` refresh mechanism HD §6.2 specifies. `memorysafe-api`'s
`AppState { keys: Arc<ApiKeyStore> }` has the identical defect. The trait removes both, because a
resolver is free to read whatever it likes per call.

### 5.1 The trade this makes, stated deliberately

`Authenticated` has private fields and no public constructor, and its doc states the invariant:
*"The only way to obtain one is `ApiKeyStore::authenticate`, so a handler that holds one cannot
have skipped the check."* A trait widens who may act as the authentication authority: any binary
composing the server can implement a resolver that fabricates a `Resolved`.

The property that actually protects callers is unchanged — **a tool body still cannot obtain a
scope except by calling `resolve`** — and the resolver *is* the authority by definition, in a
composition root that is trusted code. But this is a real reduction in what the type system
proves, and it is recorded here as an accepted decision rather than left to be noticed later.
`Authenticated` itself keeps its constructor privacy; nothing in this design exposes it.

---

## 6. Credentials

Two kinds reach the endpoint, both as `Authorization: Bearer <credential>`:

| Kind | Discriminated by | Yields |
|---|---|---|
| API key | the existing `KEY_PREFIX` (`msk_`) | tenant, subject, default namespace — from the record |
| OAuth access token | anything else | tenant, subject — from validated claims |

Prefix discrimination is unambiguous and needs no negotiation, because `KEY_PREFIX` already
exists and is already the first thing `parse_presented` looks at.

**Tenant and subject from a token.** Subject is the authenticated human, `u_<github_user_id>`.
Tenant is that user's personal tenant by default, or an org tenant `o_<github_org_id>` the token
was issued for. For a single-developer personal tenant, tenant and subject coincide; that is
correct, not a degenerate case.

**Org tenant selection is a property of the token, not of a header.** A header naming the tenant,
validated against an org list carried in the token, would re-create HD §5.2's staleness problem
in a new place. Issuing the token for one tenant keeps the audience honest and lets the standard
step-up flow handle switching.

**A payoff worth naming.** HD §5.2's correction records an open risk: a seven-day session cookie
is a seven-day membership cache, and API keys minted inside that window never expire, so removing
someone from a GitHub org can leave a working credential permanently. Short-lived OAuth access
tokens with refresh *are* option 1 of the three that correction lists, obtained as a side effect
of D6 — membership is re-verified per refresh rather than per week. This does **not** fix the API
key path, which still needs HD §5.2's option 2 (re-verify membership at mint time). That remains
open and is out of scope here.

---

## 7. Why the namespace is a header and not a URL path

`https://api.memorysafe.dev/mcp/checkout-service` is the prettier design and was rejected on
evidence.

The MCP authorization specification requires clients to send an RFC 8707 `resource` parameter
identifying the server in both authorization and token requests, and requires servers to validate
that a token was issued for them as audience. Path components are explicitly legal in a canonical
resource URI — the spec lists `https://mcp.example.com/server/mcp` as valid *"when path component
is necessary to identify individual MCP server"*. That is precisely the problem: under path
scoping **each namespace becomes a distinct OAuth resource**, so a developer with twenty
repositories performs twenty authorization flows and holds twenty audience-bound tokens, and a
token for one project is correctly rejected by another.

Protected-resource-metadata discovery compounds it. The metadata path is derived from the
resource identifier, and there is field evidence of clients resolving `.well-known` endpoints
against the root domain rather than the advertised path.

The resource URL therefore stays flat at `https://api.memorysafe.dev/mcp` and the namespace rides
in `MemorySafe-Namespace`. This costs nothing structurally: `ScopeSource::resolve` already pulls
`http::request::Parts` out of the MCP request extensions, so reading one more header is a
two-line change at a seam that already exists.

---

## 8. Client configuration

### 8.1 The committed entry

```json
{"mcpServers":{"memorysafe":{
  "type":"http",
  "url":"${MEMORYSAFE_URL:-https://api.memorysafe.dev}/mcp",
  "headersHelper":"msafe mcp headers"}}}
```

One entry, safe to commit, with no secret in it. `msafe mcp headers` is re-run on every
connection, in the project directory, and emits a JSON object of headers:

- `Authorization` — from `msafe`'s own credential store, refreshed as needed;
- `MemorySafe-Namespace` — **slugged from the working directory by the same `slug()` already
  written and tested in `cmd/serve.rs`**.

That reuse is the point. The "namespace follows the project you are in" ergonomic that makes the
local server pleasant is preserved on the hosted path, dynamically rather than frozen at install
time, and the rule exists in exactly one function rather than two that can drift. Pointing the
same entry at a locally running `msafe serve --transport http` is one environment variable.

### 8.2 The portable fallback

`headersHelper` is a Claude Code feature, not part of the MCP specification. Under D2 that is
acceptable, but `msafe mcp install` must also be able to emit a spec-portable entry:

```json
{"mcpServers":{"memorysafe":{
  "type":"http",
  "url":"https://api.memorysafe.dev/mcp",
  "headers":{"Authorization":"Bearer ${MEMORYSAFE_API_KEY}",
             "MemorySafe-Namespace":"checkout-service"}}}}
```

Same semantics, namespace fixed at install time instead of per connection. Environment-variable
expansion is supported in `.mcp.json`'s `url`, `headers`, `args`, `env` and `command` fields, so
this form is committable too.

**No form of `msafe mcp install` ever writes a credential into a project-scoped file.** A secret
goes in the environment, or in a user-scoped config, or nowhere.

### 8.3 Commands

- `msafe mcp install [--remote] [--portable]` — writes the project entry, choosing helper or
  literal-header form, never a secret.
- `msafe mcp headers` — the `headersHelper` implementation; emits JSON on stdout.
- `msafe login` — obtains and stores a hosted credential. Its shape depends on §10 and is
  specified with it.

---

## 9. What changes, crate by crate

| Crate | Change | Size |
|---|---|---|
| `memorysafe-auth` | `ApiKeyRecord` gains `subject: SubjectId` and an optional default namespace; `Authenticated::scope(namespace)` loses its `subject` argument; `generate` takes a subject. `check_reserved` applies to both at mint time | S |
| `memorysafe-mcp` | `ScopeSource` enum becomes the `ScopeResolver` trait plus `FixedScope` and `ApiKeyScope`; `serve_stdio`/`http_service_with` take `Arc<dyn ScopeResolver>`; DTOs drop `subject`; the five tool bodies and five test files follow. `resources.rs` needs care — see below | M |

**The resource URI grammar is the one place `subject` must survive.**
`memorysafe://{tenant}/{subject}/{namespace}/audit` identifies a resource, so the component stays
in the URI; what changes is that `read_resource` may no longer *trust* it. Today it resolves the
parsed subject through `ScopeSource::resolve` and separately compares the parsed tenant — under
§4 there is no subject parameter to resolve through, so the parsed subject must be compared
against the credential's subject exactly as the tenant already is, and a mismatch must be
`resource_not_found`. Getting this wrong is the one way §4's rule could be bypassed while every
tool-level test still passes, which is why §11 tests the resource path separately.
| `memorysafe-api` | The same rule for REST: `ScopeParams` drops `subject`, `Auth` supplies it, `AppState` holds a resolver rather than a store | M |
| `memorysafe-cli` | `serve` constructs the new resolvers; `msafe keys` mints subject-bearing keys; new `mcp install` and `mcp headers` | S–M |
| `memorysafe-cloud-control` | `CloudScope`; a `subject` column on `control.api_keys` | M |
| `memorysafe-cloud-server` | Composition root per HD §3.3 | S |

**`memorysafe-api` is separable but should not be separated.** Deferring it leaves the two
adapters with different scope rules and leaves §2's second defect open on the REST surface. It is
listed separately only so the cost is visible, not to invite skipping it.

**Timing.** These are breaking changes to crates that are not yet published and not yet depended
on from outside the workspace. Plan 3 is mid-flight: Task 14 is uncommitted, and Tasks 15-17
(shadow) touch no scope code at all.

Task 18 is the reason to move now rather than after. It writes `docs/adapters.md` and the README
walkthrough, and what it documents is precisely the rule §4 replaces — *"Over stdio the server is
bound to one tenant and one subject; ... over HTTP the API key identifies the tenant and each
call carries subject and namespace"*, plus REST reads taking `subject` as a query parameter, plus
the resource URI grammar. Landing this design after Task 18 means rewriting freshly written
documentation and a freshly written end-to-end walkthrough test. Landing it before means Task 18
documents the rule once, correctly.

---

## 10. Sequencing

**Plan A — the scope contract (public, now).** Everything in §9 except the OAuth authorization
server. On completion, a hosted user writes one committed config entry, exports one environment
variable, and gets agent behaviour identical to the local server. *The stated goal is reached
here.*

**Plan B — the authorization server (private, next).** `memorysafe-cloud-control` becomes an
OAuth 2.1 authorization server: protected-resource metadata, authorization-server metadata,
authorize and token endpoints, PKCE, client-ID-metadata-documents or dynamic client registration,
audience binding, refresh, short-lived access tokens. It upgrades setup from "export a key" to
`claude mcp login memorysafe` and no secret anywhere.

Splitting them is deliberate: Plan B is a project, and gating the whole client-side goal on it
would delay a result that Plan A already delivers.

---

## 11. Testing

Following the precedent the backend conformance suite sets — shared behaviour is pinned by a
suite every implementation inherits, not by review convention:

| Test | What it protects |
|---|---|
| A table-driven suite run against **both** public resolvers, asserting identical resolution for identical (call, connection) inputs | §4's rule, made mechanical. This is the "the two transports behave the same" claim, and it is the one most likely to rot |
| Tool-schema snapshot asserting **no tool declares `subject`** | Stops the field creeping back once D3 is out of living memory |
| Raw `subject` in tool params is ignored, on both resolvers | §2's second defect, pinned from the caller's side rather than the type's |
| A resource URI naming another subject returns `resource_not_found`, on both resolvers | §9's carve-out — the one path where `subject` is still parsed from caller input |
| Reserved-word coverage, namespace-only, both resolvers | The existing coverage, narrowed to what is still caller-supplied |
| A key whose record names a reserved subject is refused at mint and at load | The startup check's new second source |
| Key freshness (cloud): a key minted against one connection authenticates on the next request with no restart | HD §6.2's mechanism, which today's frozen store silently defeats |
| Audience rejection (cloud, Plan B): a token minted for another resource is refused with 401 | The MCP specification's MUST on audience validation |

The REST adapter has no tool schema, so it does not inherit these tests literally. If §9's
`memorysafe-api` row is done, it gets the same table rewritten against its own surface — request
params instead of tool params, `ScopeParams` instead of a schema snapshot — driven by the same
resolver and therefore asserting the same rule. If that row is deferred, REST gets none of it and
keeps §2's second defect. That is the concrete cost of deferring, and it is the reason the row
should not be deferred.

---

## 12. Open items and deliberate omissions

- **Delegating credentials.** D4 defers the SaaS-on-MemorySafe case. Adding it later means a
  credential kind that *may* name a subject, which is a third resolver and a per-call `subject`
  restored on that path only — additive to the trait, not a rework of it. The trait shape in §5 is
  chosen so this stays additive.
- **API key membership revocation.** HD §5.2's option 2 (re-verify org membership at key-mint
  time) is untouched here and remains open. §6 narrows the window for OAuth only.
- **Tenant-level budgets.** HD §9.2's correction — a free-tier budget on one namespace bounds one
  namespace out of infinitely many — is orthogonal to this design and unaffected by it. Worth
  noting that §4 makes namespaces easier to create, not harder, so the gap does not shrink on its
  own.
- **`headersHelper` portability.** §8.2's fallback is the answer for clients without it. If a
  second client becomes important and lacks both `headersHelper` and env expansion, a static
  user-scoped entry is the remaining option.
- **Not adopted: a stdio-to-remote proxy in `msafe`.** Considered and rejected under D2. It would
  be the right answer if stdio-only clients had to reach the hosted service; that trade should be
  revisited only if D2 changes.
