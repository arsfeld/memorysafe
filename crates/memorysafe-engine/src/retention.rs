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

/// **One of these three fields is enforced. Two are not.**
///
/// `purge_cascade` is live: `Engine::purge_subject` reads it and passes it to
/// `Backend::purge_subject`, which honours it (`mutate.rs`). `detail` and
/// `aggregate` have no reader outside a test — `grep -rn 'retention()\.'
/// crates/` finds exactly one hit under `src/`, and it reads `purge_cascade`;
/// every other hit is `tests/retention.rs` asserting the table below against
/// itself. Nothing expires an audit row or an aggregate row on a schedule.
/// There is no expiry job, and no `Backend` method to build one on.
///
/// **This is stated because leaving it unstated makes it a false claim rather
/// than a known gap.** An operator who selects `GdprStrict` reads
/// `detail: Days(90)` as "detail rows are deleted after ninety days" and
/// configures a compliance posture on it; what they get is rows that live
/// until the subject is purged, exactly as `Balanced` gives — the two
/// profiles are indistinguishable in everything but their cascade today. The
/// profile table below is therefore a *declaration of intent* for two of its
/// three columns and a working switch for the third.
///
/// Building the expiry pass is deliberately not done here. It needs a
/// `Backend` method to delete audit rows older than a bound (there is none;
/// `purge_subject` is by subject, not by age), a scheduler to run it, and a
/// decision about what `UntilSubjectPurge` means for a subject that is never
/// purged. That is a task, not a fix, and it is ledgered as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRetention {
    /// How long detail rows are meant to live. **Not enforced** — see this
    /// type's doc.
    pub detail: RetentionSpan,
    /// The one field with a mechanism behind it: `Engine::purge_subject`
    /// reads it on every erasure.
    pub purge_cascade: PurgeCascade,
    /// Counts, rates, and score distributions by policy version. Never
    /// identifying, so it can outlive everything else. **Not enforced** — see
    /// this type's doc.
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
