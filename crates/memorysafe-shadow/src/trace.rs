use memorysafe_core::{ReasonCode, Scope, SensitivityLevel};
use serde::{Deserialize, Serialize};

/// `memorysafe_core::Protection` with its wall-clock deadline replaced by a
/// window length relative to the item's own `created_at`.
///
/// **Not a copy of `Protection` for its own sake.** `Protection::Protected`'s
/// `until` is `created_at + Duration::days(protection_window_days)`
/// (`memorysafe_policy::admit::decide`) — an absolute timestamp computed from
/// wall-clock `now` at write time. Putting that timestamp in a trace verbatim
/// is exactly the defect this crate exists to rule out: it does not even need
/// two different processes or two different days to vary, since a run whose
/// two `remember` calls straddle a one-second boundary already produces two
/// different `until` values for what is otherwise an identical decision.
/// `window_seconds` is `until - created_at`, which is invariant under
/// `protection_window_days` alone and carries the one piece of that decision
/// a diff actually cares about — how long the window is — with the wall clock
/// projected out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TracedProtection {
    Normal,
    Protected { window_seconds: i64 },
    Pinned,
}

/// What happened, with everything run-specific removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TracedAction {
    Retain {
        protection: TracedProtection,
    },
    /// The write this merged into, by sequence number. `None` when the target
    /// predates the scenario — a corpus item this run did not create.
    Merge {
        into_seq: Option<usize>,
    },
    Reject,
}

impl TracedAction {
    /// The label a transition histogram is keyed on.
    pub fn label(&self) -> &'static str {
        match self {
            TracedAction::Retain { .. } => "retain",
            TracedAction::Merge { .. } => "merge",
            TracedAction::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TracedDecision {
    pub seq: usize,
    pub scope: Scope,
    /// BLAKE3 of the body, hex. Identifies the write without carrying it.
    pub body_digest: String,
    pub action: TracedAction,
    pub reason_codes: Vec<ReasonCode>,
    pub evicted: usize,
    /// The level the item was stored at. `None` when nothing was stored.
    /// A policy that silently downgrades sensitivity is exactly the regression
    /// this harness exists to catch, so it belongs in the trace.
    pub sensitivity: Option<SensitivityLevel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub scenario: String,
    pub policy: memorysafe_core::PolicyId,
    pub decisions: Vec<TracedDecision>,
}
