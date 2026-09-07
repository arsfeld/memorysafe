use memorysafe_auth::{ApiKeyStore, AuthError};
use memorysafe_core::{ADMIN_COMPONENT, Actor, ActorKind, Namespace, Scope, SubjectId, TenantId};
use rmcp::ErrorData;
use rmcp::model::Extensions;
use std::sync::Arc;

/// How a call's scope is established, per transport.
#[derive(Clone)]
pub enum ScopeSource {
    /// The server is configured with tenant and subject; the namespace defaults
    /// and may be overridden per call.
    Stdio {
        tenant: TenantId,
        subject: SubjectId,
        default_namespace: Namespace,
    },
    /// The API key identifies the tenant; the request carries subject and
    /// namespace.
    Http { keys: Arc<ApiKeyStore> },
}

// `Debug`: not in the plan text as literally written, but required to
// compile against the plan's own test — `stdio_lets_a_call_override_the_
// namespace_but_never_the_subject` calls `.expect_err(...)` on a
// `Result<Resolved, ErrorData>`, and `Result::expect_err` requires the `Ok`
// type to implement `Debug` (it is formatted into the panic message on the
// path where the result was unexpectedly `Ok`). Both fields (`Scope`,
// `Actor`) already derive `Debug` in `memorysafe-core`, so this is additive.
#[derive(Debug)]
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
            ScopeSource::Stdio {
                tenant,
                subject: configured,
                default_namespace,
            } => {
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
                    actor: Actor {
                        kind: ActorKind::Agent,
                        id: None,
                    },
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

                let subject = subject.ok_or_else(|| invalid("'subject' is required over HTTP"))?;
                let namespace =
                    namespace.ok_or_else(|| invalid("'namespace' is required over HTTP"))?;
                let scope = auth.scope(subject, namespace).map_err(auth_error)?;
                Ok(Resolved {
                    scope,
                    actor: auth.actor(),
                })
            }
        }
    }
}

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
        let r = stdio()
            .resolve(&Extensions::new(), None, None)
            .expect("resolve");
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
        let r = stdio()
            .resolve(&Extensions::new(), None, Some("notes"))
            .unwrap();
        assert_eq!(r.scope.namespace.as_str(), "notes");

        let err = stdio()
            .resolve(&Extensions::new(), Some("someone-else"), None)
            .expect_err("subject override must be refused");
        assert!(format!("{err:?}").contains("subject"), "{err:?}");

        // Naming the configured subject is not an override, so it is allowed.
        assert!(
            stdio()
                .resolve(&Extensions::new(), Some("user-42"), None)
                .is_ok()
        );
    }

    #[test]
    fn http_takes_the_tenant_from_the_key_and_the_rest_from_the_call() {
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        // Capture what the test needs before the record moves into the store.
        let key_id = g.record.id.clone();
        let secret = g.secret.clone();
        let source = ScopeSource::Http {
            keys: Arc::new(ApiKeyStore::new(vec![g.record])),
        };
        let ext = parts(Some(&format!("Bearer {secret}")));

        let r = source
            .resolve(&ext, Some("user-42"), Some("agent"))
            .expect("resolve");
        assert_eq!(r.scope.tenant.as_str(), "acme");
        assert_eq!(r.scope.subject.as_str(), "user-42");
        assert_eq!(r.actor.kind, ActorKind::ApiKey);
        assert_eq!(r.actor.id.as_deref(), Some(key_id.as_str()));
    }

    #[test]
    fn http_without_a_credential_resolves_nothing() {
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        let source = ScopeSource::Http {
            keys: Arc::new(ApiKeyStore::new(vec![g.record])),
        };

        assert!(
            source
                .resolve(&parts(None), Some("user-42"), Some("agent"))
                .is_err()
        );
        assert!(
            source
                .resolve(&Extensions::new(), Some("user-42"), Some("agent"))
                .is_err(),
            "no HTTP parts at all must fail closed, not fall back to stdio behaviour"
        );
        assert!(
            source.resolve(&parts(Some("Bearer msk_00000000000000000000000000_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")), Some("user-42"), Some("agent")).is_err()
        );
        assert!(
            source
                .resolve(&parts(Some(&g.secret)), Some("user-42"), Some("agent"))
                .is_err(),
            "a bare key without the Bearer scheme must be refused"
        );
    }

    #[test]
    fn http_requires_both_subject_and_namespace() {
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        let source = ScopeSource::Http {
            keys: Arc::new(ApiKeyStore::new(vec![g.record])),
        };
        let ext = parts(Some(&format!("Bearer {}", g.secret)));

        assert!(source.resolve(&ext, None, Some("agent")).is_err());
        assert!(source.resolve(&ext, Some("user-42"), None).is_err());
    }

    #[test]
    fn the_reserved_admin_scope_is_unreachable_from_either_transport() {
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        let http = ScopeSource::Http {
            keys: Arc::new(ApiKeyStore::new(vec![g.record])),
        };
        let ext = parts(Some(&format!("Bearer {}", g.secret)));
        assert!(http.resolve(&ext, Some("_admin"), Some("_admin")).is_err());
        assert!(
            stdio()
                .resolve(&Extensions::new(), None, Some("_admin"))
                .is_err()
        );
    }
}
