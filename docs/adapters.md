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

`_admin` and `_purged` are reserved. No caller — CLI, MCP, or HTTP — may name
either as a subject or namespace component.

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

`msafe keys add` inherits the configured subject (or takes an explicit
`--subject`) and an optional default `--namespace`. `msafe keys list` lists the
configured key records.

`msafe recall`'s `--sensitivity-ceiling` defaults to `internal` — the same
fail-closed default as MCP's `memory_recall` and `POST /v1/recall`, for the
same reason. Pass `--sensitivity-ceiling restricted` explicitly to see
everything.

Item paging (`review`) and audit paging (`audit`) both echo the *effective*
limit, not the raw value requested: item pages are clamped to
`memorysafe_backend::MAX_PAGE_LIMIT` and audit pages to
`memorysafe_backend::MAX_AUDIT_LIMIT`. There is no separate `truncated` flag on
item paging — `returned.len() < limit` is the sole exhaustion signal — while
`audit` does carry an explicit `truncated` boolean, continued with
`--after <id of the last record>`.

## MCP

```sh
msafe mcp install              # writes .mcp.json for local stdio
msafe mcp install --remote     # writes .mcp.json for hosted MemorySafe
msafe serve --transport stdio
```

Five tools — `memory_remember`, `memory_recall`, `memory_review`,
`memory_forget`, `memory_protect` — and two resources per scope:

```
memorysafe://{tenant}/{subject}/{namespace}/audit
memorysafe://{tenant}/{subject}/{namespace}/stats
```

Resource URIs still carry a subject as an identifier; it is compared against the
credential rather than trusted.

### Scope

Every adapter follows one rule:

- **tenant** comes from the credential — the configuration over stdio, the API key over HTTP.
- **subject** comes from the credential too. No caller, on any transport, may name a subject.
- **namespace** is the only component a caller supplies. It resolves in order: the call's own
  `namespace` argument; the namespace the connection declared (the working directory over stdio,
  the `MemorySafe-Namespace` header over HTTP, else the key's own default); then `default`.

This is why the same MCP client entry works against a local `msafe` and a hosted MemorySafe:
`memory_remember{body}` names no scope, and is a legal call on both.

`_admin` and `_purged` are reserved and may not be named as a namespace, nor minted as a key's
subject.

`memory_recall`'s `sensitivity_ceiling` defaults to `internal` — fail closed,
since recall results feed straight into an agent's working set. `memory_review`
carries no `sensitivity_ceiling` at all: it is the disclosure surface (design
spec §618), the tool that lets an agent show a subject everything stored about
them, and a ceiling that hid their own `sensitive`/`restricted` memories from
that view would defeat the purpose. This asymmetry is deliberate, not a gap to
close later.

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

`POST /v1/recall` defaults `sensitivity_ceiling` to `internal` — the same
fail-closed default as MCP's `memory_recall`, and for the same reason.

**Budgets persist; policy and retention do not.** `PUT
/v1/admin/tenants/{id}/budgets` writes through to the backend and survives a
restart. `PUT /v1/admin/tenants/{id}/policy` and `/retention` are held only
in an in-memory registry (`Engine`'s `policies`/`retentions` fields) — the
call returns `200` with an audit id, and the change is real until the process
restarts, at which point it silently reverts to the engine's configured
default with no further record. Concretely: an operator who sets retention to
`hipaa_retain` or `forensic` (`PurgeCascade::Preserve`, so a `purge_subject`
call is meant to leave audit rows in place) reverts to whatever the default
profile is on restart, and the next `purge_subject` deletes those rows — with
a `PolicyChanged` row already on file asserting the change took effect.
Persisting these two is out of scope for now; this is a statement of current
behaviour, not a plan to fix it.

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

Reads take an optional `namespace` query parameter; writes take it in the
body. Subject is never accepted from a request. `GET /v1/audit` takes its event
filter as one comma-separated value, and pages with `limit` plus `after=<last
audit id>`; the response carries `truncated` so a compliance query cannot stop
short silently — `limit` is clamped to `MAX_AUDIT_LIMIT` and the response echoes
the effective value.

## Shadow evaluation

```sh
msafe export ./archive --include-audit
msafe shadow ./archive --candidate-config ./candidate.json
```

Replays the archive's admissions under two policy configurations and diffs the
decisions. Only admissions can be replayed — a rejected write's body was never
stored, and a merged write's content was folded into its target — so the report
states its coverage rather than implying it proved something about the rest.
