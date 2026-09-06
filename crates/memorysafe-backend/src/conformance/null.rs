//! The null backend, and the vacuity census it exists to run.
//!
//! # What this is
//!
//! [`NullBackend`] implements all fourteen `Backend` methods by returning
//! success with the empty or zero value of the return type. It stores
//! nothing, reads nothing back, enforces nothing. **A conformance test that
//! passes against it asserts nothing that a real backend could fail.**
//!
//! That makes the census below the cheapest discrimination check the suite
//! can be given: it needs no reference implementation and no judgement about
//! which tests to suspect, because it asks all of them the same question.
//!
//! # Why `#[cfg(test)]` rather than a `test-support` feature
//!
//! Nothing outside this crate needs a null backend. The conformance suite
//! lives here, so the instrument that measures it can live here too, and
//! `#[cfg(test)]` keeps the public surface, the feature matrix and the
//! dependency set all exactly where they were. A `test-support` feature
//! would export a `Backend` implementation that lies about everything it is
//! asked — worth the cost only if a downstream crate had to import it, and
//! none does. `cargo clippy --all-targets` compiles this module regardless,
//! so the lint coverage is the same either way.
//!
//! # Why the census does not call `run_conformance_suite`
//!
//! `run_conformance_suite`'s `run!` macro awaits each test inline, so the
//! first panic ends the run and everything after it goes unmeasured. The
//! census needs a verdict for all forty-nine. It therefore spawns each test
//! as its own Tokio task and reads the `JoinHandle`: a panic arrives as
//! `Err(JoinError)` instead of unwinding the census. The list below is a
//! transcription of `run_conformance_suite`'s own list, in its order, and
//! `the_census_measures_every_test_the_suite_runs` compares the two name by
//! name against `mod.rs` itself, so a test added there cannot go unmeasured
//! here.

use crate::{
    AppliedWrite, AuditAggregate, AuditAggregateFilter, Backend, BackendError, CandidateQuery,
    ExportStream, ImportReport, ImportStream, Page, PurgeReport, ScopeSelector, WriteTransaction,
};
use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, Budget, CapacityState, Embedding, ItemId, MemoryItem,
    PurgeCascade, Scope, ScopeStats, ScoredCandidate, SubjectId, TenantId,
};

use super::BackendFactory;
use std::future::Future;

/// The zero `AuditId`: a nil ULID.
///
/// Built explicitly rather than with `AuditId::new()`, which would mint a
/// fresh, plausible-looking id. The point of this backend is that it does
/// nothing, and "returns an id unrelated to the one it was handed" is a
/// truer null than "returns a brand-new id that could be mistaken for a
/// persisted one". Either way the four echo-rule tests must notice.
fn nil_audit_id() -> AuditId {
    AuditId::parse("00000000000000000000000000").expect("a nil ULID is a valid AuditId")
}

/// A `Backend` that accepts every call and does nothing.
#[derive(Debug, Clone, Copy)]
pub struct NullBackend;

#[async_trait::async_trait]
impl Backend for NullBackend {
    async fn retrieve_candidates(
        &self,
        _scope: &Scope,
        _query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        Ok(vec![])
    }

