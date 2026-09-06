//! The conformance suite. Every backend must pass it unmodified.
//!
//! This is the load-bearing artifact of the two-backend design: without it,
//! SQLite and Postgres drift within a month and the `Backend` trait becomes a
//! lie. Backends call `run_conformance_suite` from their own integration test.

pub mod atomicity;
pub mod fixtures;
pub mod isolation;

pub use fixtures as fx;

use crate::Backend;
use std::future::Future;

/// Hands out a pristine backend per test. SQLite returns one rooted in a fresh
/// `TempDir`; Postgres will return one rooted in a fresh schema.
pub trait BackendFactory: Send + Sync {
    type B: Backend;
    fn create(&self) -> impl Future<Output = Self::B> + Send;
}

/// Runs every conformance test in order. Panics on the first failure with the
/// test's own assertion message.
///
/// What this suite proves, and what it does not: every test here observes
/// state through the `Backend` trait after `apply` returns — item present or
/// absent, evictions gone, audit rows counted. That catches a backend that
/// skips a write, fabricates a success, or leaves a failed transaction's
/// side effects behind. It does not prove atomicity in the transactional
/// sense: there is no fault injection and no concurrent observer, so a
/// backend that performs the item write, the evictions, and the audit row
/// as three separate, non-atomic commits — and simply does not crash
/// between them — passes it too.
/// `atomicity::admit_evict_and_audit_commit_together` is the test whose
/// name promises more than it can check; read it as "the end state after a
/// successful apply is internally consistent," not as proof the three
/// writes committed as one transaction.
pub async fn run_conformance_suite<F: BackendFactory>(factory: &F) {
    macro_rules! run {
        ($($test:path),* $(,)?) => {
            $(
                eprintln!("conformance: {}", stringify!($test));
                $test(factory).await;
            )*
        };
    }

    run!(
        isolation::tenants_are_isolated,
        isolation::subjects_are_isolated,
        isolation::namespaces_are_separated,
        isolation::audit_is_scoped,
        atomicity::admit_evict_and_audit_commit_together,
        atomicity::a_failed_transaction_leaves_no_trace,
        atomicity::every_mutation_writes_exactly_one_audit_record,
        atomicity::idempotent_writes_replay_the_original_outcome,
        atomicity::idempotency_conflict_on_different_payload,
    );
}
