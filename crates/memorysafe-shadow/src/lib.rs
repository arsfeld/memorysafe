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

// `replay` is filled in by Task 17; this task leaves it as an empty module
// (see its own doc comment), so nothing is re-exported from it yet.
pub use diff::{DecisionChange, TraceDiff, diff};
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