    async fn neighbours(
        &self,
        _scope: &Scope,
        _embedding: &Embedding,
        _k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError> {
        Ok(vec![])
    }

    async fn capacity_state(&self, _scope: &Scope) -> Result<CapacityState, BackendError> {
        Ok(CapacityState {
            budget: Budget::UNBOUNDED,
            used_items: 0,
            used_bytes: 0,
        })
    }

    async fn scope_stats(&self, _scope: &Scope) -> Result<ScopeStats, BackendError> {
        Ok(ScopeStats {
            item_count: 0,
            total_bytes: 0,
            mean_neighbour_similarity: 0.0,
            median_item_bytes: 0,
        })
    }

    async fn apply(&self, _txn: WriteTransaction) -> Result<AppliedWrite, BackendError> {
        Ok(AppliedWrite {
            item_id: None,
            audit_id: nil_audit_id(),
            evicted: vec![],
            replayed: false,
            replayed_outcome: None,
        })
    }

    async fn record_recall(&self, _record: AuditRecord) -> Result<AuditId, BackendError> {
        Ok(nil_audit_id())
    }

    async fn get(&self, _scope: &Scope, _id: &ItemId) -> Result<Option<MemoryItem>, BackendError> {
        Ok(None)
    }

    async fn list(&self, _scope: &Scope, _page: &Page) -> Result<Vec<MemoryItem>, BackendError> {
        Ok(vec![])
    }

    async fn audit(
        &self,
        _scope: &Scope,
        _filter: &AuditFilter,
    ) -> Result<Vec<AuditRecord>, BackendError> {
        Ok(vec![])
    }

    async fn purge_subject(
        &self,
        _tenant: &TenantId,
        _subject: &SubjectId,
        _cascade: PurgeCascade,
        _audit: AuditRecord,
    ) -> Result<PurgeReport, BackendError> {
        Ok(PurgeReport {
            items_removed: 0,
            vectors_removed: 0,
            audit_rows_removed: 0,
            audit_rows_preserved: 0,
        })
    }

    async fn audit_aggregates(
        &self,
        _tenant: &TenantId,
        _filter: &AuditAggregateFilter,
    ) -> Result<Vec<AuditAggregate>, BackendError> {
        Ok(vec![])
    }

    async fn export(&self, _sel: &ScopeSelector) -> Result<ExportStream, BackendError> {
        Ok(vec![])
    }

    async fn import(
        &self,
        _destination: &TenantId,
        _stream: ImportStream,
    ) -> Result<ImportReport, BackendError> {
        Ok(ImportReport {
            items_imported: 0,
            vectors_imported: 0,
            audit_imported: 0,
            items_skipped_existing: 0,
        })
    }

    async fn set_budget(&self, _scope: &Scope, _budget: Budget) -> Result<(), BackendError> {
        Ok(())
    }
}

/// Hands out a fresh `NullBackend` per call, as the factory contract requires.
///
/// Two `NullBackend`s are trivially mutually invisible: neither can see
/// anything, including its own writes. The export/import pair of tests that
/// need two live backends therefore gets exactly what it asks for.
#[derive(Debug, Clone, Copy)]
pub struct NullFactory;

impl BackendFactory for NullFactory {
    type B = NullBackend;
    fn create(&self) -> impl Future<Output = Self::B> + Send {
        std::future::ready(NullBackend)
    }
}

// ---------------------------------------------------------------------------
// The census
// ---------------------------------------------------------------------------

/// One test's verdict against [`NullBackend`].
#[derive(Debug)]
enum Verdict {
    /// The test returned without asserting anything the null backend could
    /// violate.
    Passed,
    /// The test panicked. A failed assertion and an `unwrap()` on a `None`
    /// the test expected to be `Some` are both failures, and both are the
    /// right outcome.
    Failed(String),
}

fn panic_message(err: tokio::task::JoinError) -> String {
    if err.is_cancelled() {
        return "task cancelled".to_owned();
    }
    let payload = err.into_panic();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panicked with a non-string payload".to_owned()
    }
}

