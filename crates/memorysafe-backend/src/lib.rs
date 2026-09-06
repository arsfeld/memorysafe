//! The storage seam. One trait covering persistence and retrieval, because
//! pgvector searches inside the database and a separate index trait would
//! bake the SQLite shape into the interface.

pub mod aggregates;
pub mod conformance;
pub mod portability;
pub mod query;
pub mod write;

use memorysafe_core::{
    AuditFilter, AuditId, AuditRecord, CapacityState, Embedding, ItemId, MemoryItem, PurgeCascade,
    Scope, ScopeStats, ScoredCandidate, SubjectId, TenantId,
};
use thiserror::Error;

pub use aggregates::{
    AggregateKey, AuditAggregate, AuditAggregateFilter, SCORE_HISTOGRAM_BUCKETS,
    SCORE_HISTOGRAM_EDGES, SCORE_HISTOGRAM_VERSION,
};
pub use portability::{
    ExportRecord, ExportStream, ExportVector, FORMAT_VERSION, ImportReport, ImportStream,
    ScopeSelector,
};
pub use query::{CandidateQuery, HardFilters, MAX_PAGE_LIMIT, Page};
pub use write::{AppliedWrite, ItemWrite, MergeWrite, PurgeReport, WriteTransaction};

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("storage failure: {message} (retryable: {retryable})")]
    Storage { message: String, retryable: bool },
    #[error("item {0} not found")]
    ItemNotFound(ItemId),
    #[error("merge target {0} does not exist")]
    MergeTargetMissing(ItemId),
    #[error("idempotency key reused with a different payload")]
    IdempotencyConflict,
    #[error("query is invalid: {0}")]
    InvalidQuery(String),
    /// `WriteTransaction::is_valid` rejected the transaction — it both
    /// inserts and merges, or its scope-bearing fields disagree. `Backend::apply`
    /// returns this having written nothing.
    #[error("transaction is invalid: {0}")]
    InvalidTransaction(String),
    #[error("vector uses embedder {got}, scope uses {expected}")]
    EmbedderMismatch { got: String, expected: String },
    #[error("import stream is malformed: {0}")]
    MalformedImport(String),
}

