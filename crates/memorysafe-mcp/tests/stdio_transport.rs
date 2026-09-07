mod support;

use memorysafe_core::{ADMIN_COMPONENT, Namespace, PURGED_COMPONENT, SubjectId, TenantId};
use memorysafe_mcp::{ScopeSource, serve_stdio};
use support::engine;

/// A server operator, not a caller, controls `ScopeSource::Stdio`'s `subject`
/// and `default_namespace` — `ScopeSource::resolve`'s reserved-word check
/// only ever sees a caller's per-call override, so a deployment configured
/// with either field set to a reserved word sails through it untouched. This
/// pins `serve_stdio` itself catching that at startup, before the transport
/// ever runs — not just the private helper it delegates to.
///
/// Safe to call `serve_stdio` directly here without a real client: the check
/// runs before `stdio()` is touched at all, so a rejected configuration
/// returns immediately rather than blocking on real stdin.
#[tokio::test]
async fn a_reserved_subject_is_refused_before_the_stdio_transport_starts() {
    let (eng, _dir) = engine();
    let source = ScopeSource::Stdio {
        tenant: TenantId::new("acme").unwrap(),
        subject: SubjectId::new(ADMIN_COMPONENT).unwrap(),
        default_namespace: Namespace::new("agent").unwrap(),
    };
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), serve_stdio(eng, source))
        .await
        .expect("serve_stdio must fail fast, not hang, on a rejected configuration")
        .expect_err("a reserved subject must be refused before the transport starts");
    assert!(format!("{err:?}").contains(ADMIN_COMPONENT), "{err:?}");
}

#[tokio::test]
async fn a_reserved_default_namespace_is_refused_before_the_stdio_transport_starts() {
    let (eng, _dir) = engine();
    let source = ScopeSource::Stdio {
        tenant: TenantId::new("acme").unwrap(),
        subject: SubjectId::new("user-42").unwrap(),
        default_namespace: Namespace::new(PURGED_COMPONENT).unwrap(),
    };
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), serve_stdio(eng, source))
        .await
        .expect("serve_stdio must fail fast, not hang, on a rejected configuration")
        .expect_err("a reserved default namespace must be refused before the transport starts");
    assert!(format!("{err:?}").contains(PURGED_COMPONENT), "{err:?}");
}
