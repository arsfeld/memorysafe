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
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<crate::Authenticated, AuthError> {
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

    fn resolve(&self, headers: &HeaderMap, namespace: Option<&str>) -> Result<Resolved, AuthError> {
        let auth = self.authenticate(headers)?;
        let namespace = pick_namespace(namespace, headers, auth.default_namespace())?;
        Ok(Resolved {
            scope: auth.scope(namespace.as_str())?,
            actor: auth.actor(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiKeyStore, generate};
    use http::HeaderMap;
    use memorysafe_core::{
        ADMIN_COMPONENT, ActorKind, Namespace, PURGED_COMPONENT, SubjectId, TenantId,
    };
    use std::sync::Arc;

    fn fixed() -> FixedScope {
        FixedScope::new(
            TenantId::new("acme").unwrap(),
            SubjectId::new("user-42").unwrap(),
            Namespace::new("coding-agent").unwrap(),
        )
        .expect("an ordinary configuration")
    }

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
        let r = source
            .resolve(&bearer(&secret), Some("agent"))
            .expect("resolve");

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
            source
                .resolve(&headers, None)
                .unwrap()
                .scope
                .namespace
                .as_str(),
            "checkout-service"
        );
        assert_eq!(
            source
                .resolve(&headers, Some("notes"))
                .unwrap()
                .scope
                .namespace
                .as_str(),
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
            source
                .resolve(&bearer(&secret), None)
                .unwrap()
                .scope
                .namespace
                .as_str(),
            "from-key"
        );

        let mut headers = bearer(&secret);
        headers.insert(NAMESPACE_HEADER, "from-header".parse().unwrap());
        assert_eq!(
            source
                .resolve(&headers, None)
                .unwrap()
                .scope
                .namespace
                .as_str(),
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

        let unknown =
            bearer("msk_00000000000000000000000000_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
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
