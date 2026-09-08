# Unified MCP Scope Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make one MCP client configuration behave identically against a local `msafe` and a hosted MemorySafe, by taking tenant and subject from the credential and leaving namespace as the only caller-supplied scope component.

**Architecture:** `memorysafe-auth` grows a transport-neutral `ScopeResolver` trait with two implementations — `FixedScope` (credential is process configuration) and `ApiKeyScope` (credential is a bearer key). `memorysafe-mcp` and `memorysafe-api` both consume the trait instead of owning their own scope logic, so the two adapters cannot disagree. `subject` disappears from every caller-facing surface; namespace resolves call argument → `MemorySafe-Namespace` header → the key's own default → `default`.

**Tech Stack:** Rust 2024, `rmcp` 3.2 (MCP server), `axum` 0.8 (HTTP), `clap` (CLI), `http` 1.3 (header types), `schemars` 1.2 (tool schemas).

**Source spec:** `docs/superpowers/specs/2026-09-07-unified-mcp-endpoint-design.md`. This plan implements **Plan A only** — the public scope contract. The OAuth 2.1 authorization server (spec §10, Plan B) is explicitly out of scope.

---

## Correction to the spec, applied by this plan

The spec's §5 sketches the trait inside `memorysafe-mcp`, taking `&rmcp::model::Extensions` and returning `rmcp::ErrorData`. Its §9 also requires `memorysafe-api`'s `AppState` to hold "a resolver rather than a store". **Those two statements are incompatible:** honouring §9 with §5's signature would make `memorysafe-api` depend on `rmcp`, pulling an MCP server implementation into the REST adapter for a type it only needs to convert away from.

This plan therefore places the trait in **`memorysafe-auth`**, taking `&http::HeaderMap` and returning `AuthError`. Each adapter maps `AuthError` into its own error type, which both already do (`memorysafe-mcp`'s `auth_error`, `memorysafe-api`'s `ApiError: From<AuthError>`). `HeaderMap` is the transport-neutral thing both adapters have in hand: MCP reaches it through `http::request::Parts` in the request extensions, and axum has it directly.

`memorysafe-auth` gains one dependency, `http` — a pure types crate with no I/O. The workspace purity constraint in `AGENTS.md` binds `memorysafe-core` (no I/O dependencies) and `memorysafe-embed` (no network); `memorysafe-auth` is bound by neither.

Everything else in the spec is implemented as written. Task 10 amends the spec so the two documents agree.

---

## Global Constraints

Every task's requirements implicitly include this section.

