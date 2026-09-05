use crate::error::CoreError;
use serde::{Deserialize, Serialize};

const MAX_COMPONENT_BYTES: usize = 256;

/// Scope components are used as SQLite filenames and SQL parameters, so the
/// allowed alphabet is deliberately narrow: ASCII alphanumerics, `-`, `_`, `.`,
/// with a leading `.` rejected to rule out `.` and `..`.
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
        let ok = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
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
        let long = "x".repeat(257);
        assert!(TenantId::new(&long).is_err());
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
}
