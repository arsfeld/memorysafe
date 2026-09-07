//! Shadow evaluation.
//!
//! A `Scenario` is a corpus plus an ordered sequence of writes. Running one
//! against a policy produces a `Trace`: the decision and reasons for every
//! write, with everything that differs between runs — ids, timestamps, bodies —
//! left out. Two traces over one scenario can therefore be diffed exactly,
//! which is what makes a governance policy improvable without gambling on
//! customer data.

pub mod diff;
pub mod replay;
pub mod run;
pub mod scenario;
pub mod trace;

// `diff` and `replay` are filled in by Tasks 16 and 17; this task leaves
// them as empty modules (see their own doc comments), so nothing is
// re-exported from either here yet — the plan's own lib.rs sketch for this
// task re-exported `DecisionChange`/`TraceDiff`/`diff` from a module it also
// says to leave empty, which cannot resolve. Left for whichever of those
// tasks actually defines those items to add its own `pub use`.
pub use run::run;
pub use scenario::{Scenario, ScenarioWrite, Unreplayable};
pub use trace::{Trace, TracedAction, TracedDecision, TracedProtection};

use memorysafe_core::CoreError;
use memorysafe_engine::EngineError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShadowError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(
        "traces cover different numbers of decisions ({before} and {after}); they are not \
             two runs of one scenario"
    )]
    Misaligned { before: usize, after: usize },
    #[error("cannot build a scenario: {0}")]
    Malformed(String),
}
