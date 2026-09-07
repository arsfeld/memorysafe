//! The two real transports: stdio, for a single local client, and streamable
//! HTTP, for many concurrent tenants behind a bearer token.

use crate::{MemorySafeServer, ScopeSource};
use memorysafe_auth::check_reserved;
use memorysafe_engine::Engine;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService, stdio};
use std::sync::Arc;

/// `ScopeSource::Stdio`'s `subject` and `default_namespace` are server
/// configuration, not caller input — `ScopeSource::resolve`'s
/// `check_reserved` call only ever sees what a *caller* passes as an
/// override, so a deployment that configures either field to `_admin` or
/// `_purged` sails through it untouched (a call that never overrides
/// anything resolves with `check_reserved(None, None)`, which is vacuously
/// fine). That is operator error, not a caller attack, but this is the
/// module that builds the stdio entry point, so it is the place to catch it
/// — once, at startup, rather than serving a session that writes into a
/// reserved scope on every call. `ScopeSource::Http` carries no
/// operator-configured subject or namespace (both arrive per request and are
/// already checked there), so this is a no-op for it.
fn reject_reserved_configuration(source: &ScopeSource) -> anyhow::Result<()> {
    if let ScopeSource::Stdio {
        subject,
        default_namespace,
        ..
    } = source
    {
        check_reserved(Some(subject.as_str()), Some(default_namespace.as_str()))?;
    }
    Ok(())
}

/// Serve one client over stdin/stdout and return when it disconnects.
///
/// Nothing may be written to stdout except MCP frames — stdout *is* the
/// transport. Logging goes to stderr; the CLI configures that before calling
/// this.
pub async fn serve_stdio(engine: Arc<Engine>, source: ScopeSource) -> anyhow::Result<()> {
    reject_reserved_configuration(&source)?;
    let running = MemorySafeServer::new(engine, source).serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// A tower service ready to be mounted, typically at `/mcp`.
///
/// With `legacy_session_mode(false)`, `StreamableHttpServerConfig`'s own doc
/// says sessions are removed under protocol `2026-07-28` and every request
/// negotiating it "is always served statelessly regardless of this setting"
/// — confirmed against `rmcp-3.2.0`'s own request-handling code, which calls
/// this factory again for each stateless request it serves directly. So the
/// factory may run once per *request*, not once per session. Either way the
/// cost is the same: it only clones an `Arc<Engine>` and a small
/// `ScopeSource`, since the engine is the thing with state and it is
/// `Arc`-shared deliberately.
pub fn http_service(
    engine: Arc<Engine>,
    source: ScopeSource,
) -> StreamableHttpService<MemorySafeServer, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(MemorySafeServer::new(engine.clone(), source.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::{ADMIN_COMPONENT, Namespace, PURGED_COMPONENT, SubjectId, TenantId};

    fn stdio_source(subject: &str, default_namespace: &str) -> ScopeSource {
        ScopeSource::Stdio {
            tenant: TenantId::new("acme").unwrap(),
            subject: SubjectId::new(subject).unwrap(),
            default_namespace: Namespace::new(default_namespace).unwrap(),
        }
    }

    // `serve_stdio` itself — proving it actually calls this check, not just
    // that the check is correct in isolation — is exercised in
    // `tests/stdio_transport.rs`, which has a real `Engine` on hand via
    // `support::engine()` rather than duplicating that construction here.

    #[test]
    fn a_reserved_subject_is_refused() {
        let err = reject_reserved_configuration(&stdio_source(ADMIN_COMPONENT, "agent"))
            .expect_err("a reserved subject must be refused");
        assert!(format!("{err:?}").contains(ADMIN_COMPONENT), "{err:?}");
    }

    #[test]
    fn a_reserved_default_namespace_is_refused() {
        let err = reject_reserved_configuration(&stdio_source("user-42", PURGED_COMPONENT))
            .expect_err("a reserved default namespace must be refused");
        assert!(format!("{err:?}").contains(PURGED_COMPONENT), "{err:?}");
    }

    #[test]
    fn an_ordinary_stdio_configuration_passes_the_check() {
        assert!(reject_reserved_configuration(&stdio_source("user-42", "agent")).is_ok());
    }

    #[test]
    fn an_http_source_has_nothing_for_this_check_to_reject() {
        let keys = Arc::new(memorysafe_auth::ApiKeyStore::new(vec![]));
        assert!(reject_reserved_configuration(&ScopeSource::Http { keys }).is_ok());
    }
}