/// # The echo rule: a persisted audit row keeps the id it was given
///
/// **Every method on this trait that persists an audit row persists the
/// `AuditId` it was handed, unchanged, and returns that same id.** Four
/// methods do: [`Backend::apply`] (`txn.audit.id`), [`Backend::record_recall`]
/// (`record.id`), [`Backend::import`] (each `ExportRecord::Audit`'s own id),
/// and [`Backend::purge_subject`] (the `audit` argument's id). Each states the
/// rule in one line and points here; this is where the reason lives.
///
/// **Why it is stated once, at the trait, rather than four times.** The damage
/// is not per-method and does not add up per-method: a table whose ids come
/// from three places is not three times worse than one whose ids come from
/// two — it is a table where "order by id" has no single meaning at all.
/// `AuditFilter::after` is a ULID cursor and `Backend::audit` pages by it, so
/// one minting path anywhere makes the whole log's order, and therefore its
/// pagination, unsound. Stating this per method invites closing three of four
/// paths and reading the fourth as unconstrained, which is exactly the state
/// this trait was in: `import`'s id provenance held only by implication from
/// the byte-exact preservation rule — a second rule true only by derivation,
/// with no test of its own and nothing to stop it silently ceasing to be
/// implied.
///
/// **The minting boundary.** The rule governs rows the backend is *handed*.
/// "Backends never mint ids" is an available reading of the sentence above and
/// it is the wrong one: a backend that *originates* a row — one no caller
/// supplied and no caller can name — mints that row's `AuditId` itself,
/// because there is no given id to echo. What it may never do is replace an id
/// it was given.
///
/// One conformance test per path pins this, all four in
/// `conformance::lifecycle`:
/// `apply_persists_the_audit_id_it_was_given`,
/// `record_recall_persists_the_audit_id_it_was_given`,
/// `import_preserves_every_audit_id`, and
/// `purge_subject_persists_the_record_it_was_given`.
#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    /// Ordered descending by `relevance`, ties broken by ascending `ItemId`.
    /// This must be a total order: `retrieve_candidates` truncates at
    /// `query.limit`, so tied relevance at the limit boundary means two
    /// backends that each conform to "descending by relevance" alone can
    /// return different *candidate sets* — not the same set in a different
    /// order. The policy then assesses different neighbours and can reach a
    /// different admit/merge/reject decision. Worse, `Reason` carries a
    /// `FeatureMap`, and those feature numbers are written into the audit
    /// record, so two backends would record different evidence for the same
    /// input — an auditor replaying a decision against the other backend
    /// could not reproduce it.
    ///
    /// Populates `ScoredCandidate::last_accessed_at` and `access_count` from
    /// each item's stored access statistics — the values `record_recall`
    /// maintains. An item never recalled comes back as `(None, 0)`, never
    /// `(Some(created_at), 0)`; see `ScoredCandidate::last_accessed_at` for why
    /// the two must stay distinguishable.
    /// `conformance::retrieval::recall_updates_access_statistics` enforces it,
    /// and enforces it on the `Option` rather than on the timestamp: `fx::item`
    /// pins `created_at` to `UNIX_EPOCH`, so a backend seeding
    /// `last_accessed_at` from `created_at` produces a *value* no fixture here
    /// can distinguish from a real one — `is_none()` is what separates them.
    async fn retrieve_candidates(
        &self,
        scope: &Scope,
        query: &CandidateQuery,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

    /// Ordered descending by `relevance`, ties broken by ascending `ItemId`
    /// — the same rule as `retrieve_candidates`, for the same reason:
    /// `neighbours` truncates too, at `k`, so tied similarity at the k-th
    /// position means two backends that each conform to "descending by
    /// relevance" alone can return different *neighbour sets* — not the
    /// same set in a different order. The tie-break must be applied before
    /// truncating at `k`, not after. Concretely, the policy's `best()`
    /// neighbour — the merge target — becomes nondeterministic on tied
    /// similarity if two backends break ties differently.
    ///
    /// Populates the access statistics on every returned candidate, on the
    /// same terms as `retrieve_candidates`.
    async fn neighbours(
        &self,
        scope: &Scope,
        embedding: &Embedding,
        k: usize,
    ) -> Result<Vec<ScoredCandidate>, BackendError>;

    async fn capacity_state(&self, scope: &Scope) -> Result<CapacityState, BackendError>;

    async fn scope_stats(&self, scope: &Scope) -> Result<ScopeStats, BackendError>;

    /// Persists `txn.audit` under the id it already carries and returns that
    /// id as `AppliedWrite::audit_id` — see the echo rule on this trait.
    ///
    /// **`apply` must reject a transaction `WriteTransaction::is_valid`
    /// rejects, with [`BackendError::InvalidTransaction`], having written
    /// nothing.** Validation is not optional and not advisory: `is_valid`
    /// exists because `scope`, the upserted item's own scope and
    /// `audit.scope` are three independently-settable public fields that a
    /// real backend reads for three different rows, so a transaction that
    /// disagrees with itself files a row, its vector and its audit trail under
    /// three different subjects. Rejecting *after* writing part of it is the
    /// same defect with an error attached.
    /// `conformance::atomicity::an_invalid_transaction_is_rejected_and_writes_nothing`
    /// enforces both halves.
    async fn apply(&self, txn: WriteTransaction) -> Result<AppliedWrite, BackendError>;

    /// Writes the recall's audit row **and**, in the same transaction, updates
    /// the access statistics of every item `record.items` references: each
    /// item's `access_count` increments by one and its `last_accessed_at`
    /// becomes `record.at`.
    ///
    /// `record.at` rather than a clock read, for the reason `AuditRecord::new`
    /// gives for `at` being supplied and never sampled: a replay has to
    /// produce comparable rows, and a statistic stamped from the wall clock
    /// would differ on every run.
    ///
    /// No separate trait method for this, and no separate write. `record`
    /// already carries exactly the ids that were recalled — that is what
    /// `ItemRef` is — so the increment rides the write that is happening
    /// anyway. A second method would let a backend audit a recall without
    /// counting it, or count it without auditing it, and the pair is the same
    /// fact.
    ///
    /// Only the referenced items are touched. A backend that bumps every item
    /// in the scope makes `last_accessed_at` mean "the scope was read", which
    /// is not what the replay quota needs to know.
    ///
    /// The row is persisted under `record.id` and that id is what comes back —
    /// see the echo rule on this trait.
    async fn record_recall(&self, record: AuditRecord) -> Result<AuditId, BackendError>;

    async fn get(&self, scope: &Scope, id: &ItemId) -> Result<Option<MemoryItem>, BackendError>;

    /// Ordered ascending by `created_at`, ties broken by ascending `id`
    /// (`id` is a ULID — itself a total, time-sortable order — so breaking
    /// ties by it never contradicts the primary sort). This must be a
    /// *total* order: ordering by timestamp alone is insufficient, since a
    /// bulk import can leave many rows with an identical `created_at`, and
    /// an unstable sort under `LIMIT`/`OFFSET` paging can then return the
    /// same row on two pages while silently dropping another. Every backend
    /// must use this same key and direction — SQLite paging ascending while
    /// Postgres paged descending would each "conform" to a total order
    /// stated without one, which is exactly the cross-backend drift this
    /// suite exists to prevent.
    async fn list(&self, scope: &Scope, page: &Page) -> Result<Vec<MemoryItem>, BackendError>;

    /// Ordered by `AuditId`, descending (newest first). The choice of key —
    /// `AuditId` rather than `at` — is `AuditFilter::after`'s rule; see its
    /// doc comment in `memorysafe-core` rather than restating the reasoning
    /// here.
    ///
    /// `filter.after`, when set, continues a previous page. Because the
    /// list is descending, "after" names a *position* in that returned
    /// order, not a point in time: the next page is restricted to
    /// `id < after` (strictly smaller), not `id > after` — the temporal
    /// reading would instead re-request rows already returned.
    /// `conformance::lifecycle::audit_pages_by_the_after_cursor_without_repeating_a_row`
    /// enforces this: it pages a four-row log at `limit: 2` and asserts
    /// strict descent, disjoint pages, a complete walk, and termination on a
    /// short page.
    ///
    /// `filter.since` and `filter.until` are both **inclusive** bounds: a
    /// record timestamped exactly at either edge matches.
    /// `conformance::lifecycle::audit_since_and_until_include_a_record_on_the_boundary`
    /// enforces this, by placing each bound exactly on the timestamp of the
    /// record that must be the corresponding extreme of the result.
    /// `audit_filter_narrows_by_event_and_time` still places its own bounds
    /// off every record's timestamp, on purpose: it tests the window without
    /// depending on the inclusivity choice, so the two tests fail for
    /// different reasons.
    ///
    /// **Truncation is detectable from the page size, so there is no
    /// `truncated` flag.** An implementation must return exactly
    /// `min(filter.limit, rows still matching after the cursor)` — never
    /// fewer for an internal batch size, a statement timeout, or a partial
    /// read, and never more. Given that, `returned.len() < filter.limit`
    /// means the log is exhausted, and a caller pages by re-issuing with
    /// `after` set to the last returned row's `AuditId` until a short page
    /// arrives. A flag on the result would be a second encoding of a fact the
    /// cursor already carries, and the two could disagree.
    ///
    /// This is the resolution of a `NOTE` that stood in
    /// `AuditFilter`: `limit` defaults to 100, so a compliance query built
    /// from `AuditFilter::default()` stops at 100 rows. It still does — but
    /// the caller can now tell, because a full page means "ask again", not
    /// "that was everything".
    /// `conformance::lifecycle::audit_returns_min_of_the_limit_and_the_rows_that_remain`
    /// pins both arguments of the `min` in one test — a filter matching fewer
    /// rows than the limit returns all of them, and one matching more
    /// truncates to exactly the limit.
    async fn audit(
        &self,
        scope: &Scope,
        filter: &AuditFilter,
    ) -> Result<Vec<AuditRecord>, BackendError>;

    /// Erases a subject within a tenant. This is the *subject* erasure API,
    /// and it is the one with a conformance test; tenant erasure is an
    /// out-of-band operator action with no method here — see the
    /// `aggregates` module doc for why, and for what survives.
    ///
    /// **What is deleted, under either `cascade`:** the subject's items, their
    /// vectors, their idempotency records, and their capacity accounting, in
    /// every namespace the subject owns. Audit *aggregates* are never deleted
    /// — they name no subject and no namespace, so there is nothing in them
    /// for this call to erase; `audit_aggregates` carries that argument.
    ///
    /// **What `cascade` decides:** the subject's audit detail rows.
    /// [`PurgeCascade::Cascade`] deletes them; [`PurgeCascade::Preserve`]
    /// keeps them. Bodies were never in them, so a preserved row is ids,
    /// digests, and feature numbers.
    ///
    /// **`audit` is inserted either way, delete before insert, in one
    /// transaction.** The record is the caller's `SubjectPurged` row and it is
    /// written after the deletes, inside the same transaction, under the id it
    /// carries (the echo rule on this trait). Three properties, each of which
    /// a plausible implementation gets wrong on its own:
    ///
    /// - *Delete before insert.* A backend that inserted first and then swept
    ///   the subject's audit rows under `Cascade` would delete its own
    ///   `SubjectPurged` record — the purge eats the only evidence it ran.
    /// - *One transaction.* A crash between the deletes and the insert must
    ///   not be able to leave an erasure with no record of itself, or (under
    ///   `Preserve`) a half-erased subject.
    /// - *Either way.* `Preserve` does not mean "write nothing"; it means the
    ///   *pre-existing* rows survive. The `SubjectPurged` row is written under
    ///   both.
    ///
    /// The record's `scope` names the subject being purged; a subject spans
    /// namespaces, so which of its namespaces the caller files the row under
    /// is the caller's choice and the backend stores it as given, exactly as
    /// `apply` stores `txn.audit.scope`. A backend may rely on
    /// `audit.scope.tenant == *tenant` and `audit.scope.subject == *subject`
    /// and is not required to check it; it must never rewrite either, for the
    /// reason `import` gives — a rewritten audit row is a forged one.
    ///
    /// **The engine does not read or replay audit rows around this call.** An
    /// earlier engine draft implemented `Preserve` by reading the subject's
    /// audit rows with a hard-coded `AuditFilter { limit: 100_000, .. }`,
    /// calling a cascade-only `purge_subject`, and re-inserting each row
    /// through `record_recall`. Every part of that is now impossible by
    /// signature, and each part was a defect: `record_recall` also updates
    /// access statistics, so replaying `Admitted`/`Merged`/`Forgotten` rows
    /// through it moved live items' `last_accessed_at` *backwards* to a
    /// historical `record.at` during an erasure, double-counted the
    /// aggregates that ride the audit write, spanned two transactions (a crash
    /// between them lost every preserved row), and silently truncated at
    /// 100_000 while reporting `audit_rows_preserved` as complete.
    ///
    /// **Accounting** is an equation, not a convention — see [`PurgeReport`].
    async fn purge_subject(
        &self,
        tenant: &TenantId,
        subject: &SubjectId,
        cascade: PurgeCascade,
        audit: AuditRecord,
    ) -> Result<PurgeReport, BackendError>;

    /// Every audit aggregate held for `tenant` that matches `filter`.
    ///
    /// Mirrors `audit`'s shape rather than inventing a second idiom for a
    /// bounded, resumable read: `filter` carries `since`/`until` **day**
    /// bounds, an optional `policy` narrowing, a `limit`, and an `after`
    /// cursor. The feature's primary query is a day range against one policy
    /// version — "how did behaviour change across policy 1.4" — which the
    /// original `audit_aggregates(&self, tenant)`, unbounded, unpaginated and
    /// unfilterable, could not express at all.
    ///
    /// Aggregates are keyed by tenant + policy version + event class + day
    /// bucket, and by nothing finer — see the `aggregates` module doc, which
    /// carries the whole argument, the cost it accepts, and the residual it
    /// leaves. The consequence relevant to an implementer is this: **a
    /// cascading `purge_subject` must not delete aggregate rows.** They name
    /// no subject and no namespace, so there is nothing in them for the purge
    /// to be erasing; a backend that stores them in, or cascades them from,
    /// the audit detail table destroys the one artifact that was designed to
    /// outlive the detail.
    ///
    /// **Ordering.** Rows are ordered ascending by the full key, which is
    /// **five comparisons in this order** — enumerated in full rather than
    /// summarised, because "the full key" previously named three of them and a
    /// Postgres implementer building from this paragraph would have produced a
    /// different total order:
    ///
    /// 1. `day`, ascending.
    /// 2. `policy`, with `None` before every `Some`.
    /// 3. the policy's `name`.
    /// 4. the policy's `version`.
    /// 5. the event's serialised snake_case name.
    ///
    /// and then `tenant` as a sixth, which never discriminates in practice —
    /// `audit_aggregates` takes the tenant as a parameter, so every row in one
    /// result set shares it — but is part of the comparison because [`AggregateKey`]'s
    /// `Ord` must agree with its `Eq`, and two keys differing only in tenant
    /// are not equal. It is listed so an implementer reading this paragraph
    /// and an implementer reading `AggregateKey::cmp` write the same code.
    ///
    /// **Name and version compare separately, not as `name@version`.** The
    /// render is `Display`'s job and it is not injective: `PolicyId` constrains
    /// neither field, so `("a@b", "c")` and `("a", "b@c")` both render
    /// `"a@b@c"`. An ordering built on the render returns `Equal` for two keys
    /// `==` calls different — and a backend storing the policy as one rendered
    /// column does worse than misorder them, it **merges their counts into one
    /// aggregate row**. Store and compare the two parts.
    ///
    /// Stating the order costs nothing now and stops the same cross-backend
    /// drift `list` and `audit` each had to have pinned down after the fact.
    /// It has an executable referent — [`AggregateKey`]'s hand-written `Ord`,
    /// which encodes exactly this sequence and deliberately not the struct's
    /// field order — and a conformance test,
    /// `conformance::lifecycle::audit_aggregates_page_in_the_documented_order`.
    ///
    /// **The event sorts by its serialised name — `AuditEvent::as_str` — and
    /// never by a stored ordinal.** `AuditEvent` has no `Ord` on purpose: a
    /// derive would give declaration order, where `rejected` is second and
    /// `exported` sixth, against an alphabetical tenth and second. This one is
    /// not a dialect split: it divides any two backends that store the event
    /// differently, and storing an enum as an integer is an established
    /// pattern in this codebase (`SensitivityLevel::ordinal`), so a backend
    /// author following the local convention lands on the wrong order by doing
    /// the idiomatic thing.
    ///
    /// # Collation and null placement are stated, never defaulted
    ///
    /// **Every ordering over a text column must state its collation
    /// explicitly; every ordering over a nullable column must state null
    /// placement explicitly. Neither may rely on a dialect default.** This
    /// binds every backend, not only the one whose defaults happen to
    /// disagree — a rule written as "dialect X defaults the wrong way" rests on
    /// a remembered default, and if the recollection is wrong it is wrong in
    /// both directions at once.
    ///
    /// The rule is deliberately **semantic, not syntactic**: each dialect
    /// supplies its own spelling, and this trait does not know them. Byte order
    /// is the required semantics for text here, because [`AggregateKey`]'s `Ord`
    /// compares through Rust's `str: Ord` and the conformance sweep measures a
    /// backend against that `Ord` — so a backend sorting under any other
    /// collation disagrees with the type it is being compared to. It is forced,
    /// not preferred, and must not be relaxed to match a customer's expected
    /// sort order.
    ///
    /// The columns this reaches, for the key above: the policy's name, the
    /// policy's version, the event name, and the tenant — every text component,
    /// not only the ones a test can currently reach. The nullable one is the
    /// policy, whose `None` must sort before every `Some`.
    ///
    /// **Each backend must carry a test named
    /// `ordering_sql_states_collation_and_null_placement`**, asserting that the
    /// SQL it builds for this ordering states both. Two things about that test,
    /// both of which stop it being read as more than it is:
    ///
    /// - **It checks a different property from the conformance sweep, not a
    ///   better one.** The sweep checks the resulting *order*; a backend
    ///   relying on a default that happens to agree produces the right order
    ///   and passes. This checks *explicitness*, which is what survives a
    ///   deployment moving to a different locale or a different engine.
    /// - **The canonical name is the propagation check; the test's content is
    ///   the enforcement check.** Grepping the name across backend crates
    ///   answers "did this backend write one", which is why the name is fixed
    ///   rather than left to judgement. It does not answer "does it assert
    ///   anything" — an empty function satisfies the grep. Do not read a clean
    ///   grep as a clean audit.
    ///
    /// The test must derive the SQL from whatever builds the query rather than
    /// asserting against a copied literal: a literal is a second copy of the
    /// query and drifts from it, which is the objection that produced
    /// `AggregateKey`'s `Ord` in the first place.
    ///
    /// **The cursor.** `filter.after`, when set, continues a previous page:
    /// the next page is restricted to keys strictly greater than it in the
    /// order above. `AggregateKey` is a value comparable without existing —
    /// it names a coordinate in the key space, not a pointer to a stored row.
    /// That is precisely what makes it safe as a cursor here: it resumes
    /// correctly even when the row it names has since expired under
    /// `AuditRetention::aggregate`, whereas a row-id cursor would name
    /// nothing once its row was gone and would make the log look exhausted
    /// for a reason unrelated to the caller's query.
    /// `conformance::lifecycle::audit_aggregates_resume_from_a_cursor_that_names_no_stored_row`
    /// enforces that, with a cursor placed between two stored rows and named
    /// by no row at all — a case no paging sweep can generate, since every
    /// cursor a sweep produces came from a row the backend just returned.
    ///
    /// **Truncation is detectable from the page size, exactly as `audit`
    /// documents — there is no `truncated` flag.** An implementation must
    /// return exactly `min(filter.limit, rows still matching after the
    /// cursor)` — never fewer for an internal batch size and never more.
    /// `returned.len() < filter.limit` means exhausted.
    ///
    /// Rows are produced by the write paths (Tasks 20 and 23) and expired by
    /// retention (Plan 1's retention-profiles task). Neither is this method's business.
    async fn audit_aggregates(
        &self,
        tenant: &TenantId,
        filter: &AuditAggregateFilter,
    ) -> Result<Vec<AuditAggregate>, BackendError>;

    /// `Header` first, then `Item`s ascending by `ItemId`, then `Audit`
    /// rows ascending by `AuditId`. `ExportStream` carries no relevance and
    /// no single id type, so neither the retrieval tie-break nor `list`'s
    /// rule applies here — the key differs per record kind instead. Two
    /// backends exporting the same tenant with a different (or absent)
    /// order produce byte-different artifacts for identical data, so
    /// checksums do not match and a customer verifying a migration cannot.
    /// `export_import_round_trips_exactly` still does not observe the stream —
    /// it compares `list` output after re-sorting both sides by `ItemId` —
    /// but `conformance::lifecycle::export_orders_the_stream_by_kind_then_by_id`
    /// now walks the records themselves, over a corpus whose insertion order
    /// deliberately disagrees with its id order so that a backend emitting
    /// rows in storage order fails.
    ///
    /// `sel`'s optional `subject` and `namespace` must narrow the result;
    /// `conformance::lifecycle::export_narrows_to_the_selectors_subject_and_namespace`
    /// enforces that, asserting both that the matching records are present and
    /// that the others are absent.
    async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, BackendError>;

    /// Imports `stream` into `destination`. The tenant is a parameter, not a
    /// property of the payload: an authorising layer must be able to decide
    /// where an import lands without parsing the blob, and a later plan puts
    /// tenant-scoped API keys in front of this engine.
    ///
    /// **The disagreement rule.** `destination` is compared against **every
    /// record's own tenant**, one by one. No record's tenant is authority for
    /// any other record's. A record whose tenant disagrees **rejects the
    /// whole import** with `MalformedImport` — it is never silently
    /// retargeted, because retargeting only moves the differential from "the
    /// caller does not know where this landed" to "the caller does not know
    /// what this became". The payload's `subject` and `namespace` are
    /// preserved exactly as written; only the tenant is checked, and it is
    /// checked rather than assigned.
    ///
    /// **Atomicity.** `import` is all-or-nothing: either every record in
    /// `stream` is applied or none are. A conformance test already requires
    /// this — no partial import survives a rejected stream — but until now
    /// nothing said so; the trait doc stated only that a disagreement
    /// "rejects the whole import", which reads as a property of that one
    /// rejection path rather than of `import` itself. This paragraph is the
    /// general statement: a validation failure (a disagreeing tenant, a
    /// missing or unsupported `Header`, a malformed record) must leave the
    /// destination exactly as it was before the call. What this does **not**
    /// cover is a mid-stream storage error on an otherwise-valid stream —
    /// whether such a backend leaves partial writes behind is
    /// under-determined by this contract; it must not silently report
    /// success over a partial result, but the specific guarantee in that
    /// case is out of scope here.
    ///
    /// Because every record is compared against `destination`, records
    /// cannot disagree with *each other* either — a separate "the stream may
    /// not span tenants" check is strictly implied by this one and must not
    /// be written. A second rule that is true only by implication has no test
    /// of its own, cannot fail today, and silently stops being implied the
    /// day someone weakens the first.
    ///
    /// **Audit rows follow from the same comparison, not from a rule of their
    /// own.** `ExportRecord::Audit` carries an `AuditRecord` whose `scope`
    /// has a tenant. Same tenant as `destination` → the row is preserved
    /// byte-exact. Different → the import is rejected, exactly as for an
    /// item. A scope is never rewritten to make a row fit: a rewritten audit
    /// row is a forged one.
    ///
    /// The row's `AuditId` is part of "byte-exact", but it is no longer left
    /// to be *derived* from it: the echo rule on this trait states it
    /// directly, alongside the three other paths that persist a row they were
    /// handed. A rule true only by implication has no test of its own and
    /// stops being implied the day the rule it hangs off is weakened.
    ///
    /// **Why this is not a `Scope` or a `ScopeSelector`.** Tenant is the
    /// authorisation unit and the isolation unit, and taking a `TenantId`
    /// makes the disagreement rule total. A `Scope` would force an answer to
    /// "what happens when only the subject differs?", reintroducing the
    /// ambiguity the parameter exists to remove — in a path where the wrong
    /// answer is a cross-subject write. A `ScopeSelector`'s optional fields
    /// would make "unset" mean something, and every available meaning for it
    /// is a silent retarget.
    ///
    /// **Why this is not symmetric with `export`, and must not be
    /// harmonised.** `export` takes a `ScopeSelector` because it *narrows a
    /// read* within one tenant; `import` takes a `TenantId` because it
    /// *authorises a write* into one. Different questions, different types.
    /// Harmonising in either direction reintroduces a defect: giving
    /// `export` a bare tenant removes the subject/namespace narrowing a
    /// partial export needs, and giving `import` a selector puts the two
    /// optional fields back.
    ///
    /// Order-tolerant: records may arrive in any order. This is deliberate
    /// — requiring exactly one `Header` would reject the concatenation of
    /// two exports, which migration tooling actually does, so the rule is
    /// "at least one".
    ///
    /// At least one `Header` must be present, and every `Header` present
    /// must carry `portability::FORMAT_VERSION`. **This is a contract Task
    /// 24 must implement — today's code does not enforce it.** The draft
    /// `import` checks `format_version` only inside the `Header` match arm,
    /// so a stream that omits a `Header` entirely never reaches that check
    /// and is accepted with no version check at all, making the format
    /// version optional by omission.
    ///
    /// A stream of a `Header` and nothing else is **valid** and imports
    /// nothing: `export` of an empty tenant produces exactly that, and the
    /// round trip has to survive it. With the destination supplied as a
    /// parameter there is no longer anything to derive from the first item,
    /// so there is no reason left to reject it.
    async fn import(
        &self,
        destination: &TenantId,
        stream: ImportStream,
    ) -> Result<ImportReport, BackendError>;

    /// Set a namespace's budget. Used by the conformance suite and by admin APIs.
    async fn set_budget(
        &self,
        scope: &Scope,
        budget: memorysafe_core::Budget,
    ) -> Result<(), BackendError>;
}
