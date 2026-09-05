use crate::error::CoreError;
use serde::{Deserialize, Serialize};

// 240, not 255: the SQLite backend writes `<tenant>.db` plus SQLite's own
// `-wal` and `-shm` sidecars, and every one of those must fit under NAME_MAX.
const MAX_COMPONENT_BYTES: usize = 240;

/// Scope components are used as SQLite filenames and SQL parameters, so the
/// allowed alphabet is deliberately narrow: lowercase ASCII alphanumerics,
/// `-`, `_`, `.`, with a leading `.` rejected to rule out `.` and `..`.
///
/// Uppercase is rejected rather than folded. Tenant isolation is structural —
/// one database file per tenant — so on a case-insensitive filesystem (default
/// macOS APFS, default Windows NTFS, most SMB mounts) `Acme` and `acme` would
/// be two distinct tenants sharing one file. Folding to lowercase would close
/// the breach but silently merge two customers; rejecting makes the constraint
/// visible at the API boundary and fails loudly instead.
fn validate_component(field: &'static str, raw: &str) -> Result<(), CoreError> {
    if raw.is_empty() {
        return Err(CoreError::Empty { field });
    }
    if raw.len() > MAX_COMPONENT_BYTES {
        return Err(CoreError::TooLong {
            field,
            max: MAX_COMPONENT_BYTES,
        });
    }
    if raw.starts_with('.') {
        return Err(CoreError::IllegalChar { field, index: 0 });
    }
    for (index, byte) in raw.bytes().enumerate() {
        let ok = byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'-' | b'_' | b'.');
        if !ok {
            return Err(CoreError::IllegalChar { field, index });
        }
    }
    Ok(())
}

macro_rules! scope_component {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(raw: &str) -> Result<Self, CoreError> {
                validate_component($field, raw)?;
                Ok(Self(raw.to_owned()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

scope_component!(TenantId, "tenant");
scope_component!(SubjectId, "subject");
scope_component!(Namespace, "namespace");

macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(ulid::Ulid::generate().to_string())
            }
            pub fn parse(raw: &str) -> Result<Self, CoreError> {
                ulid::Ulid::from_string(raw).map_err(|_| CoreError::IllegalChar {
                    field: stringify!($name),
                    index: 0,
                })?;
                Ok(Self(raw.to_owned()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

ulid_id!(ItemId);
ulid_id!(AuditId);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Scope {
    pub tenant: TenantId,
    pub subject: SubjectId,
    pub namespace: Namespace,
}

impl Scope {
    pub fn new(tenant: &str, subject: &str, namespace: &str) -> Result<Self, CoreError> {
        Ok(Self {
            tenant: TenantId::new(tenant)?,
            subject: SubjectId::new(subject)?,
            namespace: Namespace::new(namespace)?,
        })
    }

    /// Cache-key form. Uses ASCII unit separator, which `validate_component`
    /// forbids inside components, so the encoding is unambiguous.
    pub fn key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}",
            self.tenant, self.subject, self.namespace
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_ids_are_unique_and_sortable() {
        let a = ItemId::new();
        let b = ItemId::new();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 26);
    }

    #[test]
    fn scope_components_reject_empty_and_oversize() {
        assert!(TenantId::new("").is_err());
        assert!(SubjectId::new("").is_err());
        assert!(Namespace::new("").is_err());
        let long = "x".repeat(241);
        assert!(TenantId::new(&long).is_err());
        assert!(TenantId::new(&"x".repeat(240)).is_ok());
    }

    #[test]
    fn scope_components_reject_path_and_control_characters() {
        // Tenant ids become filenames in the SQLite backend.
        assert!(TenantId::new("../escape").is_err());
        assert!(TenantId::new("a/b").is_err());
        assert!(TenantId::new("a\0b").is_err());
        assert!(TenantId::new("acme-corp_1").is_ok());
    }

    #[test]
    fn scope_key_is_stable_and_unambiguous() {
        let s = Scope::new("t1", "s1", "n1").unwrap();
        assert_eq!(s.key(), "t1\u{1f}s1\u{1f}n1");
    }

    #[test]
    fn uppercase_is_rejected_so_tenants_cannot_collide_on_case_insensitive_disks() {
        // `Acme` and `acme` would be distinct tenants sharing one `.db` file on
        // APFS or NTFS. Structural isolation depends on this.
        assert!(TenantId::new("Acme").is_err());
        assert!(TenantId::new("ACME").is_err());
        assert!(TenantId::new("acme").is_ok());
        assert!(SubjectId::new("User42").is_err());
        assert!(Namespace::new("CodingAgent").is_err());
    }

    #[test]
    fn ulid_ids_parse_back_and_reject_garbage() {
        let id = ItemId::new();
        assert_eq!(ItemId::parse(id.as_str()).unwrap(), id);
        assert!(ItemId::parse("not-a-ulid").is_err());
        assert!(ItemId::parse("").is_err());
        assert!(AuditId::parse(AuditId::new().as_str()).is_ok());
    }
}
