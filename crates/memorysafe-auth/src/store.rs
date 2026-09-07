use crate::AuthError;
use crate::key::{ApiKeyRecord, hash_presented, parse_presented};
use memorysafe_core::{
    ADMIN_COMPONENT, Actor, ActorKind, Namespace, PURGED_COMPONENT, Scope, SubjectId, TenantId,
};
use std::collections::HashMap;
use subtle::ConstantTimeEq;

/// Subject/namespace values a caller may never supply. Both are legal
/// components by `memorysafe-core`'s validation (that crate cannot enforce
/// caller-input rules — see `PURGED_COMPONENT`'s doc), so guarding against
/// them is this adapter's job.
///
/// A slice, not a chain of `||`s: the failure mode this guards against is a
/// third reserved word landing in `memorysafe-core` and someone updating this
/// adapter's rejection logic without noticing there is now a second `||` to
/// add too — which is exactly how `PURGED_COMPONENT` was missed here the
/// first time. Adding a word to this slice is the only step required; the
/// check below iterates it and cannot itself go stale.
const RESERVED_COMPONENTS: &[&str] = &[ADMIN_COMPONENT, PURGED_COMPONENT];

/// Reject a caller-supplied scope component that names a reserved word.
///
/// Exposed (not just used internally by `Authenticated::scope`) because not
/// every adapter path has an `Authenticated` to go through: the MCP stdio
/// transport is configured with its tenant and subject rather than
/// authenticating a key, and must still refuse the same words
/// `Authenticated::scope` refuses — otherwise the two transports would
/// disagree about which scopes are reachable, and `_purged` in particular
/// would go unguarded on the transport that never authenticates a key.
///
/// Takes `Option<&str>` rather than `&str` because a caller may not always
/// have both components in hand yet (the MCP stdio path checks the prelude
/// before the namespace default is resolved); `None` never matches a
/// reserved word.
pub fn check_reserved(subject: Option<&str>, namespace: Option<&str>) -> Result<(), AuthError> {
    if let Some(&component) = RESERVED_COMPONENTS
        .iter()
        .find(|&&reserved| subject == Some(reserved) || namespace == Some(reserved))
    {
        return Err(AuthError::Reserved { component });
    }
    Ok(())
}

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
        let matches: bool =
            expected.len() == actual.len() && bool::from(expected.ct_eq(actual.as_bytes()));
        if !matches {
            return Err(AuthError::Unknown);
        }
        if record.disabled {
            return Err(AuthError::Disabled);
        }

        Ok(Authenticated {
            tenant: record.tenant.clone(),
            subject: record.subject.clone(),
            default_namespace: record.default_namespace.clone(),
            key_id: record.id.clone(),
        })
    }
}

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
    ///
    /// The namespace is checked because it is caller input. The *subject* is
    /// checked because `ApiKeyStore::new` takes records on trust, so a
    /// hand-edited record carrying a reserved subject reaches here.
    pub fn scope(&self, namespace: &str) -> Result<Scope, AuthError> {
        check_reserved(Some(self.subject.as_str()), Some(namespace))?;
        Ok(Scope::new(
            self.tenant.as_str(),
            self.subject.as_str(),
            namespace,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::generate;
    use memorysafe_core::{ActorKind, SubjectId, TenantId};

    fn store_with(label: &str) -> (ApiKeyStore, String, TenantId) {
        let tenant = TenantId::new("acme").unwrap();
        let subject = SubjectId::new("user-42").unwrap();
        let g = generate(tenant.clone(), subject, label).unwrap();
        (ApiKeyStore::new(vec![g.record]), g.secret, tenant)
    }

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
            Err(AuthError::Reserved {
                component: "_admin"
            })
        ));
        assert!(matches!(
            auth.scope(PURGED_COMPONENT),
            Err(AuthError::Reserved {
                component: "_purged"
            })
        ));
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

        assert!(matches!(
            store.authenticate(&tampered),
            Err(AuthError::Unknown)
        ));
    }

    #[test]
    fn an_unknown_id_is_rejected_with_the_same_error_as_a_wrong_secret() {
        let (store, _, _) = store_with("ci");
        let other = generate(
            TenantId::new("acme").unwrap(),
            SubjectId::new("user-42").unwrap(),
            "elsewhere",
        )
        .unwrap();
        assert!(matches!(
            store.authenticate(&other.secret),
            Err(AuthError::Unknown)
        ));
    }

    #[test]
    fn a_disabled_key_does_not_authenticate() {
        let tenant = TenantId::new("acme").unwrap();
        let g = generate(tenant, SubjectId::new("user-42").unwrap(), "revoked").unwrap();
        let mut record = g.record;
        record.disabled = true;
        let store = ApiKeyStore::new(vec![record]);
        assert!(matches!(
            store.authenticate(&g.secret),
            Err(AuthError::Disabled)
        ));
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
        assert!(
            auth.authorize_tenant(&TenantId::new("acme").unwrap())
                .is_ok()
        );
    }

    #[test]
    fn a_key_builds_scopes_only_inside_its_own_tenant() {
        let (store, secret, _) = store_with("ci");
        let auth = store.authenticate(&secret).unwrap();

        let scope = auth.scope("coding-agent").expect("in-tenant scope");
        assert_eq!(scope.tenant.as_str(), "acme");
        assert_eq!(scope.subject.as_str(), "user-42");
    }

    #[test]
    fn the_reserved_component_is_refused_as_a_namespace() {
        // Task 2 writes tenant-level audit rows under `_admin`. A caller that
        // could name that scope could read another tenant's policy history —
        // or forge rows that look like the engine wrote them.
        //
        // Pin the returned `component` to the literal, not just the variant:
        // `Err(AuthError::Reserved { .. })` alone is satisfied by a `scope`
        // that always reports "_admin" regardless of which reserved word
        // actually matched -- a caller rejected for "_purged" would then be
        // told "'_admin' is reserved". That degree of freedom does not exist
        // in the old single-word check; the `RESERVED_COMPONENTS` slice
        // introduced it, so the test has to close it.
        let (store, secret, _) = store_with("ci");
        let auth = store.authenticate(&secret).unwrap();

        // The subject position no longer exists: key minting rejects it in
        // `a_key_may_not_be_minted_for_a_reserved_subject` in key.rs.
        assert!(matches!(
            auth.scope(memorysafe_core::ADMIN_COMPONENT),
            Err(AuthError::Reserved {
                component: "_admin"
            })
        ));
    }

    #[test]
    fn purged_is_refused_as_a_namespace() {
        // `_purged` is where the engine files a purged subject's `SubjectPurged`
        // residue when the subject owned no items (memorysafe-core's
        // `PURGED_COMPONENT` doc). A caller who names an ordinary subject or
        // namespace `_purged` puts its own audit rows alongside purge records.
        // Hardcode the literal here (not by referencing the production
        // constant list) so this test still fails if the fix's reserved-word
        // slice ever drops `_purged`.
        let (store, secret, _) = store_with("ci");
        let auth = store.authenticate(&secret).unwrap();

        // Pin the returned `component` to "_purged", not just the variant --
        // see the comment on `the_reserved_component_is_refused_as_a_namespace`
        // for why `Err(AuthError::Reserved { .. })` alone is not enough.
        // The subject position no longer exists: key minting rejects it in
        // `a_key_may_not_be_minted_for_a_reserved_subject` in key.rs.
        assert!(
            matches!(
                auth.scope("_purged"),
                Err(AuthError::Reserved {
                    component: "_purged"
                })
            ),
            "'_purged' as namespace was accepted, or reported the wrong component"
        );
    }

    #[test]
    fn check_reserved_treats_a_missing_component_as_never_reserved() {
        // The only caller of `check_reserved` that can pass `None` at all is
        // MCP's stdio `resolve`, checking its prelude before a namespace
        // default is resolved. `Authenticated::scope` always passes
        // `Some(_)` for both, so this path is untested by any existing
        // `Authenticated::scope` test.
        assert!(check_reserved(None, None).is_ok());
        assert!(check_reserved(Some("user-42"), None).is_ok());
        assert!(check_reserved(None, Some("agent")).is_ok());
        assert!(matches!(
            check_reserved(Some(ADMIN_COMPONENT), None),
            Err(AuthError::Reserved {
                component: "_admin"
            })
        ));
        assert!(matches!(
            check_reserved(None, Some(PURGED_COMPONENT)),
            Err(AuthError::Reserved {
                component: "_purged"
            })
        ));
    }

    #[test]
    fn an_invalid_component_surfaces_as_a_scope_error_not_a_panic() {
        let (store, secret, _) = store_with("ci");
        let auth = store.authenticate(&secret).unwrap();
        assert!(matches!(auth.scope("Agent-Caps"), Err(AuthError::Scope(_))));
        assert!(matches!(auth.scope(""), Err(AuthError::Scope(_))));
    }

    #[test]
    fn only_a_missing_or_malformed_credential_is_unauthenticated() {
        // 401 says "identify yourself"; 403 says "you did, and no". Getting this
        // backwards makes a client retry forever with a key that will never work.
        assert!(AuthError::Missing.is_unauthenticated());
        assert!(AuthError::Malformed.is_unauthenticated());
        assert!(AuthError::Unknown.is_unauthenticated());
        assert!(!AuthError::Disabled.is_unauthenticated());
        assert!(
            !AuthError::Reserved {
                component: "_admin"
            }
            .is_unauthenticated()
        );
        assert!(
            !AuthError::WrongTenant {
                authorized: "acme".into(),
                requested: "globex".into()
            }
            .is_unauthenticated()
        );
    }
}
