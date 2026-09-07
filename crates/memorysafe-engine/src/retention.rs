use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionSpan {
    Forever,
    Days(u32),
    /// Detail lives exactly as long as the subject does.
    UntilSubjectPurge,
}

// `PurgeCascade` is **not** defined here. It is a parameter of
// `Backend::purge_subject`, so it lives in `memorysafe-core` alongside
// `AuditRecord` — `memorysafe-backend` may depend on core and not on the
// engine. Re-exported so `AuditRetention` below and every engine caller can
// name it without a second import path.
pub use memorysafe_core::PurgeCascade;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRetention {
    pub detail: RetentionSpan,
    pub purge_cascade: PurgeCascade,
    /// Counts, rates, and score distributions by policy version. Never
    /// identifying, so it can outlive everything else.
    pub aggregate: RetentionSpan,
}

/// The tested, documented surface. Free-form overrides are permitted but
/// unsupported — this is what keeps "configurable per tenant" from becoming an
/// untestable matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionProfile {
    #[default]
    Balanced,
    GdprStrict,
    HipaaRetain,
    Forensic,
}

impl RetentionProfile {
    /// **This match is exhaustive by construction and must stay that way.**
    /// There is no `_ =>` arm, so adding a variant to `RetentionProfile`
    /// makes this fail to compile until the new profile is given its own
    /// literal. That failure is the feature. The compiler enforces it; this
    /// comment exists to stop someone "fixing" the error with a wildcard arm,
    /// which would silently hand a new profile another profile's retention —
    /// a wrong `purge_cascade` there is a destroyed audit trail or an
    /// undeleted one, discovered during an erasure.
    pub fn retention(self) -> AuditRetention {
        match self {
            RetentionProfile::Balanced => AuditRetention {
                detail: RetentionSpan::UntilSubjectPurge,
                purge_cascade: PurgeCascade::Cascade,
                aggregate: RetentionSpan::Forever,
            },
            RetentionProfile::GdprStrict => AuditRetention {
                detail: RetentionSpan::Days(90),
                purge_cascade: PurgeCascade::Cascade,
                aggregate: RetentionSpan::Days(365),
            },
            RetentionProfile::HipaaRetain => AuditRetention {
                detail: RetentionSpan::Days(2190),
                purge_cascade: PurgeCascade::Preserve,
                aggregate: RetentionSpan::Forever,
            },
            RetentionProfile::Forensic => AuditRetention {
                detail: RetentionSpan::Forever,
                purge_cascade: PurgeCascade::Preserve,
                aggregate: RetentionSpan::Forever,
            },
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "balanced" => Some(RetentionProfile::Balanced),
            "gdpr_strict" => Some(RetentionProfile::GdprStrict),
            "hipaa_retain" => Some(RetentionProfile::HipaaRetain),
            "forensic" => Some(RetentionProfile::Forensic),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors `memorysafe_core::audit::purge_cascade_serializes_as_snake_case`.
    // That test's own comment gives the reason this one exists too:
    // `AuditRetention` is deserialized from tenant configuration, so a variant
    // renamed on the wire silently turns a configured `gdpr_strict` into a
    // parse error in the one path that erases a subject. `from_name` pins the
    // *config-key* spelling; nothing pinned the *wire* spelling until this
    // test — the two are independent (a rename on one enum's `#[serde(...)]`
    // attribute cannot touch the other's `match` arms), so deleting either
    // guard leaves the other blind to it.
    //
    // Rejects: the plausible struct/enum written without the
    // `#[serde(rename_all = "snake_case")]` attribute every neighbour in this
    // module carries — it would emit `"GdprStrict"`/`"UntilSubjectPurge"`.
    //
    // Vacuous if: nothing, unlike the sibling test — `GdprStrict` and
    // `UntilSubjectPurge` each mix multiple words, so `snake_case` and a
    // hypothetical unrenamed default diverge visibly, and `Days`'s wire key
    // (a struct-variant-like `{"days": n}`) diverges too.
    #[test]
    fn retention_profile_and_span_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&RetentionProfile::GdprStrict).unwrap(),
            "\"gdpr_strict\""
        );
        assert_eq!(
            serde_json::from_str::<RetentionProfile>("\"gdpr_strict\"").unwrap(),
            RetentionProfile::GdprStrict
        );

        assert_eq!(
            serde_json::to_string(&RetentionSpan::UntilSubjectPurge).unwrap(),
            "\"until_subject_purge\""
        );
        assert_eq!(
            serde_json::from_str::<RetentionSpan>("\"until_subject_purge\"").unwrap(),
            RetentionSpan::UntilSubjectPurge
        );

        // `Days` carries a value, so it round-trips through its tagged form
        // (`{"days":90}`) rather than a bare string; the tag itself still
        // owes its `days` spelling to `rename_all`.
        let days = RetentionSpan::Days(90);
        let wire = serde_json::to_string(&days).unwrap();
        assert_eq!(wire, "{\"days\":90}");
        assert_eq!(serde_json::from_str::<RetentionSpan>(&wire).unwrap(), days);
    }
}
