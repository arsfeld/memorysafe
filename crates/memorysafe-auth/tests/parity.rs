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
                matches!(resolver.resolve(&base, Some(bad)), Err(AuthError::Scope(_))),
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