/// The census list. **A transcription of `run_conformance_suite`'s `run!`
/// list, in its order.** `census_covers_every_test_in_the_suite` pins the
/// count so a test added to the suite and not to this list is a red test
/// rather than a silent gap.
macro_rules! census {
    ($($test:path),* $(,)?) => {{
        let mut out: Vec<(&'static str, Verdict)> = Vec::new();
        $(
            {
                // Spawned, not awaited inline: a panic must be this census's
                // datum, not the end of the run.
                let handle = tokio::spawn(async { $test(&NullFactory).await });
                let verdict = match handle.await {
                    Ok(()) => Verdict::Passed,
                    Err(e) => Verdict::Failed(panic_message(e)),
                };
                out.push((stringify!($test), verdict));
            }
        )*
        out
    }};
}

/// Runs every conformance test against [`NullBackend`], one spawned task
/// each, and returns each test's verdict in suite order.
///
/// **Computed once per process.** Two tests below read the census, and
/// running it twice would be both wasted work and a race on the panic hook:
/// each run would install its own, take the other's back, and the
/// suppression would come apart. The `OnceCell` makes the hook installation
/// happen exactly once, before any conformance code runs.
///
/// The hook exists because forty-seven expected panics per CI run is noise
/// and the message is captured into the `Verdict` anyway. It is permanent —
/// restoring it would reintroduce the race it exists to avoid — and it is
/// narrow: it swallows a panic only when the panic's own source file is one
/// of the suite's, listed in [`SUPPRESSED_PANIC_SITES`]. Everything else goes
/// to the hook installed before it.
///
/// **`null.rs` is deliberately not on that list.** The first version of this
/// matched on the `conformance/` directory, which meant the guard swallowed
/// its own assertion messages: it failed CI red and silent, with nothing to
/// say why. A guard whose failure mode is "red with no message" is most of a
/// guard that does not work.
async fn census() -> &'static [(&'static str, Verdict)] {
    static CENSUS: tokio::sync::OnceCell<Vec<(&'static str, Verdict)>> =
        tokio::sync::OnceCell::const_new();
    CENSUS.get_or_init(run_census).await.as_slice()
}

/// Files whose panics the census suppresses: the five suite modules and the
/// fixtures they build corpora with. Not this file.
const SUPPRESSED_PANIC_SITES: &[&str] = &[
    "isolation.rs",
    "atomicity.rs",
    "retrieval.rs",
    "capacity.rs",
    "lifecycle.rs",
    "fixtures.rs",
];

async fn run_census() -> Vec<(&'static str, Verdict)> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let from_suite = info.location().is_some_and(|l| {
            let file = l.file();
            file.contains("conformance") && SUPPRESSED_PANIC_SITES.iter().any(|f| file.ends_with(f))
        });
        if !from_suite {
            previous(info);
        }
    }));

    census!(
        super::isolation::tenants_are_isolated,
        super::isolation::subjects_are_isolated,
        super::isolation::namespaces_are_separated,
        super::isolation::audit_is_scoped,
        super::isolation::retrieval_never_crosses_a_scope_boundary,
        super::atomicity::admit_evict_and_audit_commit_together,
        super::atomicity::a_failed_transaction_leaves_no_trace,
        super::atomicity::an_invalid_transaction_is_rejected_and_writes_nothing,
        super::atomicity::every_mutation_writes_exactly_one_audit_record,
        super::atomicity::idempotent_writes_replay_the_original_outcome,
        super::atomicity::idempotency_conflict_on_different_payload,
        super::retrieval::sensitivity_ceiling_is_enforced_in_the_query,
        super::retrieval::tag_and_kind_filters_narrow_results,
        super::retrieval::vector_search_ranks_by_similarity,
        super::retrieval::keyword_search_finds_exact_terms,
        super::retrieval::keyword_search_escapes_user_input,
        super::retrieval::hybrid_returns_both_signal_sources,
        super::retrieval::list_pages_are_disjoint_and_complete,
        super::retrieval::list_orders_oldest_first_by_created_at,
        super::retrieval::list_tie_break_is_total_over_identical_timestamps,
        super::retrieval::pending_embedding_items_are_excluded_when_asked,
        super::retrieval::cross_model_vectors_are_rejected,
        super::retrieval::neighbours_break_ties_before_truncating_at_k,
        super::retrieval::recall_updates_access_statistics,
        super::capacity::capacity_accounting_tracks_items_and_bytes,
        super::capacity::eviction_releases_capacity,
        super::capacity::concurrent_admits_do_not_double_count,
        super::capacity::scope_stats_reflect_the_corpus,
        super::lifecycle::audit_filter_narrows_by_event_and_time,
        super::lifecycle::audit_returns_min_of_the_limit_and_the_rows_that_remain,
        super::lifecycle::audit_pages_by_the_after_cursor_without_repeating_a_row,
        super::lifecycle::audit_since_and_until_include_a_record_on_the_boundary,
        super::lifecycle::purge_subject_removes_everything_for_that_subject,
        super::lifecycle::purge_subject_leaves_other_subjects_intact,
        super::lifecycle::purge_subject_preserves_audit_when_asked,
        super::lifecycle::purge_subject_persists_the_record_it_was_given,
        super::lifecycle::apply_persists_the_audit_id_it_was_given,
        super::lifecycle::record_recall_persists_the_audit_id_it_was_given,
        super::lifecycle::import_preserves_every_audit_id,
        super::lifecycle::export_narrows_to_the_selectors_subject_and_namespace,
        super::lifecycle::export_orders_the_stream_by_kind_then_by_id,
        super::lifecycle::export_import_round_trips_exactly,
        super::lifecycle::import_is_idempotent,
        super::lifecycle::import_rejects_a_later_record_whose_tenant_disagrees,
        super::lifecycle::import_rejects_a_foreign_audit_record_even_when_every_item_agrees,
        super::lifecycle::audit_aggregates_survive_a_cascading_purge,
        super::lifecycle::audit_aggregates_page_in_the_documented_order,
        super::lifecycle::audit_aggregates_resume_from_a_cursor_that_names_no_stored_row,
        super::lifecycle::audit_aggregates_narrow_by_day_window_and_policy,
    )
}

