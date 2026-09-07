//! How a call's scope is established, once, for every adapter.
//!
//! The rule this module enforces is the whole point of it: **tenant and
//! subject come from the credential; the namespace is the only component a
//! caller supplies.** That is why `resolve` has no `subject` parameter — the
//! rule is a signature, not a convention a reviewer has to remember.

use crate::{AuthError, check_reserved};
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
    fn resolve(&self, headers: &HeaderMap, namespace: Option<&str>) -> Result<Resolved, AuthError>;
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
    if let Some(namespace) = credential_default {
        return Ok(namespace.clone());
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

    fn resolve(&self, headers: &HeaderMap, namespace: Option<&str>) -> Result<Resolved, AuthError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;
    use memorysafe_core::{
        ADMIN_COMPONENT, ActorKind, Namespace, PURGED_COMPONENT, SubjectId, TenantId,
    };

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
        let r = fixed().resolve(&HeaderMap::new(), None).expect("resolve");
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
        assert_eq!(
            fixed().default_scope(),
            Some(fixed().resolve(&HeaderMap::new(), None).unwrap().scope)
        );
    }

    #[test]
    fn a_reserved_namespace_is_refused_whether_configured_or_named() {
        for word in [ADMIN_COMPONENT, PURGED_COMPONENT] {
            assert!(
                matches!(
                    FixedScope::new(
                        TenantId::new("acme").unwrap(),
                        SubjectId::new("user-42").unwrap(),
                        Namespace::new(word).unwrap()
                    ),
                    Err(AuthError::Reserved { .. })
                ),
                "'{word}' was accepted as a configured default namespace"
            );
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
                        Namespace::new("agent").unwrap()
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
