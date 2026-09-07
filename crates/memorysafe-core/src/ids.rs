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

/// Reserved subject and namespace for tenant-level records. Legal as a
/// component, so reserving it is a rule the adapters enforce, not something
/// validation gives for free.
pub const ADMIN_COMPONENT: &str = "_admin";

// **Signature change here is not local.** `scope_component!` generates
// `new(&str) -> Result<Self, CoreError>` and `as_str(&self) -> &str` for
// `TenantId`, `SubjectId` and `Namespace`. Those signatures are inside a macro
// body, so an external arity checker cannot read them off the source; it
// resolves them from a hand-maintained table keyed by macro name. `TenantId::new`
// alone has 66 call sites across the plan documents, so changing the arity or
// the return type here invalidates all 66 at once and the table is the only
// thing standing between that and a clean report. Change this macro's generated
// signatures only together with that table.
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

/// The reserved scope component naming a purged subject's residue.
///
/// A subject spans namespaces but an `AuditRecord` carries exactly one
/// `Scope`, so the `SubjectPurged` row has to be filed under some namespace.
/// When the subject owned no items there is no namespace to file it under,
/// and this is the name used instead. A leading underscore is deliberate and
/// legal: `validate_component` rejects a leading `.` (to rule out `.` and
/// `..` as filenames) but permits `_`, so this parses as a `Namespace` — and
/// it must, or the fallback could not be constructed at all.
///
/// **This constant does not enforce the reservation, and core cannot.**
/// `Namespace::new(PURGED_COMPONENT)` succeeds, exactly as
/// `Namespace::new(ADMIN_COMPONENT)` will. Rejecting the name as *caller*
/// input is an authorisation decision, and "caller" is a concept that does
/// not exist below the adapters: everything reaching this engine in Plan 1 is
/// trusted in-process code.
///
/// **What enforces it, named rather than gestured at:** Plan 3's adapter
/// deliverable — `docs/superpowers/plans/2026-09-05-adapters-and-shadow.md`,
/// Task 1 ("Core — the reserved admin scope; `memorysafe-auth`"), whose
/// Global Constraints already state that a caller-supplied **subject or
/// namespace** equal to `_admin` is rejected before the scope reaches the
/// engine. `_purged` belongs in that same check, on the same two component
/// kinds. A reader can go to that task and see whether it covers `_purged`
/// alongside `_admin`; a deferral that named no referent could never be found
/// unfulfilled.
///
/// **Why deferring is acceptable here is not the reason it is acceptable for
/// `_admin`**, and the difference matters enough to write down. `_admin`
/// guards an *authorisation* risk that only exists at the adapter, so the
/// adapter is the only meaningful place to check it. `_purged` guards a
/// *collision* risk that is live in Plan 1: a legitimate, trusted caller can
/// innocently name a namespace `_purged` today, after which their ordinary
/// audit rows sit alongside purge records. That is tolerable only because the
/// collision is recoverable — a `SubjectPurged` row and an `Admitted` row in
/// a namespace literally called `_purged` remain distinguishable by
/// `AuditRecord::event`, so a compliance query is more awkward but never
/// wrong. A queryability wart, not a correctness failure. If that ever stops
/// being true, the deferral stops being licensed.
///
/// The `SubjectPurged` fallback that consumes this is the engine's
/// `purge_scope` helper (Plan 1's engine purge task).
pub const PURGED_COMPONENT: &str = "_purged";