/// `stringify!` on a `path` fragment spaces out the `::`, and the census list
/// is written `super::`-qualified. Normalise so a census name is spelled the
/// way `run!` spells it.
fn tidy(name: &str) -> String {
    name.replace(' ', "")
        .trim_start_matches("super::")
        .to_owned()
}

// ---------------------------------------------------------------------------
// Reading the suite's own source
// ---------------------------------------------------------------------------
//
// Checks 1 and 3 below are questions about the suite, and the suite's source
// is the only place the answers live. `include_str!` reads it at compile
// time: no runtime file access, no I/O dependency, and a stale copy is
// impossible because the copy *is* the file.

const SUITE_SOURCES: &[(&str, &str)] = &[
    ("isolation", include_str!("isolation.rs")),
    ("atomicity", include_str!("atomicity.rs")),
    ("retrieval", include_str!("retrieval.rs")),
    ("capacity", include_str!("capacity.rs")),
    ("lifecycle", include_str!("lifecycle.rs")),
];

const SUITE_LIST_SOURCE: &str = include_str!("mod.rs");

/// Setup methods. **Excluded from check 3's intersection, deliberately and by
/// name rather than by inference.**
///
/// 45 of the 49 conformance tests call `apply` to build a corpus, so an
/// intersection that counted it would be non-empty for almost any pair drawn
/// from the suite — it would measure that both are tests, not that they cover
/// a common surface. Any predicate of the form "what does this test touch"
/// has to exclude the ubiquitous for the same reason.
const SETUP_METHODS: &[&str] = &[
    "apply",
    "record_recall",
    "import",
    "purge_subject",
    "set_budget",
];

/// Observation methods: where a conformance test's assertions actually read
/// from. Check 3 intersects over exactly these.
const READ_METHODS: &[&str] = &[
    "audit",
    "audit_aggregates",
    "capacity_state",
    "export",
    "get",
    "list",
    "neighbours",
    "retrieve_candidates",
    "scope_stats",
];