- Rust 2024 edition, toolchain **1.97.1**, pinned by `rust-toolchain.toml`. Run all commands from the repository root.
- The workspace **forbids `unsafe`** and denies the configured Rust and Clippy lints (`unsafe_code = "forbid"`, `clippy::all = "deny"`).
- `memorysafe-core` must not acquire I/O dependencies. `memorysafe-embed` must not reach a network stack under any feature combination.
- Scope is always tenant → subject → namespace. Validate identifiers through the existing typed constructors (`TenantId::new`, `SubjectId::new`, `Namespace::new`, `Scope::new`) rather than bypassing them.
- Tests must be deterministic and must not require network access, external services, or model downloads. Use `tempfile` temporary directories and `memorysafe_embed::DeterministicEmbedder`.
- `_admin` (`ADMIN_COMPONENT`) and `_purged` (`PURGED_COMPONENT`) are reserved. No caller — CLI, MCP, or HTTP — may name either as a subject or namespace component.
- Audit records and logs carry ids, digests, and feature numbers, **never memory bodies and never key secrets**, at any log level.
- Required checks before every commit:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features -- -D warnings
  ```
- **Running the test suite on this machine** needs the C++ runtime on the loader path, because the toolchain is a rustup cargo rather than a nix devshell:
  ```bash
  LD_LIBRARY_PATH=$(ls -d /nix/store/*gcc-15.3.0-lib/lib | head -1) \
    cargo test --workspace --all-features --no-fail-fast
  ```
  Without it, `--all-features` unification pulls `model2vec-rs`/`tokenizers` into targets that fail with `libstdc++.so.6: cannot open shared object file`. That is a **link/load** failure, so those targets print **no `test result:` line at all**. Verify a full run by comparing counts, never by scanning for `FAILED`:
  ```bash
  LD_LIBRARY_PATH=$(ls -d /nix/store/*gcc-15.3.0-lib/lib | head -1) \
    cargo test --workspace --all-features --no-fail-fast 2>&1 | tee /tmp/run.log
  grep -c '^running ' /tmp/run.log
  grep -c '^test result:' /tmp/run.log   # these two must be equal
  ```
  Per-package runs without `--all-features` link fine and hide the problem, so a green `cargo test -p <crate>` is not evidence the workspace is green.
- Conventional Commit subjects (`feat:`, `fix:`, `docs:`, `test:`), scoped to the crate where practical. No attribution or co-author trailers.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/memorysafe-auth/src/key.rs` | `ApiKeyRecord` carries the subject and optional default namespace a key is minted for; `generate` refuses reserved components | 1 |
| `crates/memorysafe-auth/src/store.rs` | `Authenticated` carries subject; `scope(namespace)` builds a `Scope` from credential + one caller value | 1 |
| `crates/memorysafe-auth/src/resolver.rs` | **New.** `Resolved`, `ScopeResolver`, `NAMESPACE_HEADER`, `FALLBACK_NAMESPACE`, `FixedScope` | 2 |
| `crates/memorysafe-auth/src/resolver.rs` | `ApiKeyScope` and the namespace precedence chain | 3 |
| `crates/memorysafe-auth/tests/parity.rs` | **New.** The table-driven suite both resolvers must satisfy identically | 3 |
| `crates/memorysafe-mcp/src/scope.rs` | Shrinks to the `AuthError` → `ErrorData` mapping and re-exports | 4 |
| `crates/memorysafe-mcp/src/dto.rs` | The five param structs drop `subject` | 4 |
| `crates/memorysafe-mcp/src/tools_write.rs`, `tools_curate.rs` | Call sites drop the subject argument | 4 |
| `crates/memorysafe-mcp/src/lib.rs`, `transport.rs` | Server and transports take `Arc<dyn ScopeResolver>` | 4 |
| `crates/memorysafe-mcp/src/lib.rs` | `read_resource` compares the URI's subject against the credential's | 5 |
| `crates/memorysafe-api/src/scope.rs`, `auth.rs`, `memories.rs`, `ops.rs`, `lib.rs` | The same rule for REST | 6 |
| `crates/memorysafe-cli/src/cmd/keys.rs`, `cmd/serve.rs` | Mint subject-bearing keys; build resolvers | 7 |
| `crates/memorysafe-cli/src/cmd/mcp.rs` | **New.** `msafe mcp install` and `msafe mcp headers` | 8 |
| `docs/adapters.md`, `README.md`, `crates/memorysafe-cli/tests/walkthrough.rs` | Document the one rule, once | 9 |
| `docs/superpowers/specs/2026-09-07-unified-mcp-endpoint-design.md` | Amend §5 and §9 to match what was built | 10 |

---

## Task 1: Auth — an API key carries the subject it was minted for

**Files:**
- Modify: `crates/memorysafe-auth/src/key.rs`
- Modify: `crates/memorysafe-auth/src/store.rs`

**Interfaces:**
- Consumes: `memorysafe_core::{TenantId, SubjectId, Namespace, Scope, Actor, ActorKind}`.
- Produces:
  - `ApiKeyRecord { id: String, tenant: TenantId, subject: SubjectId, default_namespace: Option<Namespace>, hash: String, label: String, disabled: bool }`
  - `generate(tenant: TenantId, subject: SubjectId, label: &str) -> Result<GeneratedKey, AuthError>`
  - `Authenticated::subject(&self) -> &SubjectId`
  - `Authenticated::default_namespace(&self) -> Option<&Namespace>`
  - `Authenticated::scope(&self, namespace: &str) -> Result<Scope, AuthError>` — **note the dropped `subject` parameter**

- [ ] **Step 1: Write the failing test for the record's new fields**

Add to the `tests` module in `crates/memorysafe-auth/src/key.rs`:

```rust
#[test]
fn a_generated_key_carries_the_subject_it_was_minted_for() {
    // The whole scope contract rests on this: if a key does not name a
    // subject, the subject has to come from the request, and any key can
    // write as any subject in its tenant.
    let tenant = TenantId::new("acme").unwrap();
    let subject = memorysafe_core::SubjectId::new("user-42").unwrap();
    let g = generate(tenant.clone(), subject.clone(), "laptop").expect("generate");

    assert_eq!(g.record.tenant, tenant);
    assert_eq!(g.record.subject, subject);
    assert_eq!(
        g.record.default_namespace, None,
        "a key defaults to no namespace of its own; the header or the fallback supplies one"
    );
}

#[test]
fn a_key_may_not_be_minted_for_a_reserved_subject() {
    // `_admin` is where tenant-level audit rows live. A key minted for it
    // would let its holder write rows that look like the engine wrote them,
    // and no per-call check would ever see the subject to reject it.
    let tenant = TenantId::new("acme").unwrap();
    for reserved in [memorysafe_core::ADMIN_COMPONENT, memorysafe_core::PURGED_COMPONENT] {
        let subject = memorysafe_core::SubjectId::new(reserved).unwrap();
        assert!(
            matches!(
                generate(tenant.clone(), subject, "bad"),
                Err(AuthError::Reserved { .. })
            ),
            "'{reserved}' was accepted as a key subject"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p memorysafe-auth --lib key::tests::a_generated_key_carries_the_subject_it_was_minted_for`
Expected: FAIL to compile — `generate` takes 2 arguments, and `ApiKeyRecord` has no field `subject`.

- [ ] **Step 3: Add the fields to `ApiKeyRecord`**

In `crates/memorysafe-auth/src/key.rs`, replace the struct:

```rust
/// What is written to configuration. Holds a hash, never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub tenant: TenantId,
    /// The one subject this key acts as. Never supplied by a request — that
    /// is the whole point. See the scope contract in `resolver.rs`.
    pub subject: SubjectId,
    /// The namespace this key falls back to when a request declares none.
    /// `None` means the `FALLBACK_NAMESPACE` applies.
    #[serde(default)]
    pub default_namespace: Option<Namespace>,
    /// BLAKE3 of the whole presented key, hex encoded.
    pub hash: String,
    pub label: String,
    #[serde(default)]
    pub disabled: bool,
}
```

Update the imports at the top of the file:

```rust
use memorysafe_core::{Namespace, SubjectId, TenantId};
```

- [ ] **Step 4: Change `generate` to take and check a subject**

Replace `generate` in `crates/memorysafe-auth/src/key.rs`:

```rust
pub fn generate(
    tenant: TenantId,
    subject: SubjectId,
    label: &str,
) -> Result<GeneratedKey, AuthError> {
    // At mint time, not only at call time. `ScopeResolver::resolve` never
    // sees a caller-supplied subject any more, so if a reserved one is not
    // refused here it is never refused at all.
    crate::store::check_reserved(Some(subject.as_str()), None)?;

    let id = ulid::Ulid::generate().to_string();
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Rng)?;
    let secret_part = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let secret = format!("{KEY_PREFIX}_{id}_{secret_part}");

    Ok(GeneratedKey {
        record: ApiKeyRecord {
            id,
            tenant,
            subject,
            default_namespace: None,
            hash: hash_presented(&secret),
            label: label.to_owned(),
            disabled: false,
        },
        secret,
    })
}
```

- [ ] **Step 5: Update the existing `key.rs` tests to the new signature**

Every existing call in `crates/memorysafe-auth/src/key.rs`'s test module is `generate(TenantId::new("acme").unwrap(), "<label>")`. Insert a subject argument in each:

```rust
// Add near the top of the tests module:
fn subject() -> memorysafe_core::SubjectId {
    memorysafe_core::SubjectId::new("user-42").expect("a legal subject")
}

// Then every call site becomes, e.g.:
let g = generate(TenantId::new("acme").unwrap(), subject(), "ci runner").expect("generate");
```

Apply to all six existing tests: `a_generated_key_carries_its_id_in_the_clear_and_its_secret_only_once`, `the_record_never_contains_the_secret`, `two_generated_keys_never_collide` (both calls), `the_debug_rendering_of_a_generated_key_does_not_leak_the_secret`, and `the_debug_rendering_of_a_generated_key_still_shows_the_record`.

In `the_debug_rendering_of_a_generated_key_still_shows_the_record`, add one assertion — the redacted `Debug` must keep showing the subject, since that is now diagnostic information a reader needs:

```rust
assert!(rendered.contains("user-42"), "{rendered}");
```

- [ ] **Step 6: Run the key.rs tests to verify they pass**

Run: `cargo test -p memorysafe-auth --lib key::`
Expected: PASS, 8 tests.

- [ ] **Step 7: Write the failing test for `Authenticated`**

Add to the `tests` module in `crates/memorysafe-auth/src/store.rs`:

```rust
#[test]
fn an_authenticated_key_reports_the_subject_it_was_minted_for() {
    let (store, secret, tenant) = store_with("ci");
    let auth = store.authenticate(&secret).expect("authenticate");
    assert_eq!(auth.tenant(), &tenant);
    assert_eq!(auth.subject().as_str(), "user-42");
    assert_eq!(auth.default_namespace(), None);
}

#[test]
fn scope_takes_the_namespace_and_nothing_else_from_the_caller() {
    // The signature is the enforcement. There is no subject parameter to
    // pass, so no adapter can route request input into the subject position.
    let (store, secret, _) = store_with("ci");
    let auth = store.authenticate(&secret).unwrap();

    let scope = auth.scope("coding-agent").expect("in-tenant scope");
    assert_eq!(scope.tenant.as_str(), "acme");
    assert_eq!(scope.subject.as_str(), "user-42");
    assert_eq!(scope.namespace.as_str(), "coding-agent");
}

#[test]
fn scope_still_refuses_a_reserved_namespace() {
    let (store, secret, _) = store_with("ci");
    let auth = store.authenticate(&secret).unwrap();

    assert!(matches!(
        auth.scope(ADMIN_COMPONENT),
        Err(AuthError::Reserved { component: "_admin" })
    ));
    assert!(matches!(
        auth.scope(PURGED_COMPONENT),
        Err(AuthError::Reserved { component: "_purged" })
    ));
}
```

- [ ] **Step 8: Run the test to verify it fails**

Run: `cargo test -p memorysafe-auth --lib store::tests::scope_takes_the_namespace_and_nothing_else_from_the_caller`
Expected: FAIL to compile — `scope` takes 2 arguments, `Authenticated` has no method `subject`.

- [ ] **Step 9: Carry the subject through `Authenticated`**

In `crates/memorysafe-auth/src/store.rs`, make `check_reserved` visible to `key.rs` by leaving it `pub` (it already is), then replace the `Authenticated` struct, its construction in `authenticate`, and `scope`:

```rust
        Ok(Authenticated {
            tenant: record.tenant.clone(),
            subject: record.subject.clone(),
            default_namespace: record.default_namespace.clone(),
            key_id: record.id.clone(),
        })
```

```rust
/// Proof that a caller is one specific tenant *and* one specific subject.
/// The only way to obtain one is `ApiKeyStore::authenticate`, so a handler
/// that holds one cannot have skipped the check.
#[derive(Debug, Clone)]
pub struct Authenticated {
    tenant: TenantId,
    subject: SubjectId,
    default_namespace: Option<Namespace>,
    key_id: String,
}

impl Authenticated {
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// The subject this credential acts as. Fixed when the key was minted.
    pub fn subject(&self) -> &SubjectId {
        &self.subject
    }

    /// The namespace to use when a request declares none. `None` means the
    /// caller should fall back to `FALLBACK_NAMESPACE`.
    pub fn default_namespace(&self) -> Option<&Namespace> {
        self.default_namespace.as_ref()
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The audit actor for anything this caller does. The key id, never the key.
    pub fn actor(&self) -> Actor {
        Actor {
            kind: ActorKind::ApiKey,
            id: Some(self.key_id.clone()),
        }
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

    /// The constructor of a `Scope` for any adapter path that authenticates
    /// an API key. Tenant *and subject* come from the credential; only the
    /// namespace comes from the request. A scope that crosses tenants or
    /// impersonates another subject is unrepresentable here rather than
    /// merely rejected.
    /// Both components are checked, not just the caller's.
    ///
    /// The namespace is checked because it is caller input. The *subject* is
    /// checked because `ApiKeyStore::new` is a constructor, not a validator:
    /// it takes records on trust, so a hand-edited `msafe.toml` carrying
    /// `subject = "_admin"` reaches here unexamined. `memorysafe-core` will
    /// not catch it either — `SubjectId::new(ADMIN_COMPONENT)` succeeds by
    /// design (see `PURGED_COMPONENT`'s doc there, and the assertions at
    /// `ids.rs:303-308`), because rejecting reserved words as *caller* input
    /// is an adapter's job. This is that adapter. One extra comparison per
    /// call closes the only path by which a reserved subject could reach a
    /// real scope.
    ///
    /// `FixedScope` needs no equivalent: its constructor refuses a reserved
    /// subject and its fields are immutable thereafter, so there is no
    /// later moment at which an unchecked value could appear.
    pub fn scope(&self, namespace: &str) -> Result<Scope, AuthError> {
        check_reserved(Some(self.subject.as_str()), Some(namespace))?;
        Ok(Scope::new(
            self.tenant.as_str(),
            self.subject.as_str(),
            namespace,
        )?)
    }
}
```

Update the file's imports to add `Namespace` and `SubjectId`:

```rust
use memorysafe_core::{
    ADMIN_COMPONENT, Actor, ActorKind, Namespace, PURGED_COMPONENT, Scope, SubjectId, TenantId,
};
```

- [ ] **Step 10: Update the existing `store.rs` tests to the new signatures**

Change the helper to mint with a subject:

```rust
    fn store_with(label: &str) -> (ApiKeyStore, String, TenantId) {
        let tenant = TenantId::new("acme").unwrap();
        let subject = SubjectId::new("user-42").unwrap();
        let g = generate(tenant.clone(), subject, label).unwrap();
        (ApiKeyStore::new(vec![g.record]), g.secret, tenant)
    }
```

Then fix the call sites that no longer typecheck:

- `an_unknown_id_is_rejected_with_the_same_error_as_a_wrong_secret`: `generate(TenantId::new("acme").unwrap(), SubjectId::new("user-42").unwrap(), "elsewhere")`
- `a_disabled_key_does_not_authenticate`: same insertion.
- `a_key_builds_scopes_only_inside_its_own_tenant`: `auth.scope("coding-agent")`.
- `the_reserved_component_is_refused_in_either_position`: delete the two subject-position assertions (`auth.scope(ADMIN_COMPONENT, "agent")`) — that position no longer exists — and keep the namespace-position one as `auth.scope(ADMIN_COMPONENT)`. Rename the test to `the_reserved_component_is_refused_as_a_namespace`, and add a comment recording *why* the subject case left: it moved to `generate`, covered by `a_key_may_not_be_minted_for_a_reserved_subject` in `key.rs`.
- `purged_is_refused_in_either_position`: same treatment, renamed `purged_is_refused_as_a_namespace`.
- `an_invalid_component_surfaces_as_a_scope_error_not_a_panic`: becomes `auth.scope("Agent-Caps")` and `auth.scope("")`, both still `Err(AuthError::Scope(_))`.

- [ ] **Step 11: Run the whole auth crate to verify it passes**

Run: `cargo test -p memorysafe-auth`
Expected: PASS. `memorysafe-mcp`, `memorysafe-api`, and `memorysafe-cli` will not compile yet — that is expected and is Tasks 4, 6, and 7.

- [ ] **Step 12: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy -p memorysafe-auth --all-targets -- -D warnings
git add crates/memorysafe-auth/src/key.rs crates/memorysafe-auth/src/store.rs
git commit -m "feat(auth): a key names the subject it acts as, and scope takes only a namespace"
```

---

## Task 2: Auth — the `ScopeResolver` trait and `FixedScope`

**Files:**
- Create: `crates/memorysafe-auth/src/resolver.rs`
- Modify: `crates/memorysafe-auth/src/lib.rs`
- Modify: `crates/memorysafe-auth/Cargo.toml`

**Interfaces:**
- Consumes: Task 1's `Authenticated`, `check_reserved`.
- Produces:
  - `pub const NAMESPACE_HEADER: &str = "memorysafe-namespace"`
  - `pub const FALLBACK_NAMESPACE: &str = "default"`
  - `pub struct Resolved { pub scope: Scope, pub actor: Actor }`
  - `pub trait ScopeResolver: Send + Sync + 'static { fn default_scope(&self) -> Option<Scope>; fn resolve(&self, headers: &HeaderMap, namespace: Option<&str>) -> Result<Resolved, AuthError>; }`
  - `FixedScope::new(tenant: TenantId, subject: SubjectId, default_namespace: Namespace) -> Result<FixedScope, AuthError>`

- [ ] **Step 1: Add the `http` dependency**

In `crates/memorysafe-auth/Cargo.toml`, add to `[dependencies]`, keeping the list alphabetical:

```toml
http.workspace = true
```

`http` is a types-only crate — header maps, methods, status codes — with no I/O. `memorysafe-auth` is bound by neither of the workspace's purity rules (those cover `memorysafe-core` and `memorysafe-embed`), and both adapters already depend on it.

- [ ] **Step 2: Write the failing test for `FixedScope`**

Create `crates/memorysafe-auth/src/resolver.rs` containing only its test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::{ADMIN_COMPONENT, ActorKind, PURGED_COMPONENT};

    fn fixed() -> FixedScope {
        FixedScope::new(
            TenantId::new("acme").unwrap(),
            SubjectId::new("user-42").unwrap(),
            Namespace::new("coding-agent").unwrap(),
        )
        .expect("an ordinary configuration")
    }

    #[test]
    fn a_fixed_scope_uses_its_configured_scope_when_a_call_names_nothing() {
        let r = fixed()
            .resolve(&HeaderMap::new(), None)
            .expect("resolve");
        assert_eq!(r.scope.tenant.as_str(), "acme");
        assert_eq!(r.scope.subject.as_str(), "user-42");
        assert_eq!(r.scope.namespace.as_str(), "coding-agent");
        assert_eq!(r.actor.kind, ActorKind::Agent);
        assert_eq!(r.actor.id, None);
    }

    #[test]
    fn a_call_may_override_the_namespace() {
        let r = fixed()
            .resolve(&HeaderMap::new(), Some("notes"))
            .expect("resolve");
        assert_eq!(r.scope.namespace.as_str(), "notes");
        assert_eq!(
            r.scope.subject.as_str(),
            "user-42",
            "overriding the namespace must not move the subject"
        );
    }

    #[test]
    fn default_scope_agrees_with_what_resolve_produces_with_no_overrides() {
        // `default_scope` is a second, independent construction of the same
        // scope. `list_resources` advertises URIs built from it and
        // `read_resource` goes through `resolve`; if the two disagreed, the
        // server would advertise a URI it would then refuse.
        assert_eq!(
            fixed().default_scope(),
            Some(fixed().resolve(&HeaderMap::new(), None).unwrap().scope)
        );
    }

    #[test]
    fn a_reserved_namespace_is_refused_whether_configured_or_named() {
        for word in [ADMIN_COMPONENT, PURGED_COMPONENT] {
            // Configured: refused at construction, so an unchecked
            // `FixedScope` cannot exist to be served from.
            assert!(
                matches!(
                    FixedScope::new(
                        TenantId::new("acme").unwrap(),
                        SubjectId::new("user-42").unwrap(),
                        Namespace::new(word).unwrap(),
                    ),
                    Err(AuthError::Reserved { .. })
                ),
                "'{word}' was accepted as a configured default namespace"
            );

            // Named per call: refused at resolve.
            assert!(
                matches!(
                    fixed().resolve(&HeaderMap::new(), Some(word)),
                    Err(AuthError::Reserved { .. })
                ),
                "'{word}' was accepted as a per-call namespace"
            );
        }
    }

    #[test]
    fn a_reserved_subject_cannot_be_configured() {
        for word in [ADMIN_COMPONENT, PURGED_COMPONENT] {
            assert!(
                matches!(
                    FixedScope::new(
                        TenantId::new("acme").unwrap(),
                        SubjectId::new(word).unwrap(),
                        Namespace::new("agent").unwrap(),
                    ),
                    Err(AuthError::Reserved { .. })
                ),
                "'{word}' was accepted as a configured subject"
            );
        }
    }

    #[test]
    fn an_illegal_namespace_is_a_scope_error_not_a_panic() {
        assert!(matches!(
            fixed().resolve(&HeaderMap::new(), Some("Caps-Are-Illegal")),
            Err(AuthError::Scope(_))
        ));
        assert!(matches!(
            fixed().resolve(&HeaderMap::new(), Some("")),
            Err(AuthError::Scope(_))
        ));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p memorysafe-auth --lib resolver::`
Expected: FAIL to compile — `resolver` is not a module of the crate yet, and `FixedScope` does not exist.

- [ ] **Step 4: Write the trait, `Resolved`, and `FixedScope`**

Prepend to `crates/memorysafe-auth/src/resolver.rs`, above the test module:

```rust
//! How a call's scope is established, once, for every adapter.
//!
//! The rule this module enforces is the whole point of it: **tenant and
//! subject come from the credential; the namespace is the only component a
//! caller supplies.** That is why `resolve` has no `subject` parameter — the
//! rule is a signature, not a convention a reviewer has to remember.

use crate::{AuthError, Authenticated, check_reserved};
use http::HeaderMap;
use memorysafe_core::{Actor, ActorKind, Namespace, Scope, SubjectId, TenantId};

/// The header a request declares its namespace in.
///
/// Lowercase because `http::HeaderMap` lookups are case-insensitive but the
/// constant is also used to *build* requests and to print setup instructions,
/// where a single spelling avoids a needless second one.
pub const NAMESPACE_HEADER: &str = "memorysafe-namespace";

/// The namespace used when nothing else supplies one.
pub const FALLBACK_NAMESPACE: &str = "default";

/// A resolved call: which scope it acts in, and who to attribute it to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub scope: Scope,
    pub actor: Actor,
}

/// How a call's scope and actor are established, once per deployment shape.
///
/// Implementors are composition roots: a binary that builds one is asserting
/// "this is how callers are identified here". Tool and handler bodies never
/// implement this — they only ever *call* `resolve`, which is what keeps a
/// scope unobtainable without a credential check.
pub trait ScopeResolver: Send + Sync + 'static {
    /// The scope a client sees before it names one. `None` where scope is a
    /// property of the request rather than of the server.
    fn default_scope(&self) -> Option<Scope>;

    /// Resolve one call. `namespace` is the only caller-supplied component;
    /// `headers` carries the credential and the connection's declared
    /// namespace, and is empty for transports that have neither.
    fn resolve(&self, headers: &HeaderMap, namespace: Option<&str>)
    -> Result<Resolved, AuthError>;
}

/// Resolve a namespace under the contract's precedence: the call's argument,
/// then the connection's declared namespace, then the credential's own
/// default, then [`FALLBACK_NAMESPACE`].
///
/// Shared by both implementations rather than written twice, because "the two
/// resolvers agree" is the property the whole design rests on and duplicated
/// precedence logic is the obvious way to lose it.
pub(crate) fn pick_namespace(
    call: Option<&str>,
    headers: &HeaderMap,
    credential_default: Option<&Namespace>,
) -> Result<Namespace, AuthError> {
    if let Some(raw) = call {
        check_reserved(None, Some(raw))?;
        return Ok(Namespace::new(raw)?);
    }
    if let Some(raw) = headers.get(NAMESPACE_HEADER).and_then(|v| v.to_str().ok()) {
        let raw = raw.trim();
        if !raw.is_empty() {
            check_reserved(None, Some(raw))?;
            return Ok(Namespace::new(raw)?);
        }
    }
    if let Some(ns) = credential_default {
        return Ok(ns.clone());
    }
    Ok(Namespace::new(FALLBACK_NAMESPACE)?)
}

/// The credential is process configuration: a `msafe` serving one developer
/// over stdio, or a single-tenant local HTTP server.
///
/// Named for the binding rather than the transport, because the binding is
/// what it describes — a local `msafe serve --transport http` uses this too.
#[derive(Debug, Clone)]
pub struct FixedScope {
    tenant: TenantId,
    subject: SubjectId,
    default_namespace: Namespace,
}

impl FixedScope {
    /// Refuses a reserved subject or namespace at construction.
    ///
    /// At construction, not at resolve: these are operator-configured values
    /// that `resolve` never inspects, so a deployment configured with
    /// `_admin` would otherwise serve every call into a reserved scope. Making
    /// it a constructor error means an unchecked `FixedScope` cannot exist.
    pub fn new(
        tenant: TenantId,
        subject: SubjectId,
        default_namespace: Namespace,
    ) -> Result<Self, AuthError> {
        check_reserved(Some(subject.as_str()), Some(default_namespace.as_str()))?;
        Ok(Self {
            tenant,
            subject,
            default_namespace,
        })
    }
}

impl ScopeResolver for FixedScope {
    fn default_scope(&self) -> Option<Scope> {
        Some(Scope {
            tenant: self.tenant.clone(),
            subject: self.subject.clone(),
            namespace: self.default_namespace.clone(),
        })
    }

    fn resolve(
        &self,
        headers: &HeaderMap,
        namespace: Option<&str>,
    ) -> Result<Resolved, AuthError> {
        let namespace = pick_namespace(namespace, headers, Some(&self.default_namespace))?;
        Ok(Resolved {
            scope: Scope {
                tenant: self.tenant.clone(),
                subject: self.subject.clone(),
                namespace,
            },
            actor: Actor {
                kind: ActorKind::Agent,
                id: None,
            },
        })
    }
}
```

Note `pick_namespace` is passed `Some(&self.default_namespace)` here, so a `FixedScope` honours a `MemorySafe-Namespace` header when one is present but prefers its own configured default over the bare `FALLBACK_NAMESPACE`. Over stdio no headers exist, so this collapses to the configured default — identical to today's behaviour.

- [ ] **Step 5: Export the module**

In `crates/memorysafe-auth/src/lib.rs`, add the module and its re-exports beside the existing ones:

```rust
mod key;
mod resolver;
mod store;

pub use key::{ApiKeyRecord, GeneratedKey, KEY_PREFIX, generate};
pub use resolver::{
    FALLBACK_NAMESPACE, FixedScope, NAMESPACE_HEADER, Resolved, ScopeResolver,
};
pub use store::{ApiKeyStore, Authenticated, check_reserved};
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p memorysafe-auth --lib resolver::`
Expected: PASS, 6 tests.

- [ ] **Step 7: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy -p memorysafe-auth --all-targets -- -D warnings
git add crates/memorysafe-auth/Cargo.toml crates/memorysafe-auth/src/lib.rs crates/memorysafe-auth/src/resolver.rs
git commit -m "feat(auth): the scope contract as a trait, with the fixed-credential resolver"
```

---

## Task 3: Auth — `ApiKeyScope`, and the parity suite both resolvers must satisfy

**Files:**
- Modify: `crates/memorysafe-auth/src/resolver.rs`
- Modify: `crates/memorysafe-auth/src/lib.rs`
- Create: `crates/memorysafe-auth/tests/parity.rs`

**Interfaces:**
- Consumes: Task 2's `ScopeResolver`, `pick_namespace`, `Resolved`; Task 1's `Authenticated`.
- Produces: `ApiKeyScope::new(keys: Arc<ApiKeyStore>) -> ApiKeyScope`, implementing `ScopeResolver`.

- [ ] **Step 1: Write the failing test for `ApiKeyScope`**

Add to the `tests` module in `crates/memorysafe-auth/src/resolver.rs`:

```rust
    use crate::{ApiKeyStore, generate};
    use std::sync::Arc;

    fn keyed(label: &str) -> (ApiKeyScope, String, String) {
        let g = generate(
            TenantId::new("acme").unwrap(),
            SubjectId::new("user-42").unwrap(),
            label,
        )
        .unwrap();
        let key_id = g.record.id.clone();
        let secret = g.secret.clone();
        (
            ApiKeyScope::new(Arc::new(ApiKeyStore::new(vec![g.record]))),
            secret,
            key_id,
        )
    }

    fn bearer(secret: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {secret}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn an_api_key_supplies_both_tenant_and_subject() {
        let (source, secret, key_id) = keyed("ci");
        let r = source.resolve(&bearer(&secret), Some("agent")).expect("resolve");

        assert_eq!(r.scope.tenant.as_str(), "acme");
        assert_eq!(r.scope.subject.as_str(), "user-42");
        assert_eq!(r.scope.namespace.as_str(), "agent");
        assert_eq!(r.actor.kind, ActorKind::ApiKey);
        assert_eq!(r.actor.id.as_deref(), Some(key_id.as_str()));
    }

    #[test]
    fn a_call_with_no_namespace_anywhere_lands_in_the_fallback() {
        let (source, secret, _) = keyed("ci");
        let r = source.resolve(&bearer(&secret), None).expect("resolve");
        assert_eq!(r.scope.namespace.as_str(), FALLBACK_NAMESPACE);
    }

    #[test]
    fn the_namespace_header_supplies_a_default_a_call_can_still_override() {
        let (source, secret, _) = keyed("ci");
        let mut headers = bearer(&secret);
        headers.insert(NAMESPACE_HEADER, "checkout-service".parse().unwrap());

        assert_eq!(
            source.resolve(&headers, None).unwrap().scope.namespace.as_str(),
            "checkout-service"
        );
        assert_eq!(
            source.resolve(&headers, Some("notes")).unwrap().scope.namespace.as_str(),
            "notes",
            "the call argument outranks the header"
        );
    }

    #[test]
    fn a_keys_own_default_namespace_outranks_the_fallback_but_not_the_header() {
        let mut g = generate(
            TenantId::new("acme").unwrap(),
            SubjectId::new("user-42").unwrap(),
            "ci",
        )
        .unwrap();
        g.record.default_namespace = Some(Namespace::new("from-key").unwrap());
        let secret = g.secret.clone();
        let source = ApiKeyScope::new(Arc::new(ApiKeyStore::new(vec![g.record])));

        assert_eq!(
            source.resolve(&bearer(&secret), None).unwrap().scope.namespace.as_str(),
            "from-key"
        );

        let mut headers = bearer(&secret);
        headers.insert(NAMESPACE_HEADER, "from-header".parse().unwrap());
        assert_eq!(
            source.resolve(&headers, None).unwrap().scope.namespace.as_str(),
            "from-header"
        );
    }

    #[test]
    fn without_a_credential_nothing_resolves() {
        let (source, secret, _) = keyed("ci");

        assert!(matches!(
            source.resolve(&HeaderMap::new(), Some("agent")),
            Err(AuthError::Missing)
        ));

        let mut bare = HeaderMap::new();
        bare.insert(http::header::AUTHORIZATION, secret.parse().unwrap());
        assert!(
            source.resolve(&bare, Some("agent")).is_err(),
            "a bare key without the Bearer scheme must be refused"
        );

        let unknown = bearer(
            "msk_00000000000000000000000000_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        assert!(matches!(
            source.resolve(&unknown, Some("agent")),
            Err(AuthError::Unknown)
        ));
    }

    #[test]
    fn the_bearer_scheme_is_case_insensitive_but_still_needs_its_space() {
        let (source, secret, _) = keyed("ci");
        for scheme in ["Bearer", "bearer", "BEARER", "BeArEr"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                http::header::AUTHORIZATION,
                format!("{scheme} {secret}").parse().unwrap(),
            );
            assert!(
                source.resolve(&headers, Some("agent")).is_ok(),
                "scheme '{scheme}' was refused"
            );
        }

        let mut glued = HeaderMap::new();
        glued.insert(
            http::header::AUTHORIZATION,
            format!("Bearer{secret}").parse().unwrap(),
        );
        assert!(source.resolve(&glued, Some("agent")).is_err());
    }

    #[test]
    fn an_api_key_source_advertises_no_default_scope() {
        let (source, _, _) = keyed("ci");
        assert!(
            source.default_scope().is_none(),
            "scope is a property of the request here, not of the server"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p memorysafe-auth --lib resolver::tests::an_api_key_supplies_both_tenant_and_subject`
Expected: FAIL to compile — `ApiKeyScope` does not exist.

- [ ] **Step 3: Write `ApiKeyScope` and `strip_bearer`**

Append to the implementation half of `crates/memorysafe-auth/src/resolver.rs`, above the test module:

```rust
/// Strips a case-insensitive `Bearer` scheme from an `Authorization` value,
/// still requiring exactly the one space `strip_prefix("Bearer ")` requires.
///
/// RFC 7235 auth schemes are case-insensitive, so `bearer <token>` is a legal
/// credential and refusing it would reject a spec-conformant client for no
/// reason this crate has a stake in. `"Bearerx"` — no space, a prefix
/// collision with no scheme boundary — is still refused.
fn strip_bearer(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(rest)
}

/// The credential is a bearer API key presented per request.
#[derive(Clone)]
pub struct ApiKeyScope {
    keys: std::sync::Arc<crate::ApiKeyStore>,
}

impl ApiKeyScope {
    pub fn new(keys: std::sync::Arc<crate::ApiKeyStore>) -> Self {
        Self { keys }
    }

    /// Authenticate the credential on a request, without resolving a scope.
    ///
    /// Exposed because `memorysafe-api` needs an `Authenticated` for routes
    /// that are tenant-scoped rather than scope-scoped — `/v1/whoami` and the
    /// admin budget/policy/retention routes, which name a tenant and no
    /// subject at all.
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<Authenticated, AuthError> {
        let presented = headers
            .get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(strip_bearer)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or(AuthError::Missing)?;
        self.keys.authenticate(presented)
    }
}

impl ScopeResolver for ApiKeyScope {
    fn default_scope(&self) -> Option<Scope> {
        // Over a keyed transport the scope is a property of the request, not
        // of the server, so there is nothing concrete to advertise.
        None
    }

    fn resolve(
        &self,
        headers: &HeaderMap,
        namespace: Option<&str>,
    ) -> Result<Resolved, AuthError> {
        let auth = self.authenticate(headers)?;
        let namespace = pick_namespace(namespace, headers, auth.default_namespace())?;
        Ok(Resolved {
            scope: auth.scope(namespace.as_str())?,
            actor: auth.actor(),
        })
    }
}
```

- [ ] **Step 4: Export it**

In `crates/memorysafe-auth/src/lib.rs`:

```rust
pub use resolver::{
    ApiKeyScope, FALLBACK_NAMESPACE, FixedScope, NAMESPACE_HEADER, Resolved, ScopeResolver,
};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p memorysafe-auth --lib resolver::`
Expected: PASS, 13 tests.

- [ ] **Step 6: Write the parity suite**

Create `crates/memorysafe-auth/tests/parity.rs`:

```rust
//! The one property the whole design rests on: **the two resolvers behave
//! identically for identical inputs.**
//!
//! Every other test in this workspace checks one resolver. This checks that
//! they agree, which is the claim a user relies on when they point the same
//! agent at a local server and a hosted one. It is written as a table run
//! twice rather than two parallel test modules, because two modules drift and
//! a table cannot.

use http::HeaderMap;
use memorysafe_auth::{
    ApiKeyScope, ApiKeyStore, AuthError, FALLBACK_NAMESPACE, FixedScope, NAMESPACE_HEADER,
    ScopeResolver, generate,
};
use memorysafe_core::{ADMIN_COMPONENT, Namespace, PURGED_COMPONENT, SubjectId, TenantId};
use std::sync::Arc;

const TENANT: &str = "acme";
const SUBJECT: &str = "user-42";

/// A `FixedScope` whose configured default namespace is the fallback, so it
/// starts from the same place `ApiKeyScope` does with no header and no key
/// default. Without this the two would differ for a reason that is
/// configuration, not contract, and the table would be meaningless.
fn fixed() -> (Box<dyn ScopeResolver>, HeaderMap) {
    let resolver = FixedScope::new(
        TenantId::new(TENANT).unwrap(),
        SubjectId::new(SUBJECT).unwrap(),
        Namespace::new(FALLBACK_NAMESPACE).unwrap(),
    )
    .expect("an ordinary configuration");
    (Box::new(resolver), HeaderMap::new())
}

fn keyed() -> (Box<dyn ScopeResolver>, HeaderMap) {
    let g = generate(
        TenantId::new(TENANT).unwrap(),
        SubjectId::new(SUBJECT).unwrap(),
        "parity",
    )
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        format!("Bearer {}", g.secret).parse().unwrap(),
    );
    (
        Box::new(ApiKeyScope::new(Arc::new(ApiKeyStore::new(vec![g.record])))),
        headers,
    )
}

fn both() -> Vec<(&'static str, Box<dyn ScopeResolver>, HeaderMap)> {
    let (f, fh) = fixed();
    let (k, kh) = keyed();
    vec![("FixedScope", f, fh), ("ApiKeyScope", k, kh)]
}

#[test]
fn both_resolvers_agree_on_tenant_and_subject() {
    for (name, resolver, headers) in both() {
        let r = resolver.resolve(&headers, Some("agent")).expect(name);
        assert_eq!(r.scope.tenant.as_str(), TENANT, "{name}");
        assert_eq!(r.scope.subject.as_str(), SUBJECT, "{name}");
    }
}

#[test]
fn both_resolvers_agree_on_namespace_precedence() {
    // (call argument, header value) -> resolved namespace.
    let cases: &[(Option<&str>, Option<&str>, &str)] = &[
        (None, None, FALLBACK_NAMESPACE),
        (None, Some("from-header"), "from-header"),
        (Some("from-call"), None, "from-call"),
        (Some("from-call"), Some("from-header"), "from-call"),
    ];

    for (name, resolver, base) in both() {
        for (call, header, expected) in cases {
            let mut headers = base.clone();
            if let Some(value) = header {
                headers.insert(NAMESPACE_HEADER, value.parse().unwrap());
            }
            let r = resolver
                .resolve(&headers, *call)
                .unwrap_or_else(|e| panic!("{name} rejected {call:?}/{header:?}: {e}"));
            assert_eq!(
                r.scope.namespace.as_str(),
                *expected,
                "{name} with call={call:?} header={header:?}"
            );
        }
    }
}

#[test]
fn both_resolvers_refuse_a_reserved_namespace_identically() {
    for (name, resolver, base) in both() {
        for word in [ADMIN_COMPONENT, PURGED_COMPONENT] {
            assert!(
                matches!(
                    resolver.resolve(&base, Some(word)),
                    Err(AuthError::Reserved { .. })
                ),
                "{name} accepted '{word}' as a call namespace"
            );

            let mut headers = base.clone();
            headers.insert(NAMESPACE_HEADER, word.parse().unwrap());
            assert!(
                matches!(
                    resolver.resolve(&headers, None),
                    Err(AuthError::Reserved { .. })
                ),
                "{name} accepted '{word}' in the namespace header"
            );
        }
    }
}

#[test]
fn both_resolvers_refuse_an_illegal_namespace_identically() {
    for (name, resolver, base) in both() {
        for bad in ["Caps-Are-Illegal", "", "has spaces"] {
            assert!(
                matches!(
                    resolver.resolve(&base, Some(bad)),
                    Err(AuthError::Scope(_))
                ),
                "{name} accepted '{bad}' as a namespace"
            );
        }
    }
}

#[test]
fn neither_resolver_offers_any_way_to_name_a_subject() {
    // A compile-time property stated as a runtime test so it is discoverable:
    // `resolve` takes exactly one caller-supplied component. If a `subject`
    // parameter is ever restored, this test's call sites stop compiling, and
    // the failure names the reason.
    for (name, resolver, base) in both() {
        let r = resolver.resolve(&base, Some("agent")).expect(name);
        assert_eq!(
            r.scope.subject.as_str(),
            SUBJECT,
            "{name}: the subject must come from the credential, never a call"
        );
    }
}
```

- [ ] **Step 7: Run the parity suite to verify it passes**

Run: `cargo test -p memorysafe-auth --test parity`
Expected: PASS, 5 tests.

- [ ] **Step 8: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy -p memorysafe-auth --all-targets -- -D warnings
git add crates/memorysafe-auth/src/lib.rs crates/memorysafe-auth/src/resolver.rs crates/memorysafe-auth/tests/parity.rs
git commit -m "feat(auth): the api-key resolver, and a suite pinning both resolvers to one contract"
```

---

## Task 4: MCP — adopt the resolver and drop `subject` from the tool surface

**Files:**
- Modify: `crates/memorysafe-mcp/src/scope.rs`
- Modify: `crates/memorysafe-mcp/src/dto.rs`
- Modify: `crates/memorysafe-mcp/src/tools_write.rs`
- Modify: `crates/memorysafe-mcp/src/tools_curate.rs`
- Modify: `crates/memorysafe-mcp/src/lib.rs`
- Modify: `crates/memorysafe-mcp/src/transport.rs`
- Modify: `crates/memorysafe-mcp/tests/write_tools.rs`, `tests/curate_tools.rs`, `tests/stdio_transport.rs`, `tests/http_transport.rs`, `tests/support/mod.rs`

**Interfaces:**
- Consumes: `memorysafe_auth::{ScopeResolver, Resolved, FixedScope, ApiKeyScope, AuthError, NAMESPACE_HEADER}`.
- Produces:
  - `MemorySafeServer::new(engine: Arc<Engine>, resolver: Arc<dyn ScopeResolver>) -> MemorySafeServer`
  - `serve_stdio(engine: Arc<Engine>, resolver: Arc<dyn ScopeResolver>) -> anyhow::Result<()>`
  - `http_service_with(engine: Arc<Engine>, resolver: Arc<dyn ScopeResolver>, config: HttpTransportConfig) -> StreamableHttpService<..>`
  - `memorysafe_mcp::scope::resolve_call(resolver: &dyn ScopeResolver, extensions: &Extensions, namespace: Option<&str>) -> Result<Resolved, ErrorData>`

- [ ] **Step 1: Write the failing schema-snapshot test**

Add to `crates/memorysafe-mcp/src/dto.rs`'s test module:

```rust
    /// The field that used to lie. Its schema said optional on both
    /// transports while HTTP hard-required it, so a model had to know which
    /// transport it was on to call correctly. It is gone; this stops it
    /// coming back.
    #[test]
    fn no_tool_parameter_schema_declares_a_subject() {
        use schemars::schema_for;

        let schemas = [
            ("RememberParams", serde_json::to_string(&schema_for!(RememberParams)).unwrap()),
            ("RecallParams", serde_json::to_string(&schema_for!(RecallParams)).unwrap()),
            ("ReviewParams", serde_json::to_string(&schema_for!(ReviewParams)).unwrap()),
            ("ForgetParams", serde_json::to_string(&schema_for!(ForgetParams)).unwrap()),
            ("ProtectParams", serde_json::to_string(&schema_for!(ProtectParams)).unwrap()),
        ];

        for (name, json) in schemas {
            assert!(
                !json.contains("\"subject\""),
                "{name} still declares a `subject` property: {json}"
            );
            assert!(
                json.contains("\"namespace\""),
                "{name} must still accept a per-call namespace override: {json}"
            );
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p memorysafe-mcp --lib dto::tests::no_tool_parameter_schema_declares_a_subject`
Expected: FAIL — every params struct still declares `subject`.

- [ ] **Step 3: Drop `subject` from the five param structs**

In `crates/memorysafe-mcp/src/dto.rs`, delete the `subject` field from `RememberParams`, `RecallParams`, `ReviewParams`, `ForgetParams`, and `ProtectParams`, and give the surviving `namespace` field a description that states the contract rather than the transport:

```rust
    /// Which namespace to act in. Defaults to the namespace this connection
    /// declared — the working directory over stdio, the
    /// `MemorySafe-Namespace` header over HTTP — and falls back to
    /// `default`. Tenant and subject are fixed by the credential and cannot
    /// be named here.
    pub namespace: Option<String>,
```

Apply that same doc comment to all five, replacing both the old `subject` and `namespace` comments.

- [ ] **Step 4: Rewrite `scope.rs` as the error-mapping seam**

Replace the whole implementation half of `crates/memorysafe-mcp/src/scope.rs` (keep nothing but the file):

```rust
//! The MCP adapter's view of the scope contract.
//!
//! The contract itself lives in `memorysafe-auth`, so this adapter and the
//! HTTP one cannot disagree about it. What lives here is the two things that
//! are genuinely MCP's: pulling the request's headers out of an rmcp
//! `Extensions`, and mapping `AuthError` onto `ErrorData`.

use memorysafe_auth::{AuthError, Resolved, ScopeResolver};
use rmcp::ErrorData;
use rmcp::model::Extensions;

/// The MCP error surface has no status codes, so the distinction 401 and 403
/// draw is carried in the message rather than lost.
pub fn auth_error(e: AuthError) -> ErrorData {
    ErrorData::invalid_params(e.to_string(), None)
}

/// Resolve one tool or resource call.
///
/// Over stdio there is no HTTP request at all, so an absent `Parts` yields an
/// empty header map rather than an error — a `FixedScope` needs no headers,
/// and an `ApiKeyScope` will refuse the call anyway for want of a credential.
/// That keeps "no credential" as the reason a keyed transport rejects an
/// unauthenticated call, instead of a confusing "no HTTP request context".
pub fn resolve_call(
    resolver: &dyn ScopeResolver,
    extensions: &Extensions,
    namespace: Option<&str>,
) -> Result<Resolved, ErrorData> {
    let headers = extensions
        .get::<http::request::Parts>()
        .map(|parts| parts.headers.clone())
        .unwrap_or_default();
    resolver.resolve(&headers, namespace).map_err(auth_error)
}
```

Delete `ScopeSource`, `Resolved`, `invalid`, `strip_bearer`, and the whole old test module — every behaviour they covered now lives in `memorysafe-auth`'s `resolver::tests` and `tests/parity.rs`.

- [ ] **Step 5: Point the server at the trait**

In `crates/memorysafe-mcp/src/lib.rs`, change the struct, its constructor, and the two re-export lines:

```rust
pub use scope::{auth_error, resolve_call};
pub use transport::{HttpTransportConfig, http_service, http_service_with, serve_stdio};

#[derive(Clone)]
pub struct MemorySafeServer {
    pub(crate) engine: Arc<Engine>,
    pub(crate) resolver: Arc<dyn ScopeResolver>,
    tool_router: ToolRouter<Self>,
}

impl MemorySafeServer {
    pub fn new(engine: Arc<Engine>, resolver: Arc<dyn ScopeResolver>) -> Self {
        Self {
            engine,
            resolver,
            tool_router: Self::write_router() + Self::curate_router(),
        }
    }
}
```

Update the import line to `use memorysafe_auth::ScopeResolver;` and drop `pub use scope::{Resolved, ScopeSource};`.

In `list_resources`, change `self.source.default_scope()` to `self.resolver.default_scope()`.

- [ ] **Step 6: Update the five tool call sites**

In `crates/memorysafe-mcp/src/tools_write.rs` and `tools_curate.rs`, every call currently reads:

```rust
        let resolved = self.source.resolve(
            &ctx.extensions,
            params.subject.as_deref(),
            params.namespace.as_deref(),
        )?;
```

Replace each of the five with:

```rust
        let resolved = crate::scope::resolve_call(
            self.resolver.as_ref(),
            &ctx.extensions,
            params.namespace.as_deref(),
        )?;
```

- [ ] **Step 7: Update the transports**

In `crates/memorysafe-mcp/src/transport.rs`, delete `reject_reserved_configuration` and its test module entirely — `FixedScope::new` now refuses a reserved configuration at construction, so there is no unchecked value left for a startup check to catch. Then:

```rust
use crate::MemorySafeServer;
use memorysafe_auth::ScopeResolver;
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
pub async fn serve_stdio(
    engine: Arc<Engine>,
    resolver: Arc<dyn ScopeResolver>,
) -> anyhow::Result<()> {
    let running = MemorySafeServer::new(engine, resolver)
        .serve(stdio())
        .await?;
    running.waiting().await?;
    Ok(())
}

pub fn http_service_with(
    engine: Arc<Engine>,
    resolver: Arc<dyn ScopeResolver>,
    config: HttpTransportConfig,
) -> StreamableHttpService<MemorySafeServer, LocalSessionManager> {
    let mut server_config = StreamableHttpServerConfig::default();
    server_config.allowed_hosts = config.allowed_hosts;
    StreamableHttpService::new(
        move || Ok(MemorySafeServer::new(engine.clone(), resolver.clone())),
        Arc::new(LocalSessionManager::default()),
        server_config
            .with_legacy_session_mode(false)
            .with_json_response(true),
    )
}

pub fn http_service(
    engine: Arc<Engine>,
    resolver: Arc<dyn ScopeResolver>,
) -> StreamableHttpService<MemorySafeServer, LocalSessionManager> {
    http_service_with(engine, resolver, HttpTransportConfig::default())
}
```

Leave `HttpTransportConfig` and its `Default` impl exactly as they are.

- [ ] **Step 8: Update the integration tests**

In `crates/memorysafe-mcp/tests/support/mod.rs`, replace whatever builds a `ScopeSource::Stdio` with:

```rust
pub fn fixed_resolver() -> std::sync::Arc<dyn memorysafe_auth::ScopeResolver> {
    std::sync::Arc::new(
        memorysafe_auth::FixedScope::new(
            memorysafe_core::TenantId::new("acme").unwrap(),
            memorysafe_core::SubjectId::new("user-42").unwrap(),
            memorysafe_core::Namespace::new("coding-agent").unwrap(),
        )
        .expect("an ordinary test configuration"),
    )
}
```

Across `tests/write_tools.rs`, `tests/curate_tools.rs`, `tests/resources.rs`, and `tests/stdio_transport.rs`: delete every `"subject": "..."` entry from tool-call argument objects, and replace `ScopeSource::Stdio { .. }` construction with `support::fixed_resolver()`.

In `tests/http_transport.rs`, replace the `ScopeSource::Http { keys }` construction with `Arc::new(ApiKeyScope::new(keys))`, mint the key with a subject, drop `"subject"` from every call's arguments, and add the namespace header to the client's requests. Then add one test that pins the behaviour this whole plan exists for:

```rust
#[tokio::test]
async fn a_call_naming_no_scope_at_all_succeeds_over_http_exactly_as_it_does_over_stdio() {
    // The defect this design closes: `memory_remember{text}` used to work
    // over stdio and fail with `'subject' is required over HTTP`. The agent
    // had no way to tell which transport it was on, so the same prompt could
    // not work against both a local and a hosted server.
    let (server, key) = http_server_with_key().await;

    let result = server
        .call_tool("memory_remember", serde_json::json!({ "body": "the API is versioned" }))
        .await
        .expect("a call naming no scope must succeed over HTTP");

    assert!(!result.is_error.unwrap_or(false), "{result:?}");
}
```

Adapt `http_server_with_key` to whatever the file's existing harness is named — the point is a real streamable-HTTP round trip with a bearer key and no `subject` anywhere.

- [ ] **Step 9: Run the MCP crate to verify it passes**

Run: `cargo test -p memorysafe-mcp`
Expected: PASS, including the new schema-snapshot test and the no-scope HTTP call.

- [ ] **Step 10: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy -p memorysafe-mcp --all-targets -- -D warnings
git add crates/memorysafe-mcp/
git commit -m "feat(mcp): one scope contract for both transports, and subject leaves the tool surface"
```

---

## Task 5: MCP — a resource URI's subject is checked, not trusted

**Files:**
- Modify: `crates/memorysafe-mcp/src/lib.rs`
- Modify: `crates/memorysafe-mcp/tests/resources.rs`

**Interfaces:**
- Consumes: Task 4's `resolve_call`, `resources::parse_uri`.
- Produces: no new public API; changes `read_resource`'s rejection behaviour.

**Why this is its own task.** The resource URI is the one place a subject is still parsed from caller input — `memorysafe://{tenant}/{subject}/{namespace}/audit` names a resource, so the component has to stay in the grammar. Today `read_resource` routes that parsed subject *through* `resolve` and separately compares only the tenant. After Task 4 there is no subject parameter to route it through, so unless it is compared explicitly, a caller could read another subject's audit trail through a URI while every tool-level test still passes. This is the single way Task 4's rule could be bypassed, which is why it gets its own test cycle and its own reviewer gate.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-mcp/tests/resources.rs`:

```rust
#[tokio::test]
async fn a_resource_uri_naming_another_subject_is_not_found() {
    // The URI grammar still carries a subject, because it identifies a
    // resource. That does not make it caller-supplied scope: it must be
    // compared against the credential's subject exactly as the tenant is.
    let server = support::server().await;

    let mine = server
        .read_resource("memorysafe://acme/user-42/coding-agent/audit")
        .await;
    assert!(mine.is_ok(), "a caller must still read their own audit");

    for uri in [
        "memorysafe://acme/someone-else/coding-agent/audit",
        "memorysafe://acme/someone-else/coding-agent/stats",
        "memorysafe://globex/user-42/coding-agent/audit",
    ] {
        let err = server
            .read_resource(uri)
            .await
            .expect_err("reading another scope's resource must fail");
        assert!(
            format!("{err:?}").contains("no such resource"),
            "{uri} was refused with the wrong error: {err:?}"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p memorysafe-mcp --test resources a_resource_uri_naming_another_subject_is_not_found`
Expected: FAIL — the `someone-else` URIs are served rather than refused.

- [ ] **Step 3: Compare the parsed subject against the credential's**

In `crates/memorysafe-mcp/src/lib.rs`'s `read_resource`, replace the resolve-and-check-tenant block:

```rust
        let parsed = resources::parse_uri(&request.uri)?;

        // The URI names a scope; the credential decides which scope this
        // caller may name. Resolving the namespace through the same path the
        // tools use means a resource URI can never reach further than a tool
        // call could — but the URI also carries a tenant and a subject, and
        // neither is caller-supplied scope. Both are compared, not trusted:
        // without the subject comparison a caller could read another
        // subject's audit trail through a URI while every tool-level check
        // still held.
        let resolved = resolve_call(
            self.resolver.as_ref(),
            &context.extensions,
            Some(parsed.namespace),
        )?;
        if resolved.scope.tenant.as_str() != parsed.tenant
            || resolved.scope.subject.as_str() != parsed.subject
        {
            return Err(ErrorData::resource_not_found(
                format!("no such resource: {}", request.uri),
                None,
            ));
        }
```

`resource_not_found` rather than a permission error, deliberately: telling a caller that a subject exists but is not theirs is an enumeration oracle for the tenant's subject list.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p memorysafe-mcp --test resources`
Expected: PASS.

- [ ] **Step 5: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy -p memorysafe-mcp --all-targets -- -D warnings
git add crates/memorysafe-mcp/src/lib.rs crates/memorysafe-mcp/tests/resources.rs
git commit -m "fix(mcp): a resource URI's subject is compared against the credential, not trusted"
```

---

## Task 6: API — the same rule for REST

**Files:**
- Modify: `crates/memorysafe-api/src/scope.rs`
- Modify: `crates/memorysafe-api/src/auth.rs`
- Modify: `crates/memorysafe-api/src/lib.rs`
- Modify: `crates/memorysafe-api/src/memories.rs`, `src/ops.rs`
- Modify: `crates/memorysafe-api/tests/*.rs`

**Interfaces:**
- Consumes: `memorysafe_auth::{ApiKeyScope, ScopeResolver, NAMESPACE_HEADER}`.
- Produces:
  - `AppState { engine: Arc<Engine>, resolver: Arc<ApiKeyScope> }`
  - `ScopeParams { namespace: Option<String> }`
  - `ScopeParams::resolve(&self, resolver: &ApiKeyScope, headers: &HeaderMap) -> Result<Scope, ApiError>`

**Why `Arc<ApiKeyScope>` and not `Arc<dyn ScopeResolver>`.** The REST adapter's `/v1/whoami` and admin routes are tenant-scoped, not scope-scoped: they need an `Authenticated` without resolving a namespace at all. That is what Task 3's `ApiKeyScope::authenticate` is for, and it is not on the trait because a `FixedScope` has no credential to return. Holding the concrete type keeps both methods available. A hosted deployment that needs a different key source replaces this type; it is not on the hot path of this plan.

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-api/tests/memories.rs`:

```rust
#[tokio::test]
async fn a_write_takes_its_subject_from_the_key_and_never_from_the_body() {
    let (app, key) = support::app_with_key("user-42").await;

    // No subject in the body at all — the ordinary case, and it must work.
    let response = support::post(
        &app,
        "/v1/memories",
        &key,
        serde_json::json!({ "body": "the API is versioned", "namespace": "agent" }),
    )
    .await;
    assert_eq!(response.status(), 200, "{response:?}");

    // A subject in the body is not honoured. `deny_unknown_fields` is not set
    // on these bodies, so it is ignored rather than rejected; what matters is
    // that it does not move the scope.
    let response = support::post(
        &app,
        "/v1/memories",
        &key,
        serde_json::json!({
            "body": "written as someone else?",
            "namespace": "agent",
            "subject": "someone-else"
        }),
    )
    .await;
    assert_eq!(response.status(), 200);

    // Both writes landed in user-42's scope, so a review as user-42 sees two.
    let review = support::get(&app, "/v1/memories?namespace=agent", &key).await;
    let body: serde_json::Value = support::json(review).await;
    assert_eq!(
        body["items"].as_array().map(Vec::len),
        Some(2),
        "a body-supplied subject moved the write: {body}"
    );
}

#[tokio::test]
async fn a_read_with_no_namespace_lands_in_the_fallback_rather_than_failing() {
    // Symmetry with MCP: naming nothing is a legal call, not a 400.
    let (app, key) = support::app_with_key("user-42").await;
    let response = support::get(&app, "/v1/memories", &key).await;
    assert_eq!(response.status(), 200, "{response:?}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p memorysafe-api --test memories`
Expected: FAIL — `ScopeParams` still requires `subject`, so the first request is a 400 and `support::app_with_key` does not take a subject.

- [ ] **Step 3: Rewrite `scope.rs`**

Replace `crates/memorysafe-api/src/scope.rs`:

```rust
use crate::error::ApiError;
use http::HeaderMap;
use memorysafe_auth::{ApiKeyScope, ScopeResolver};
use memorysafe_core::Scope;
use serde::Deserialize;

/// The namespace, and only the namespace. Tenant and subject come from the
/// credential — see `memorysafe_auth::resolver`'s module documentation for
/// the contract this mirrors.
///
/// Appears as a query parameter on reads and as a flattened body field on
/// writes. Optional in both positions: a request that names no namespace
/// falls back exactly as an MCP call does, so the two adapters agree.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScopeParams {
    pub namespace: Option<String>,
}

impl ScopeParams {
    pub fn resolve(
        &self,
        resolver: &ApiKeyScope,
        headers: &HeaderMap,
    ) -> Result<Scope, ApiError> {
        Ok(resolver
            .resolve(headers, self.namespace.as_deref())?
            .scope)
    }
}
```

- [ ] **Step 4: Update `AppState` and the auth extractor**

In `crates/memorysafe-api/src/lib.rs`:

```rust
#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<Engine>,
    pub resolver: Arc<ApiKeyScope>,
}
```

replacing the `keys: Arc<ApiKeyStore>` field and its import.

In `crates/memorysafe-api/src/auth.rs`, change the extractor to go through the resolver, and add a subject accessor:

```rust
impl Auth {
    pub fn tenant(&self) -> &TenantId {
        self.0.tenant()
    }

    pub fn subject(&self) -> &memorysafe_core::SubjectId {
        self.0.subject()
    }

    pub fn actor(&self) -> Actor {
        self.0.actor()
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        Ok(Auth(state.resolver.authenticate(&parts.headers)?))
    }
}
```

Delete `strip_bearer` from this file — it lives in `memorysafe-auth` now, and having one copy is the point.

- [ ] **Step 5: Update the handlers**

In `crates/memorysafe-api/src/memories.rs` and `src/ops.rs`, every handler that currently calls `params.resolve(&auth)` or `scope::resolve(&auth, &subject, &namespace)` now needs the headers. Add `headers: HeaderMap` as an extractor argument to each such handler (axum extracts it directly) and call:

```rust
    let scope = params.resolve(&state.resolver, &headers)?;
```

For the query-parameter handlers that spelled the two fields out because `serde_urlencoded` cannot flatten, the struct now has one optional field and can be deserialized directly — delete the spelled-out `subject`/`namespace` pair and use `ScopeParams`.

Leave `/v1/whoami` and the three `/v1/admin/tenants/{tenant}/*` routes on `Auth` alone: they are tenant-scoped and resolve no namespace. Add the subject to `whoami`'s response body, since a caller can no longer choose it and now needs to be told it:

```rust
    Json(serde_json::json!({
        "tenant": auth.tenant().as_str(),
        "subject": auth.subject().as_str(),
        "key_id": auth.0.key_id(),
    }))
```

- [ ] **Step 6: Update the test support harness**

In `crates/memorysafe-api/tests/support/mod.rs`, change key minting to take a subject and build the new `AppState`:

```rust
pub async fn app_with_key(subject: &str) -> (Router, String) {
    let engine = engine().await;
    let g = memorysafe_auth::generate(
        memorysafe_core::TenantId::new("acme").unwrap(),
        memorysafe_core::SubjectId::new(subject).unwrap(),
        "test",
    )
    .unwrap();
    let secret = g.secret.clone();
    let resolver = std::sync::Arc::new(memorysafe_auth::ApiKeyScope::new(
        std::sync::Arc::new(memorysafe_auth::ApiKeyStore::new(vec![g.record])),
    ));
    (memorysafe_api::router(AppState { engine, resolver }), secret)
}
```

Then across `tests/auth.rs`, `tests/memories.rs`, `tests/ops.rs`, and `tests/admin.rs`: drop every `subject` query parameter and body field, and update `whoami` assertions to expect the new `subject` field.

- [ ] **Step 7: Run the API crate to verify it passes**

Run: `cargo test -p memorysafe-api`
Expected: PASS.

- [ ] **Step 8: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy -p memorysafe-api --all-targets -- -D warnings
git add crates/memorysafe-api/
git commit -m "feat(api): the REST surface takes its subject from the credential too"
```

---

## Task 7: CLI — mint subject-bearing keys and build the resolvers

**Files:**
- Modify: `crates/memorysafe-cli/src/cmd/keys.rs`
- Modify: `crates/memorysafe-cli/src/cmd/serve.rs`
- Modify: `crates/memorysafe-cli/tests/serve.rs`

**Interfaces:**
- Consumes: Tasks 1-6.
- Produces:
  - `msafe keys add --label <L> [--subject <S>] [--namespace <N>]`
  - `http_router(engine: Arc<Engine>, resolver: Arc<ApiKeyScope>, serve: &ServeConfig) -> Router`

- [ ] **Step 1: Write the failing test**

Add to `crates/memorysafe-cli/tests/serve.rs`:

```rust
#[test]
fn a_minted_key_records_the_subject_it_acts_as() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("msafe.toml"),
        "tenant = \"acme\"\nsubject = \"user-42\"\n",
    )
    .unwrap();

    let out = msafe(dir.path())
        .args(["keys", "add", "--label", "laptop", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let config = std::fs::read_to_string(dir.path().join("msafe.toml")).unwrap();
    assert!(
        config.contains("subject = \"user-42\""),
        "the key record did not carry a subject: {config}"
    );

    // An explicit subject overrides the configured one.
    let out = msafe(dir.path())
        .args(["keys", "add", "--label", "ci", "--subject", "build-bot", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let config = std::fs::read_to_string(dir.path().join("msafe.toml")).unwrap();
    assert!(config.contains("build-bot"), "{config}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p memorysafe-cli --test serve a_minted_key_records_the_subject_it_acts_as`
Expected: FAIL — `keys add` has no `--subject` flag and `generate` is called with two arguments.

- [ ] **Step 3: Give `keys add` a subject and a default namespace**

In `crates/memorysafe-cli/src/cmd/keys.rs`:

```rust
#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(long)]
    pub label: String,
    /// The subject this key acts as. Defaults to the configured subject.
    /// A key is bound to one subject: it cannot name another at call time.
    #[arg(long)]
    pub subject: Option<String>,
    /// The namespace this key falls back to when a request declares none.
    #[arg(long)]
    pub namespace: Option<String>,
}
```

Change `run`'s signature to take the resolved `subject: &SubjectId` alongside `tenant`, and in the `Add` arm:

```rust
            let subject = match args.subject.as_deref() {
                Some(raw) => SubjectId::new(raw)?,
                None => subject.clone(),
            };
            let mut generated = generate(tenant.clone(), subject, &args.label)?;
            if let Some(raw) = args.namespace.as_deref() {
                generated.record.default_namespace = Some(Namespace::new(raw)?);
            }
```

Add `subject` and `default_namespace` to the `AddedKey` output struct and to the human-readable `List` rendering, so an operator can see which subject a key acts as without reading the TOML:

```rust
                    println!(
                        "{}  {}  {}  {}  {}",
                        key.id, key.tenant, key.subject, state, key.label
                    );
```

- [ ] **Step 4: Build the resolvers in `serve`**

In `crates/memorysafe-cli/src/cmd/serve.rs`:

```rust
pub fn http_router(
    engine: Arc<Engine>,
    resolver: Arc<ApiKeyScope>,
    serve: &ServeConfig,
) -> Router {
    let mcp = http_service_with(
        engine.clone(),
        resolver.clone(),
        HttpTransportConfig {
            allowed_hosts: serve.allowed_hosts.clone(),
        },
    );
    api_router(AppState { engine, resolver }).nest_service(&serve.mcp_path, mcp)
}

pub async fn serve(
    engine: Arc<Engine>,
    config: &MsafeConfig,
    scope: Scope,
    args: ServeArgs,
) -> Result<()> {
    match args.transport.as_str() {
        "stdio" => {
            // The namespace defaults from the working directory; a call may
            // override it. Tenant and subject are fixed for the session.
            let resolver = Arc::new(FixedScope::new(
                scope.tenant.clone(),
                scope.subject.clone(),
                namespace_from_cwd(),
            )?);
            // Nothing is printed here: stdout is the transport.
            serve_stdio(engine, resolver).await
        }
        "http" => {
            let resolver = Arc::new(ApiKeyScope::new(Arc::new(ApiKeyStore::new(
                config.keys.clone(),
            ))));
            let bind = args.bind.as_deref().unwrap_or(&config.serve.bind);
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("binding {bind}"))?;
            let address = listener.local_addr()?;
            // stderr, so this stays usable when stdout is piped somewhere.
            eprintln!(
                "msafe listening on http://{address} (MCP at {})",
                config.serve.mcp_path
            );

            axum::serve(listener, http_router(engine, resolver, &config.serve))
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

`http_service_with` takes `Arc<dyn ScopeResolver>`; `Arc<ApiKeyScope>` coerces to it at the call site, so no turbofish is needed.

Note `FixedScope::new` returns `Result`, and `serve` already returns `anyhow::Result`, so `?` handles a reserved configured subject — the case Task 4 deleted `reject_reserved_configuration` for. Keep `slug`, `namespace_from_cwd`, and their tests exactly as they are; they are reused verbatim by Task 8.

- [ ] **Step 5: Run the CLI tests to verify they pass**

Run: `cargo test -p memorysafe-cli`
Expected: PASS.

- [ ] **Step 6: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy -p memorysafe-cli --all-targets -- -D warnings
git add crates/memorysafe-cli/
git commit -m "feat(cli): keys carry a subject, and serve builds the two resolvers"
```

---

## Task 8: CLI — `msafe mcp install` and `msafe mcp headers`

**Files:**
- Create: `crates/memorysafe-cli/src/cmd/mcp.rs`
- Modify: `crates/memorysafe-cli/src/cmd/mod.rs`
- Modify: `crates/memorysafe-cli/src/lib.rs` (or wherever the top-level `Command` enum lives)
- Create: `crates/memorysafe-cli/tests/mcp.rs`

**Interfaces:**
- Consumes: `crate::cmd::serve::namespace_from_cwd`, `memorysafe_auth::NAMESPACE_HEADER`.
- Produces: `msafe mcp install [--remote] [--portable] [--url <U>]`, `msafe mcp headers`.

- [ ] **Step 1: Write the failing tests**

Create `crates/memorysafe-cli/tests/mcp.rs`:

```rust
mod support;
use support::msafe;

#[test]
fn install_writes_a_committable_local_entry() {
    let dir = tempfile::tempdir().unwrap();
    let out = msafe(dir.path()).args(["mcp", "install"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");

    let written = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
    let entry = &parsed["mcpServers"]["memorysafe"];
    assert_eq!(entry["command"], "msafe");
    assert_eq!(entry["args"][0], "serve");
}

#[test]
fn install_remote_never_writes_a_secret() {
    // `.mcp.json` is the scope Claude Code commits to source control. A
    // credential written here is a credential in git history.
    let dir = tempfile::tempdir().unwrap();
    let out = msafe(dir.path())
        .args(["mcp", "install", "--remote"])
        .env("MEMORYSAFE_API_KEY", "msk_shouldnotappear_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let written = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    assert!(
        !written.contains("shouldnotappear"),
        "a secret reached the committed config: {written}"
    );
    assert!(written.contains("headersHelper"), "{written}");
}

#[test]
fn install_remote_portable_uses_env_expansion_rather_than_a_helper() {
    let dir = tempfile::tempdir().unwrap();
    let out = msafe(dir.path())
        .args(["mcp", "install", "--remote", "--portable"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let written = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    assert!(written.contains("${MEMORYSAFE_API_KEY}"), "{written}");
    assert!(!written.contains("headersHelper"), "{written}");
    assert!(
        written.contains("MemorySafe-Namespace") || written.contains("memorysafe-namespace"),
        "the portable form must pin a namespace: {written}"
    );
}

#[test]
fn headers_emits_the_namespace_for_the_current_directory() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("checkout-service");
    std::fs::create_dir(&project).unwrap();

    let out = msafe(&project)
        .args(["mcp", "headers"])
        .env("MEMORYSAFE_API_KEY", "msk_01ARZ3NDEKTSV4RRFFQ69G5FAV_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(parsed["memorysafe-namespace"], "checkout-service");
    assert!(
        parsed["Authorization"].as_str().unwrap().starts_with("Bearer msk_"),
        "{parsed}"
    );
}

#[test]
fn headers_without_a_credential_says_so_on_stderr_and_fails() {
    // A helper that silently emits no Authorization produces a confusing 401
    // inside the MCP client. Fail loudly at the helper instead.
    let dir = tempfile::tempdir().unwrap();
    let out = msafe(dir.path())
        .args(["mcp", "headers"])
        .env_remove("MEMORYSAFE_API_KEY")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("MEMORYSAFE_API_KEY"),
        "{out:?}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p memorysafe-cli --test mcp`
Expected: FAIL — there is no `mcp` subcommand.

- [ ] **Step 3: Write the command**

Create `crates/memorysafe-cli/src/cmd/mcp.rs`:

```rust
//! `msafe mcp` — write an MCP client entry, and supply its headers.
//!
//! The point of both subcommands is that one committed `.mcp.json` entry
//! works against a local `msafe` and a hosted MemorySafe, and that neither
//! form ever puts a credential in a file that gets committed.

use crate::cmd::serve::namespace_from_cwd;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use memorysafe_auth::NAMESPACE_HEADER;
use std::path::Path;

const CONFIG_FILE: &str = ".mcp.json";
const KEY_ENV: &str = "MEMORYSAFE_API_KEY";
const DEFAULT_URL: &str = "https://api.memorysafe.dev";

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Write an MCP client entry for this project.
    Install(InstallArgs),
    /// Emit the headers an MCP client should connect with. Intended as a
    /// `headersHelper`, re-run by the client on every connection.
    Headers,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Point at a hosted MemorySafe rather than a local `msafe` subprocess.
    #[arg(long)]
    pub remote: bool,
    /// Emit a spec-portable entry using environment expansion instead of a
    /// `headersHelper`. The namespace is fixed at install time rather than
    /// following the working directory.
    #[arg(long)]
    pub portable: bool,
    /// The hosted base URL. Only meaningful with `--remote`.
    #[arg(long)]
    pub url: Option<String>,
}

pub fn run(command: McpCommand) -> Result<()> {
    match command {
        McpCommand::Install(args) => install(args, Path::new(CONFIG_FILE)),
        McpCommand::Headers => headers(),
    }
}

fn entry(args: &InstallArgs) -> serde_json::Value {
    if !args.remote {
        return serde_json::json!({
            "command": "msafe",
            "args": ["serve", "--transport", "stdio"]
        });
    }

    let base = args.url.as_deref().unwrap_or(DEFAULT_URL);
    if args.portable {
        // The namespace is baked now, because there is no helper to compute
        // it per connection. Same rule, evaluated once.
        serde_json::json!({
            "type": "http",
            "url": format!("{base}/mcp"),
            "headers": {
                "Authorization": format!("Bearer ${{{KEY_ENV}}}"),
                NAMESPACE_HEADER: namespace_from_cwd().as_str(),
            }
        })
    } else {
        // `${VAR:-default}` so a local hosted instance is one env var away,
        // and `headersHelper` so the namespace follows the directory the
        // client is actually working in.
        serde_json::json!({
            "type": "http",
            "url": format!("${{MEMORYSAFE_URL:-{base}}}/mcp"),
            "headersHelper": "msafe mcp headers"
        })
    }
}

fn install(args: InstallArgs, path: &Path) -> Result<()> {
    // Merge rather than overwrite: a project's `.mcp.json` usually already
    // names other servers, and clobbering them would be a hostile install.
    let mut doc: serde_json::Value = if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    doc.as_object_mut()
        .context("the MCP configuration file must hold a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("`mcpServers` must be a JSON object")?
        .insert("memorysafe".to_owned(), entry(&args));

    let mut rendered = serde_json::to_string_pretty(&doc)?;
    rendered.push('\n');
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))?;

    // stderr: `install` is occasionally piped, and this is advice, not output.
    eprintln!("wrote {} ({})", path.display(), if args.remote { "hosted" } else { "local" });
    if args.remote {
        eprintln!("set {KEY_ENV} in your environment; it is never written to {CONFIG_FILE}");
    }
    Ok(())
}

fn headers() -> Result<()> {
    let Ok(secret) = std::env::var(KEY_ENV) else {
        bail!("{KEY_ENV} is not set; `msafe keys add` mints one and prints it once");
    };
    if secret.trim().is_empty() {
        bail!("{KEY_ENV} is set but empty");
    }

    let out = serde_json::json!({
        "Authorization": format!("Bearer {}", secret.trim()),
        NAMESPACE_HEADER: namespace_from_cwd().as_str(),
    });
    println!("{out}");
    Ok(())
}
```

- [ ] **Step 4: Register the subcommand**

In `crates/memorysafe-cli/src/cmd/mod.rs` add `pub mod mcp;`. In the top-level `Command` enum add:

```rust
    /// Write an MCP client entry, or emit the headers one should use.
    #[command(subcommand_negates_reqs = true)]
    Mcp {
        #[command(subcommand)]
        command: cmd::mcp::McpCommand,
    },
```

and dispatch it with `cmd::mcp::run(command)`. This command reads no configuration and constructs no engine, so wire it before the config load in whatever `main` does — `msafe mcp install` must work in an empty directory.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p memorysafe-cli --test mcp`
Expected: PASS, 5 tests.

- [ ] **Step 6: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy -p memorysafe-cli --all-targets -- -D warnings
git add crates/memorysafe-cli/src/cmd/mcp.rs crates/memorysafe-cli/src/cmd/mod.rs crates/memorysafe-cli/src/lib.rs crates/memorysafe-cli/tests/mcp.rs
git commit -m "feat(cli): msafe mcp install and headers, one committed entry for either endpoint"
```

---

## Task 9: Documentation and the end-to-end walkthrough

**Files:**
- Modify: `docs/adapters.md`
- Modify: `README.md`
- Modify: `crates/memorysafe-cli/tests/walkthrough.rs`

**Interfaces:** consumes everything above; produces no code API.

**Context.** Plan 3's Task 18 wrote `docs/adapters.md` and `walkthrough.rs` describing the rule this plan replaces — *"Over stdio the server is bound to one tenant and one subject; ... Over streamable HTTP the API key identifies the tenant and each call carries subject and namespace"* (`docs/adapters.md:87-90`) and *"Reads take `subject` and `namespace` as query parameters"* (`:163`). Both are now false.

- [ ] **Step 1: Update the walkthrough test first**

`crates/memorysafe-cli/tests/walkthrough.rs` executes the README. Change its embedded `msafe.toml` and command sequence to the new surface: `keys add` gains no flag (it inherits the configured subject), and no HTTP call passes a subject. Add one step proving the headline claim end to end:

```rust
    // The same tool call, with no scope named, against the HTTP transport.
    // Over stdio this has always worked; this asserts it now works here too,
    // which is the entire point of the scope contract.
    let response = http_post(
        &base,
        "/v1/memories",
        &key,
        serde_json::json!({ "body": "deploys are gated on the conformance suite" }),
    );
    assert_eq!(response.status(), 200, "{response:?}");
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p memorysafe-cli --test walkthrough`
Expected: FAIL until the docs and any stale flags in the test are updated together.

- [ ] **Step 3: Rewrite the scope section of `docs/adapters.md`**

Replace lines 87-90 with one rule stated once:

```markdown
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
```

Update line 163's REST paragraph to *"Reads take an optional `namespace` query parameter; writes take it in the body. Subject is never accepted from a request."* Update the resource URI paragraph to note that the URI still carries a subject as an identifier, and that it is compared against the credential rather than trusted.

- [ ] **Step 4: Update `README.md`**

Update the quickstart's `msafe.toml` and any `claude mcp add` / `.mcp.json` snippet to the Task 8 form, and add the one-line hosted setup:

```bash
msafe mcp install --remote     # writes .mcp.json; the key stays in your environment
export MEMORYSAFE_API_KEY=msk_...
```

- [ ] **Step 5: Run the walkthrough to verify it passes**

Run: `cargo test -p memorysafe-cli --test walkthrough`
Expected: PASS.

- [ ] **Step 6: Run the whole workspace**

```bash
LD_LIBRARY_PATH=$(ls -d /nix/store/*gcc-15.3.0-lib/lib | head -1) \
  cargo test --workspace --all-features --no-fail-fast 2>&1 | tee /tmp/run.log
grep -c '^running ' /tmp/run.log
grep -c '^test result:' /tmp/run.log
```

Expected: the two counts are **equal**, and every `test result:` line reads `ok`. Unequal counts mean a target failed to link, not that a test failed — re-read the Global Constraints note before concluding anything.

- [ ] **Step 7: Format, lint, and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
git add docs/adapters.md README.md crates/memorysafe-cli/tests/walkthrough.rs
git commit -m "docs: one scope rule, stated once, for every adapter"
```

---

## Task 10: Amend the spec to match what was built

**Files:**
- Modify: `docs/superpowers/specs/2026-09-07-unified-mcp-endpoint-design.md`

- [ ] **Step 1: Correct §5's trait sketch**

Replace the `ScopeResolver` code block with the signature actually built (`memorysafe-auth`, `&HeaderMap`, `AuthError`), and add a short note recording why: §9 requires `memorysafe-api` to hold a resolver, which is impossible if the trait returns `rmcp::ErrorData` without dragging `rmcp` into the REST adapter.

- [ ] **Step 2: Correct §9's timing paragraph**

The paragraph argues for landing before Plan 3's Task 18. Plan 3 merged to `master` as `00a46a0` on 2026-09-07, before this plan was written, so the argument is spent. Replace it with what is now true: the change lands as an ordinary change on `master`, and Task 9 of this plan rewrites the `docs/adapters.md` and walkthrough content Task 18 wrote — the cost the original paragraph hoped to avoid, now scheduled rather than dodged.

- [ ] **Step 3: Record the `FixedScope::new` improvement in §4**

The spec says `check_reserved` still applies to operator-configured values via `transport::reject_reserved_configuration`. That function is gone; the check moved into `FixedScope::new`, making an unchecked configuration unconstructible rather than merely rejected at startup. State it.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-09-07-unified-mcp-endpoint-design.md
git commit -m "docs(spec): match the built signature, and retire a timing argument that expired"
```

---

## Self-Review

**Spec coverage.** §4's scope rule → Tasks 1-3 (contract), 4 and 6 (adopted). §5's trait and three implementations → Tasks 2, 3; the cloud's third implementation is out of scope by design and unblocked by Task 2's trait being public. §5.1's accepted trade → recorded in `resolver.rs`'s module doc (Task 2, Step 4). §6's credential kinds → the API key half is Tasks 1 and 3; the OAuth half is Plan B. §7's namespace header → `NAMESPACE_HEADER`, Task 2. §8's client configuration → Task 8, both forms. §9's crate table → Tasks 1, 3, 4, 6, 7, 8 one-for-one; the two `memorysafe-cloud` rows are Plan B. §11's test table → the parity suite (Task 3), the schema snapshot (Task 4), raw-subject-ignored (Task 6's first test and Task 4's HTTP test), reserved-word coverage (Tasks 1-3), the resource-URI carve-out (Task 5). The two cloud rows are Plan B.

**Gap found and closed:** §11 lists "a key whose record names a reserved subject is refused at mint and at load". Mint is covered by Task 1's `a_key_may_not_be_minted_for_a_reserved_subject`. *Load* — a hand-edited `msafe.toml` carrying `subject = "_admin"` — is not, because `ApiKeyStore::new` accepts records without inspecting them.

Checked against the source rather than assumed: `memorysafe-core` **does not** reject reserved words as components, and pins that deliberately — `crates/memorysafe-core/src/ids.rs:303-308` asserts `Namespace::new(PURGED_COMPONENT).is_ok()` and `SubjectId::new(PURGED_COMPONENT).is_ok()`, with `PURGED_COMPONENT`'s doc explaining that rejecting them as *caller* input belongs to the adapters. So `Scope::new` would not have caught this, and the first draft of this plan was wrong to suggest it might.

The fix is therefore in `Authenticated::scope`, which checks **both** components (`check_reserved(Some(self.subject.as_str()), Some(namespace))`) as written in Task 1, Step 9. **Add to Task 1, Step 7** a test pinning it:

```rust
#[test]
fn a_hand_edited_record_naming_a_reserved_subject_cannot_produce_a_scope() {
    // `ApiKeyStore::new` takes records on trust — it is a constructor, not a
    // validator. A config file edited by hand can therefore carry
    // `subject = "_admin"`. It must still be unusable.
    let tenant = TenantId::new("acme").unwrap();
    let g = generate(tenant, SubjectId::new("user-42").unwrap(), "ci").unwrap();
    let mut record = g.record;
    record.subject = SubjectId::new(ADMIN_COMPONENT).unwrap();
    let store = ApiKeyStore::new(vec![record]);

    let auth = store.authenticate(&g.secret).expect("the key still authenticates");
    assert!(
        matches!(
            auth.scope("agent"),
            Err(AuthError::Reserved { component: "_admin" })
        ),
        "a reserved subject from a loaded record produced a usable scope"
    );
}
```

Note the key still *authenticates* — the credential is genuine, and pretending otherwise would report "unknown API key" for what is really a bad configuration. It simply cannot produce a scope.

**Placeholder scan.** No "TBD", "TODO", "add appropriate error handling", or "similar to Task N". Task 4 Step 8 and Task 6 Step 5 describe mechanical edits across several test files without quoting every line — they name the exact files, the exact substitution, and the exact construction to use, which is the level a reader can act on without inventing anything.

**Type consistency.** `generate(TenantId, SubjectId, &str)` is used identically in Tasks 1, 3, 6, 7. `Authenticated::scope(&str)` — one argument — in Tasks 1, 3, 6. `ScopeResolver::resolve(&HeaderMap, Option<&str>)` in Tasks 2, 3, 4, 6. `Resolved { scope, actor }` unchanged from the original. `NAMESPACE_HEADER` is the constant everywhere; the literal `"MemorySafe-Namespace"` appears only in prose and in Task 8's portable-form assertion, which accepts either casing because `HeaderMap` lookups are case-insensitive. `FixedScope::new` returns `Result` at every call site (Tasks 2, 4, 7).

---

## Execution note

Tasks 1-3 leave `memorysafe-mcp`, `memorysafe-api`, and `memorysafe-cli` **not compiling**; Tasks 4, 6, and 7 restore them. That is deliberate — the alternative is one enormous commit — but it means `cargo test --workspace` is not green until Task 7, and reviewers gating on a green workspace should gate at Tasks 7 and 9 rather than after every task. Per-crate commands (`cargo test -p memorysafe-auth`) are green throughout and are what each task's steps specify.