// **Signature change here is not local**, for the same reason as
// `scope_component!` above: `ulid_id!` generates `new() -> Self`,
// `parse(&str) -> Result<Self, CoreError>` and `as_str(&self) -> &str` for
// `ItemId` and `AuditId`, all inside a macro body an external arity checker
// cannot read. It resolves them from a hand-maintained table keyed by macro
// name. Change these generated signatures only together with that table.
macro_rules! ulid_id {
    ($name:ident) => {
        /// A ULID: a 48-bit millisecond timestamp followed by 80 bits of
        /// randomness, encoded as a canonical 26-character Crockford Base32
        /// string. Every ordering contract this crate documents on `ItemId`
        /// and `AuditId` — `Backend::list`'s, `retrieve_candidates`'s,
        /// `neighbours`'s, `AuditFilter::after`'s — leans on one property of
        /// that encoding: lexicographic (byte) order of the string equals
        /// numeric order of the 128-bit value, which equals creation order
        /// up to the timestamp's millisecond resolution. The derived `Ord`
        /// below is not an arbitrary choice of comparison; it is that
        /// property.
        ///
        /// This holds only because every value is the same length:
        /// Crockford Base32's digits are ASCII-ordered the same as their
        /// numeric value (`0`-`9` sort before `A`-`Z`), so byte comparison
        /// of two equal-length encodings agrees with numeric comparison —
        /// but would not if the lengths differed. `parse` enforces the
        /// canonical 26-character form via `ulid::Ulid::from_string`, so
        /// every value built through this crate's own API satisfies it;
        /// `Deserialize` does not route through `parse` (the inner field is
        /// a plain `String`), so a value arriving from outside this process
        /// is not guaranteed to.
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

    /// The scope tenant-level audit records are written under. Nothing a caller
    /// can name, because every adapter refuses `ADMIN_COMPONENT` in a
    /// caller-supplied subject or namespace.
    pub fn admin(tenant: &TenantId) -> Scope {
        Scope {
            tenant: tenant.clone(),
            subject: SubjectId::new(ADMIN_COMPONENT).expect("ADMIN_COMPONENT is a valid component"),
            namespace: Namespace::new(ADMIN_COMPONENT)
                .expect("ADMIN_COMPONENT is a valid component"),
        }
    }

    /// Both halves, not either: a scope reserved in one position only is an
    /// ordinary scope that happens to share a name.
    pub fn is_admin(&self) -> bool {
        self.subject.as_str() == ADMIN_COMPONENT && self.namespace.as_str() == ADMIN_COMPONENT
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

    /// `PURGED_COMPONENT` is a stored word, not an internal one: it is written
    /// into the `scope.namespace` of every `SubjectPurged` audit row for a
    /// subject that owned no items, and those rows outlive the subject. A
    /// compliance query that looks for the purge record of an erased subject
    /// searches for this literal, so changing it orphans every row already
    /// written under the old spelling — the data cannot be migrated, because
    /// the subject it belonged to is gone by construction.
    ///
    /// **Vacuous if** rewritten to `assert_eq!(PURGED_COMPONENT, PURGED_COMPONENT)`
    /// or to any comparison against the constant itself; the literal on the
    /// right is the whole test.
    #[test]
    fn the_purged_component_literal_is_frozen_and_is_a_legal_component() {
        assert_eq!(
            PURGED_COMPONENT, "_purged",
            "PURGED_COMPONENT is written into stored audit rows; changing its \
             spelling orphans every row already filed under the old one"
        );
        // Legal in core by construction — the reservation is enforced above
        // this crate (see the constant's doc comment), so `Namespace::new`
        // must accept it here.
        assert!(
            Namespace::new(PURGED_COMPONENT).is_ok(),
            "a leading underscore must stay legal, or the fallback namespace \
             cannot be built at all"
        );
        assert!(
            SubjectId::new(PURGED_COMPONENT).is_ok(),
            "core does not reject the reserved word; an adapter does"
        );
    }

    /// `ADMIN_COMPONENT` is a stored word too, not just an internal check
    /// value: Task 2 writes every tenant-level audit row under a `Scope`
    /// whose subject and namespace are both this literal (see
    /// `the_admin_scope_is_recognisable_and_is_not_a_normal_scope` below),
    /// and adapters compare caller input against this same constant to
    /// reject it (`memorysafe-auth`'s `RESERVED_COMPONENTS`). Nothing else in
    /// this crate pins the literal `"_admin"` — it appears exactly once, at
    /// the const definition — so changing the constant's value would compile
    /// clean and pass every other test here, while silently orphaning every
    /// already-written `_admin` audit row and un-reserving the string
    /// `"_admin"` itself in every adapter that checks against the constant.
    ///
    /// **Vacuous if** rewritten to `assert_eq!(ADMIN_COMPONENT, ADMIN_COMPONENT)`
    /// or to any comparison against the constant itself; the literal on the
    /// right is the whole test.
    #[test]
    fn the_admin_component_literal_is_frozen_and_is_a_legal_component() {
        assert_eq!(
            ADMIN_COMPONENT, "_admin",
            "ADMIN_COMPONENT is written into stored audit rows and compared \
             against by name in adapters; changing its spelling orphans every \
             row already filed under the old one and silently un-reserves it"
        );
        assert!(
            Namespace::new(ADMIN_COMPONENT).is_ok(),
            "a leading underscore must stay legal, or Scope::admin could not \
             be built at all"
        );
        assert!(
            SubjectId::new(ADMIN_COMPONENT).is_ok(),
            "core does not reject the reserved word; an adapter does"
        );
    }

    #[test]
    fn ulid_ids_parse_back_and_reject_garbage() {
        let id = ItemId::new();
        assert_eq!(ItemId::parse(id.as_str()).unwrap(), id);
        assert!(ItemId::parse("not-a-ulid").is_err());
        assert!(ItemId::parse("").is_err());
        assert!(AuditId::parse(AuditId::new().as_str()).is_ok());
    }

    #[test]
    fn the_admin_scope_is_recognisable_and_is_not_a_normal_scope() {
        let tenant = TenantId::new("acme").unwrap();
        let admin = Scope::admin(&tenant);

        assert_eq!(admin.tenant, tenant);
        assert_eq!(admin.subject.as_str(), ADMIN_COMPONENT);
        assert_eq!(admin.namespace.as_str(), ADMIN_COMPONENT);
        assert!(admin.is_admin());

        let ordinary = Scope::new("acme", "user-42", "agent").unwrap();
        assert!(!ordinary.is_admin());
    }

    #[test]
    fn a_scope_is_only_admin_when_both_halves_are_reserved() {
        // Half-reserved scopes are ordinary. `is_admin` gates whether audit rows
        // are treated as tenant-level, so a scope that is reserved in only one
        // position must not be mistaken for one the engine wrote.
        let half = Scope::new("acme", ADMIN_COMPONENT, "agent").unwrap();
        assert!(!half.is_admin());
        let other_half = Scope::new("acme", "user-42", ADMIN_COMPONENT).unwrap();
        assert!(!other_half.is_admin());
    }
}