/// The suite's test list, read out of `run_conformance_suite`'s `run!`
/// invocation in source order.
fn suite_list() -> Vec<&'static str> {
    let mut out = vec![];
    let after = SUITE_LIST_SOURCE
        .split_once("run!(")
        .expect("run_conformance_suite must still invoke run!")
        .1;
    for line in after.lines() {
        let line = line.trim();
        if line.starts_with(");") {
            break;
        }
        let name = line.trim_end_matches(',');
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

/// The body of one conformance test, located by name — never by line number.
fn body_of(qualified: &str) -> Option<&'static str> {
    let (module, name) = qualified.split_once("::")?;
    let (_, source) = SUITE_SOURCES.iter().find(|(m, _)| *m == module)?;
    let header = format!("pub async fn {name}");
    let start = source
        .match_indices(&header)
        .find(|(i, _)| {
            // Top level only, and the name must end here rather than being a
            // prefix of a longer one.
            let at_line_start = *i == 0 || source.as_bytes()[i - 1] == b'\n';
            let next = source[i + header.len()..].chars().next();
            at_line_start && matches!(next, Some('<') | Some('('))
        })
        .map(|(i, _)| i)?;
    let rest = &source[start..];
    let end = rest
        .match_indices("\n}")
        .map(|(i, _)| i + 2)
        .next()
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// True when `body` contains a call to `.method(`.
///
/// The name must be followed by `(` once whitespace is skipped, so `.audit(`
/// does not match `.audit_aggregates(`.
fn calls(body: &str, method: &str) -> bool {
    let needle = format!(".{method}");
    let mut from = 0usize;
    while let Some(rel) = body[from..].find(&needle) {
        let after = &body[from + rel + needle.len()..];
        if after.trim_start().starts_with('(') {
            return true;
        }
        from += rel + needle.len();
    }
    false
}

/// The observation methods one conformance test calls.
///
/// Line comments are stripped first, so prose naming a method does not count
/// as touching it. One helper is shared between tests —
/// `lifecycle::seed_aggregate_corpus` — and it calls `apply` and nothing else,
/// so no read method is hidden behind it. A future helper that reads would be,
/// and check 3 would then under-report rather than over-report: it would fail
/// an honest entry rather than pass a dishonest one.
fn read_methods(qualified: &str) -> Vec<&'static str> {
    let Some(body) = body_of(qualified) else {
        return vec![];
    };
    let stripped: String = body
        .lines()
        .map(|l| l.split_once("//").map_or(l, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n");
    READ_METHODS
        .iter()
        .copied()
        .filter(|m| calls(&stripped, m))
        .collect()
}

// ---------------------------------------------------------------------------
// The allow-list
// ---------------------------------------------------------------------------

/// One conformance test that passes against [`NullBackend`] **without being a
/// bad test**, and the evidence for that claim.
struct Tolerance {
    /// The test that passes the null backend, module-qualified as `run!`
    /// spells it.
    test: &'static str,
    /// What the test asserts is *absent*. Prose, checked by a reader, not by
    /// the guard — see check 4.
    asserts_absence_of: &'static str,
    /// The sibling test that covers the presence case. Checks 1 and 2.
    presence_covered_by: &'static str,
    /// The observation method both tests call. Check 3.
    sharing_read_method: &'static str,
}

/// **The census of 2026-09-06, taken at `0a8007f`: 2 of 49 conformance tests
/// pass against a backend that does nothing.** The other 47 fail — 43 on an
/// assertion, 4 by panicking on an empty vec or a `None`, all of which are the
/// right outcome.
///
/// Both survivors are *absence-shaped*: each asserts that something is not
/// there, which an empty result satisfies for free. Neither is vacuous,
/// because for each one a sibling test covers the presence case and that
/// sibling fails the null backend. The entries below record that pairing.
///
/// Absence-shaped appears to be the whole legitimate category, but not by the
/// route it looks like: a test that *requires* an error fails the null
/// backend, since a null backend returns `Ok` — but
/// `cross_model_vectors_are_rejected` *accepts* an error or an empty result,
/// and that deliberate contract tolerance is what lands it here. The rule is
/// "requires an error", not "expects one". **An entry that is not
/// absence-shaped is a defect, not a tolerance, and does not belong in this
/// list.**
const NULL_TOLERANT: &[Tolerance] = &[
    Tolerance {
        test: "retrieval::keyword_search_escapes_user_input",
        asserts_absence_of: "a hostile keyword matching more than one of three rows",
        presence_covered_by: "retrieval::keyword_search_finds_exact_terms",
        sharing_read_method: "retrieve_candidates",
    },
    Tolerance {
        test: "retrieval::cross_model_vectors_are_rejected",
        asserts_absence_of: "neighbour hits for a probe from a different embedder",
        presence_covered_by: "retrieval::neighbours_break_ties_before_truncating_at_k",
        sharing_read_method: "neighbours",
    },
];

/// **No conformance test may pass against a backend that does nothing,
/// unless it is on the allow-list and the allow-list has produced its
/// evidence.**
///
/// A test that passes [`NullBackend`] asserts nothing a real backend could
/// violate. This crate's characteristic defect is exactly that test, and a
/// dozen-odd instances were found by reading before this instrument existed.
/// The guard makes the next one fail at the moment it is written rather than
/// surviving to a contract freeze that binds two implementations.
///
/// # The four checks, and what each is worth
///
/// **A check whose limits live only in someone's head gets quoted as though
/// it had none.** So, in order, and labelled:
///
/// 1. **Mechanical.** The named sibling exists in `run!`. Read out of
///    `mod.rs` at compile time, so it cannot go stale.
/// 2. **Mechanical.** The named sibling does not itself pass the null
///    backend. This is what stops the list being a place to hide: an entry
///    added to dodge a real failure must name a sibling that *fails* the null
///    run, which means real coverage exists.
/// 3. **Near-mechanical.** The named `sharing_read_method` is one both tests
///    call. Intersecting over observation methods only —
///    [`SETUP_METHODS`] are excluded because 45 of 49 tests call `apply`.
///    **Limit: this proves the two tests read the same surface, not that they
///    cover the same property.** Two tests can both call `list` about
///    entirely different things.
/// 4. **Stated, not checked.** `asserts_absence_of` names the property. The
///    guard only requires that something is written there. **Limit: nothing
///    mechanical can verify it** — "presence of the thing this test asserts
///    the absence of" is not derivable from names or from source. The line
///    exists so a reader can judge an entry at a glance instead of opening
///    two tests and reconstructing what is at issue. The guard's job is to
///    make that human claim expensive to fake, not to eliminate it.
///
/// # Why not the two obvious shapes
///
/// "Zero must pass" is wrong the moment one legitimately null-tolerant test
/// exists — and two do — so the first person to meet it would relax it, and a
/// relaxed guard is worse than a correctly-scoped one. A *bare* allow-list is
/// a place to hide: an entry would be a label asserting "this one is fine"
/// with nothing enforcing it, which is scope-carried-by-a-label arriving
/// inside the guard built to catch it.
///
/// # Why this cannot quietly rot
///
/// [`NullBackend`] implements `Backend`. A fifteenth trait method breaks it as
/// a compile error, not as a silently narrower measurement.
/// `the_census_measures_every_test_the_suite_runs` compares the census list
/// against `run!` itself, name by name, so a test added to the suite and not
/// here is a red test that names the test it is missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_conformance_test_passes_against_a_backend_that_does_nothing() {
    let verdicts = census().await;
    let passing: Vec<String> = verdicts
        .iter()
        .filter(|(_, v)| matches!(v, Verdict::Passed))
        .map(|(n, _)| tidy(n))
        .collect();

    for (name, verdict) in verdicts {
        match verdict {
            Verdict::Passed => println!("PASS  {}", tidy(name)),
            Verdict::Failed(msg) => {
                println!(
                    "fail  {}  --  {}",
                    tidy(name),
                    msg.lines().next().unwrap_or("")
                );
            }
        }
    }
    println!("{} of {} passed", passing.len(), verdicts.len());

    let allowed: Vec<&str> = NULL_TOLERANT.iter().map(|t| t.test).collect();

    // The guard proper.
    let unexplained: Vec<&String> = passing
        .iter()
        .filter(|p| !allowed.contains(&p.as_str()))
        .collect();
    assert!(
        unexplained.is_empty(),
        "these conformance tests pass against a backend that does nothing, and are \
         therefore vacuous unless someone shows otherwise: {unexplained:?}. \
         Fix the test. Do not add it to NULL_TOLERANT unless it is absence-shaped \
         and a sibling covers the presence case — and then supply the sibling."
    );

    // No stale entries: a tolerance that has stopped being one is a claim
    // nobody is checking any more.
    for t in NULL_TOLERANT {
        assert!(
            passing.iter().any(|p| p == t.test),
            "NULL_TOLERANT lists {} but it no longer passes the null backend. \
             The test was strengthened; delete the entry.",
            t.test
        );
    }

    let suite = suite_list();
    for t in NULL_TOLERANT {
        // Check 1, mechanical.
        assert!(
            suite.contains(&t.presence_covered_by),
            "{}: names {} as covering the presence case, but no such test is in run!",
            t.test,
            t.presence_covered_by
        );

        // Check 2, mechanical.
        assert!(
            !passing.iter().any(|p| p == t.presence_covered_by),
            "{}: names {} as covering the presence case, but that sibling also passes \
             the null backend. Two vacuous tests do not make a covered property.",
            t.test,
            t.presence_covered_by
        );

        // Check 3, near-mechanical. Proves a shared read surface, not a
        // shared property.
        let mine = read_methods(t.test);
        let theirs = read_methods(t.presence_covered_by);
        assert!(
            mine.contains(&t.sharing_read_method) && theirs.contains(&t.sharing_read_method),
            "{}: claims to share Backend::{} with {}, but the observation methods are \
             {mine:?} and {theirs:?}. Setup methods ({SETUP_METHODS:?}) are excluded \
             on purpose.",
            t.test,
            t.sharing_read_method,
            t.presence_covered_by
        );

        // Check 4, stated only. The guard cannot judge this claim; it can
        // only require that the claim was made.
        assert!(
            !t.asserts_absence_of.trim().is_empty(),
            "{}: asserts_absence_of is empty. Say in one line what the test asserts \
             is absent, so a reader can judge the entry without opening two tests.",
            t.test
        );
    }
}

/// The census list must be the suite list. Otherwise a test added to `run!`
/// is never measured, and the guard above silently stops covering it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_census_measures_every_test_the_suite_runs() {
    let measured: Vec<String> = census().await.iter().map(|(n, _)| tidy(n)).collect();
    let suite = suite_list();
    assert_eq!(
        measured, suite,
        "the census list and run_conformance_suite's run! list have diverged. \
         Every test in run! must appear in the census!(..) list in this file, in \
         the same order."
    );
    assert_eq!(
        suite.len(),
        49,
        "the suite's size changed; update the census record"
    );
}
