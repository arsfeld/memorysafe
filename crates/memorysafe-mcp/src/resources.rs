use memorysafe_core::Scope;
use rmcp::ErrorData;

pub const SCHEME: &str = "memorysafe://";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Audit,
    Stats,
}

impl ResourceKind {
    pub const ALL: [ResourceKind; 2] = [ResourceKind::Audit, ResourceKind::Stats];

    pub fn as_str(self) -> &'static str {
        match self {
            ResourceKind::Audit => "audit",
            ResourceKind::Stats => "stats",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ResourceKind::Audit => {
                "Every governance decision recorded for this scope: what was admitted, rejected, \
                 merged, forgotten, and recalled, and why. Ids and content digests only — never \
                 memory bodies."
            }
            ResourceKind::Stats => {
                "Capacity and corpus shape for this scope: budget, bytes and items used, and the \
                 statistics the policy calibrates against."
            }
        }
    }
}

pub struct ParsedUri<'a> {
    pub tenant: &'a str,
    pub subject: &'a str,
    pub namespace: &'a str,
    pub kind: ResourceKind,
}

pub fn resource_uri(scope: &Scope, kind: ResourceKind) -> String {
    format!(
        "{SCHEME}{}/{}/{}/{}",
        scope.tenant,
        scope.subject,
        scope.namespace,
        kind.as_str()
    )
}

pub fn uri_template(kind: ResourceKind) -> String {
    format!(
        "{SCHEME}{{tenant}}/{{subject}}/{{namespace}}/{}",
        kind.as_str()
    )
}

/// Strict: exactly four non-empty segments after the scheme, and a known kind.
/// A lenient parser here would turn a typo into a silently different scope.
pub fn parse_uri(uri: &str) -> Result<ParsedUri<'_>, ErrorData> {
    let not_found = || ErrorData::resource_not_found(format!("no such resource: {uri}"), None);

    let rest = uri.strip_prefix(SCHEME).ok_or_else(not_found)?;
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.len() != 4 || segments.iter().any(|s| s.is_empty()) {
        return Err(not_found());
    }
    let kind = match segments[3] {
        "audit" => ResourceKind::Audit,
        "stats" => ResourceKind::Stats,
        _ => return Err(not_found()),
    };
    Ok(ParsedUri {
        tenant: segments[0],
        subject: segments[1],
        namespace: segments[2],
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_uri_parses_into_a_scope_and_a_kind() {
        let parsed = parse_uri("memorysafe://acme/user-42/coding-agent/audit").expect("parse");
        assert_eq!(parsed.tenant, "acme");
        assert_eq!(parsed.subject, "user-42");
        assert_eq!(parsed.namespace, "coding-agent");
        assert_eq!(parsed.kind, ResourceKind::Audit);

        let stats = parse_uri("memorysafe://acme/user-42/coding-agent/stats").expect("parse");
        assert_eq!(stats.kind, ResourceKind::Stats);
    }

    #[test]
    fn a_malformed_uri_is_a_resource_not_found_not_a_panic() {
        for bad in [
            "",
            "memorysafe://",
            "https://acme/user-42/coding-agent/audit",
            "memorysafe://acme/user-42/audit",
            "memorysafe://acme/user-42/coding-agent/secrets",
            "memorysafe://acme/user-42/coding-agent/audit/extra",
            "memorysafe://acme//coding-agent/audit",
        ] {
            assert!(parse_uri(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn the_uri_for_a_scope_round_trips_through_the_parser() {
        let scope = memorysafe_core::Scope::new("acme", "user-42", "agent").unwrap();
        for kind in [ResourceKind::Audit, ResourceKind::Stats] {
            let uri = resource_uri(&scope, kind);
            let parsed = parse_uri(&uri).expect("round trip");
            assert_eq!(parsed.tenant, "acme");
            assert_eq!(parsed.kind, kind);
        }
    }
}
