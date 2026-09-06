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

    /// The only constructor of a `Scope` in the network adapters. The tenant is
    /// taken from the credential and never from the request, so a scope that
    /// crosses tenants is unrepresentable rather than merely rejected.
    pub fn scope(&self, subject: &str, namespace: &str) -> Result<Scope, AuthError> {
        if subject == ADMIN_COMPONENT || namespace == ADMIN_COMPONENT {
            return Err(AuthError::Reserved {
                component: ADMIN_COMPONENT,
            });
        }
        Ok(Scope::new(self.tenant.as_str(), subject, namespace)?)
    }
}

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

        assert!(matches!(
            store.authenticate(&tampered),
            Err(AuthError::Unknown)
        ));
    }

    #[test]
    fn an_unknown_id_is_rejected_with_the_same_error_as_a_wrong_secret() {
        let (store, _, _) = store_with("ci");
        let other = generate(TenantId::new("acme").unwrap(), "elsewhere").unwrap();
        assert!(matches!(
            store.authenticate(&other.secret),
            Err(AuthError::Unknown)
        ));
    }

    #[test]
    fn a_disabled_key_does_not_authenticate() {
        let tenant = TenantId::new("acme").unwrap();
        let g = generate(tenant, "revoked").unwrap();
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

        let scope = auth
            .scope("user-42", "coding-agent")
            .expect("in-tenant scope");
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
        assert!(matches!(
            auth.scope("User-42", "agent"),
            Err(AuthError::Scope(_))
        ));
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
