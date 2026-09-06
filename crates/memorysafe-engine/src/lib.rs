//! Orchestration. The engine performs all I/O, hands pure data to the policy,
//! validates everything the policy returns, and applies writes atomically with
//! an audit record.

pub mod error;
pub mod validate;

pub use error::EngineError;
pub use validate::FailureStance;
