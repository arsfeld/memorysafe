//! The conformance suite. Every backend must pass it unmodified.
//!
//! This is the load-bearing artifact of the two-backend design: without it,
//! SQLite and Postgres drift within a month and the `Backend` trait becomes a
//! lie. Backends call `run_conformance_suite` from their own integration test.

pub mod atomicity;
pub mod capacity;
pub mod fixtures;
pub mod isolation;
pub mod lifecycle;
#[cfg(test)]
mod null;
pub mod retrieval;

pub use fixtures as fx;

use crate::Backend;
use std::future::Future;

/// Hands out a pristine backend per `create()` call, not per test. SQLite
/// returns one rooted in a fresh `TempDir`; Postgres will return one rooted
/// in a fresh schema.
///
/// Most tests call `create()` once, but `lifecycle::export_import_round_trips_exactly`
/// and `lifecycle::import_is_idempotent` each hold a source and a target
/// backend simultaneously and require them to be mutually invisible — a
/// factory that memoized one instance per test would hand both calls the
/// same backend, and the target would already contain the source's rows
/// before import ever ran. For Postgres this means two schemas alive at
/// once, not one schema reused across a test's two `create()` calls.
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
///
/// `F::B: 'static` is required because `capacity::concurrent_admits_do_not_double_count`
/// hands `Arc<F::B>` to `tokio::spawn`, which demands a `'static` future.
/// Every real backend owns its state outright and satisfies this trivially.
///
/// Must be driven from a multi-threaded Tokio runtime (for example
/// `#[tokio::test(flavor = "multi_thread")]`). `tokio::spawn` also runs
/// under the `current_thread` flavor, but there tasks only interleave at
/// `.await` points, so `capacity::concurrent_admits_do_not_double_count`'s
/// attempt to provoke a genuine concurrent write race loses most of its
/// bite — the lock-free accounting bug it exists to catch can hide on a
/// single OS thread.
pub async fn run_conformance_suite<F: BackendFactory>(factory: &F)
where
    F::B: 'static,
{
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
        isolation::retrieval_never_crosses_a_scope_boundary,
        atomicity::admit_evict_and_audit_commit_together,
        atomicity::a_failed_transaction_leaves_no_trace,
        atomicity::an_invalid_transaction_is_rejected_and_writes_nothing,
        atomicity::every_mutation_writes_exactly_one_audit_record,
        atomicity::idempotent_writes_replay_the_original_outcome,
        atomicity::idempotency_conflict_on_different_payload,
        retrieval::sensitivity_ceiling_is_enforced_in_the_query,
        retrieval::tag_and_kind_filters_narrow_results,
        retrieval::vector_search_ranks_by_similarity,
        retrieval::keyword_search_finds_exact_terms,
        retrieval::keyword_search_escapes_user_input,
        retrieval::hybrid_returns_both_signal_sources,
        retrieval::list_pages_are_disjoint_and_complete,
        retrieval::list_orders_oldest_first_by_created_at,
        retrieval::list_tie_break_is_total_over_identical_timestamps,
        retrieval::pending_embedding_items_are_excluded_when_asked,
        retrieval::cross_model_vectors_are_rejected,
        retrieval::neighbours_break_ties_before_truncating_at_k,
        retrieval::recall_updates_access_statistics,
        capacity::capacity_accounting_tracks_items_and_bytes,
        capacity::eviction_releases_capacity,
        capacity::concurrent_admits_do_not_double_count,
        capacity::scope_stats_reflect_the_corpus,
        lifecycle::audit_filter_narrows_by_event_and_time,
        lifecycle::audit_returns_min_of_the_limit_and_the_rows_that_remain,
        lifecycle::audit_pages_by_the_after_cursor_without_repeating_a_row,
        lifecycle::audit_since_and_until_include_a_record_on_the_boundary,
        lifecycle::purge_subject_removes_everything_for_that_subject,
        lifecycle::purge_subject_leaves_other_subjects_intact,
        lifecycle::purge_subject_preserves_audit_when_asked,
        lifecycle::purge_subject_persists_the_record_it_was_given,
        lifecycle::apply_persists_the_audit_id_it_was_given,
        lifecycle::record_recall_persists_the_audit_id_it_was_given,
        lifecycle::import_preserves_every_audit_id,
        lifecycle::export_narrows_to_the_selectors_subject_and_namespace,
        lifecycle::export_orders_the_stream_by_kind_then_by_id,
        lifecycle::export_import_round_trips_exactly,
        lifecycle::import_is_idempotent,
        lifecycle::import_rejects_a_later_record_whose_tenant_disagrees,
        lifecycle::import_rejects_a_foreign_audit_record_even_when_every_item_agrees,
        lifecycle::audit_aggregates_survive_a_cascading_purge,
        lifecycle::audit_aggregates_page_in_the_documented_order,
        lifecycle::audit_aggregates_resume_from_a_cursor_that_names_no_stored_row,
        lifecycle::audit_aggregates_narrow_by_day_window_and_policy,
    );
}
