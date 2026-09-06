//! The conformance suite, run against the SQLite backend.
//!
//! **This is the first time any conformance test executes against a real
//! backend.** The suite's fifty functions were written, reviewed and frozen
//! into two plan documents entirely on the strength of reading them.
//!
//! # What is bound here, and what is not
//!
//! Task 20 implements `get`, `list`, `audit`, `record_recall` and a first
//! `apply` covering insert + eviction + audit; Task 21 adds `vectors.rs` and
//! a real `neighbours`. Every test below **except one** is one the
//! null-backend census (`memorysafe_backend::conformance::null`) measured as
//! **failing** against a backend that does nothing, so each is discriminating
//! before this crate's implementation exists.
//!
//! The one exception is `retrieval::cross_model_vectors_are_rejected`: it is
//! `NULL_TOLERANT` (`conformance::null`) because it accepts either an error
//! or an empty result, so it also passes against a backend that does nothing.
//! Its presence is covered by its sibling in the same list,
//! `retrieval::neighbours_break_ties_before_truncating_at_k` — both are bound
//! here, together, which is what makes the pairing sound: split across tasks,
//! the absence test would sit green and meaningless in between. Going green
//! is not evidence the cross-model rejection works on its own; the mutation
//! testing in the task report is.
//!
//! The remaining four tests of the isolation and atomicity modules are bound
//! by the task that supplies the method they observe, and are deliberately
//! absent rather than bound-and-failing:
//!
//! - `isolation::retrieval_never_crosses_a_scope_boundary` — reads through
//!   *both* `retrieve_candidates` and `neighbours`. Task 21 supplies only
//!   `neighbours`, so this binds at Task 22, with `retrieve_candidates`.
//! - `atomicity::a_failed_transaction_leaves_no_trace` — requires a merge and
//!   `BackendError::MergeTargetMissing`. Task 23.
//! - `atomicity::idempotent_writes_replay_the_original_outcome` and
//!   `atomicity::idempotency_conflict_on_different_payload` — require the
//!   `idempotency` table's read-write path. Task 23.
//!
//! `run_conformance_suite` is not called: it awaits each test inline, so the
//! first unimplemented method would end the run. It arrives once every method
//! is real.

use memorysafe_backend::conformance::retrieval;
use memorysafe_backend::conformance::{BackendFactory, atomicity, isolation};
use memorysafe_backend_sqlite::SqliteBackend;

/// Each test gets a backend rooted in its own `TempDir`. The directory is
/// leaked deliberately: it must outlive the backend, and the OS reclaims it.
struct SqliteFactory;

impl BackendFactory for SqliteFactory {
    type B = SqliteBackend;
    // `async fn`, not `fn create(&self) -> impl Future<..> + Send`. The trait
    // declares the latter and both are the same signature after desugaring,
    // but `clippy::manual_async_fn` is denied workspace-wide and fires on the
    // hand-written form. The `Send` bound the trait states is still checked
    // here: a factory whose future were not `Send` fails to compile.
    async fn create(&self) -> Self::B {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        SqliteBackend::open(path)
    }
}

#[tokio::test]
async fn tenants_are_isolated() {
    isolation::tenants_are_isolated(&SqliteFactory).await;
}

#[tokio::test]
async fn subjects_are_isolated() {
    isolation::subjects_are_isolated(&SqliteFactory).await;
}

#[tokio::test]
async fn namespaces_are_separated() {
    isolation::namespaces_are_separated(&SqliteFactory).await;
}

#[tokio::test]
async fn audit_is_scoped() {
    isolation::audit_is_scoped(&SqliteFactory).await;
}

#[tokio::test]
async fn admit_evict_and_audit_commit_together() {
    atomicity::admit_evict_and_audit_commit_together(&SqliteFactory).await;
}

#[tokio::test]
async fn an_invalid_transaction_is_rejected_and_writes_nothing() {
    atomicity::an_invalid_transaction_is_rejected_and_writes_nothing(&SqliteFactory).await;
}

#[tokio::test]
async fn every_mutation_writes_exactly_one_audit_record() {
    atomicity::every_mutation_writes_exactly_one_audit_record(&SqliteFactory).await;
}

#[tokio::test]
async fn vector_search_ranks_by_similarity() {
    retrieval::vector_search_ranks_by_similarity(&SqliteFactory).await;
}

#[tokio::test]
async fn cross_model_vectors_are_rejected() {
    retrieval::cross_model_vectors_are_rejected(&SqliteFactory).await;
}

#[tokio::test]
async fn neighbours_break_ties_before_truncating_at_k() {
    retrieval::neighbours_break_ties_before_truncating_at_k(&SqliteFactory).await;
}
