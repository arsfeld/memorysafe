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
//! a real `neighbours`; Task 22 adds `keyword.rs`, `retrieve.rs` and a real
//! `retrieve_candidates`. Every test below **except two** is one the
//! null-backend census (`memorysafe_backend::conformance::null`) measured as
//! **failing** against a backend that does nothing, so each is discriminating
//! before this crate's implementation exists.
//!
//! The two exceptions are both `NULL_TOLERANT` (`conformance::null`) —
//! absence-shaped tests that pass against a backend that does nothing because
//! they assert something is *not* there, which an empty result satisfies for
//! free. Each is paired with a sibling that covers the presence case, bound in
//! the same task, which is what makes the pairing sound: split across tasks,
//! the absence test would sit green and meaningless in between. Going green is
//! not evidence the property holds on its own; the mutation testing in the
//! relevant task report is.
//!
//! - `retrieval::cross_model_vectors_are_rejected` (Task 21), paired with
//!   `retrieval::neighbours_break_ties_before_truncating_at_k`.
//! - `retrieval::keyword_search_escapes_user_input` (Task 22), paired with
//!   `retrieval::keyword_search_finds_exact_terms`.
//!
//! The remaining three tests of the atomicity module are bound by the task
//! that supplies the method they observe, and are deliberately absent rather
//! than bound-and-failing:
//!
//! - `atomicity::a_failed_transaction_leaves_no_trace` — requires a merge and
//!   `BackendError::MergeTargetMissing`. Task 23.
//! - `atomicity::idempotent_writes_replay_the_original_outcome` and
//!   `atomicity::idempotency_conflict_on_different_payload` — require the
//!   `idempotency` table's read-write path. Task 23.
//!
//! `isolation::retrieval_never_crosses_a_scope_boundary` bound at Task 22: it
//! reads through *both* `retrieve_candidates` and `neighbours`, and Task 21
//! supplied only `neighbours`.
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

#[tokio::test]
async fn sensitivity_ceiling_is_enforced_in_the_query() {
    retrieval::sensitivity_ceiling_is_enforced_in_the_query(&SqliteFactory).await;
}

#[tokio::test]
async fn tag_and_kind_filters_narrow_results() {
    retrieval::tag_and_kind_filters_narrow_results(&SqliteFactory).await;
}

#[tokio::test]
async fn keyword_search_finds_exact_terms() {
    retrieval::keyword_search_finds_exact_terms(&SqliteFactory).await;
}

#[tokio::test]
async fn keyword_search_escapes_user_input() {
    retrieval::keyword_search_escapes_user_input(&SqliteFactory).await;
}

#[tokio::test]
async fn hybrid_returns_both_signal_sources() {
    retrieval::hybrid_returns_both_signal_sources(&SqliteFactory).await;
}

#[tokio::test]
async fn list_pages_are_disjoint_and_complete() {
    retrieval::list_pages_are_disjoint_and_complete(&SqliteFactory).await;
}

#[tokio::test]
async fn list_orders_oldest_first_by_created_at() {
    retrieval::list_orders_oldest_first_by_created_at(&SqliteFactory).await;
}

#[tokio::test]
async fn list_tie_break_is_total_over_identical_timestamps() {
    retrieval::list_tie_break_is_total_over_identical_timestamps(&SqliteFactory).await;
}

#[tokio::test]
async fn pending_embedding_items_are_excluded_when_asked() {
    retrieval::pending_embedding_items_are_excluded_when_asked(&SqliteFactory).await;
}

#[tokio::test]
async fn recall_updates_access_statistics() {
    retrieval::recall_updates_access_statistics(&SqliteFactory).await;
}

// Wired here rather than with the other isolation tests in the items-and-audit
// task: it calls `retrieve_candidates` and `neighbours`, so it cannot pass
// until both retrieval arms exist.
#[tokio::test]
async fn retrieval_never_crosses_a_scope_boundary() {
    isolation::retrieval_never_crosses_a_scope_boundary(&SqliteFactory).await;
}
