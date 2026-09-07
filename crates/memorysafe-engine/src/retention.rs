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
