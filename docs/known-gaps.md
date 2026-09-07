# Known gaps at the conformance freeze

State at `bc3de76`, after the SQLite backend passed the fifty-test suite and the suite was
frozen at `2d5701c`. Every line was verified by running something, not by reading.

**Why this file exists.** These were tracked in a cross-session conversation and a
git-ignored workspace. Both are `/tmp` with extra steps — the same failure as the audit
tools recovered from a dead session this week. **A number that lives in a conversation dies
with it.**

## The freeze, and what it costs to change

`git diff crates/memorysafe-backend/src/conformance/` between `520fda7` (where the SQLite
crate began) and `2d5701c` is **two files, zero non-comment lines** — both documentation.
The backend was implemented against a stationary target, which is what makes "it passes the
suite" mean anything. Checkable by anyone holding the two SHAs.

The suite is not sealed: *any change to it is a change to the `Backend` contract*, so
adding a test after the freeze is a deliberate contract revision plus a coordination round
with the Postgres plan. Expensive, not impossible.

## Gaps that the freeze locks in — a contract exists, no conformance test does

Ranked. A crate-local test proves this backend is right today; only a conformance test makes
it a requirement for every future backend.

1. **Cross-subject idempotency keys.** Both idempotency conformance tests use the single
   scope `("t","s","n")`; nothing anywhere pairs two subjects with one key. A backend keyed
   on the bare key passes the suite. Guarded here by
   `schema::tests::two_subjects_may_use_the_same_idempotency_key`.
2. **`AuditFilter::{item, subject, namespace}`** — set by no test, and **the contract is
   absent too**: `Backend::audit`'s doc says nothing about three fields the struct documents
   as load-bearing.
3. **`HardFilters::{occurred_after, occurred_before}`** — zero occurrences of either
   identifier anywhere in the suite. Inclusivity now documented; untested.
4. **`retrieve_candidates`' relevance tie-break** and **`neighbours`' access statistics** —
   both covered crate-locally only.

## From the freeze review — six the frozen suite cannot see

Each has a **measured distinguishing input**, so none is an equivalent mutant. Ranked; the
first two are cross-backend compatibility properties, which is the single class this suite
exists to protect.

1. **`import` never checks `Header`'s `format_version`, and the suite cannot tell.** Both
   halves are unpinned — presence *and* version. Every conformance import receives a header
   built with the current `FORMAT_VERSION`, so a backend that ignores the field passes.
   `FORMAT_VERSION`'s own doc calls it the mechanism against "a SQLite export that Postgres
   refuses"; that mechanism is currently unenforced by the contract.
2. **The exported vector's `scale` is unchecked.** Forcing every `scale` to a constant
   survives. `QuantizedVector::dot` multiplies by both operands' scales, so a constant
   destroys relative weighting between vectors with different `max_abs` — but
   `export_import_round_trips_exactly` compares only the ranked *ids* from `neighbours`, and
   **cosine ranking is invariant under per-vector positive scaling**, so a ranking assertion
   structurally cannot pin it. `embedder` and `dim` are pinned; `scale` is the one field of
   `ExportVector` not reconstructible from the bytes and the one nothing checks.
3. **`import` does no capacity accounting, and nothing notices.** Removing it entirely
   survives. Measured: 3 items import as `used_items=3, used_bytes=287`; without the
   accounting, 0/0. `capacity::adjust` is delta-based and never self-heals, so a migrated
   tenant's budget stays permanently under-counted and unenforced. Four capacity conformance
   tests exist; none reaches `import`.
4. **`ImportReport::items_imported` and `vectors_imported` are interchangeable.** Every
   conformance import feeds a corpus where the two are equal; the one stream with unequal
   counts asserts only `audit_imported`. Same shape as `PurgeReport`'s own swap survivor —
   two report types, one blind spot.
5. **No conformance test reads any of `AuditAggregate`'s histogram fields.** Swapping
   `value_histogram` and `fragility_histogram` on read survives, as does taking
   `histogram_version` from a neighbouring column. The suite never sets an `Assessment`, so
   both histograms are all-zeros there — empty-set vacuity again. `histogram_version` exists
   specifically to make a `SCORE_HISTOGRAM_EDGES` change detectable.
6. **`AuditAggregateFilter::since` and `until` are interchangeable.** The only test that
   sets them uses `since: Some(1), until: Some(1)` — a degenerate window where the two
   orders are equivalent. Chosen deliberately to pin *inclusivity*; the side effect is that
   the parameter assignment is unpinned. `since: Some(0), until: Some(2)` gives 7 rows
   against 0.

**And the deserialiser fix is crate-local only.** The three arms are killed by a test in
`portability`, not by the suite: `human` deleted and `Protected{until}` shifted both still
survive a conformance-only run. A future backend dropping either passes the frozen suite.

## Surviving mutants, four kinds

Run `cargo mutants`; the baseline and the timeout caveat are in `scripts/README.md`.

**Closed after the freeze review** — `export` dropping audit for item-less scopes
(Critical), and all four transactions now opening `Immediate` rather than relying on the
position of one call.

**Closed since the freeze** — `items.rs`'s `"session"`/`"tool"`/`"pinned"` arms (the enum
readers now error rather than falling back, so there is no fallback value for a fixture to
coincide with); `apply`'s `size > 0` guard (closed as a side effect of moving
`evicted.push` inside it — the guard acquired an observable consequence).

**Masked by a working first line — allowlist, with procedure.** `retrieve::passes -> true`
and its four boundary mutants; `vectors::delete -> Ok(())`. Not excuses: to mutation-test
`filter_sql`, **disable `passes` first** — with it enabled these cannot fail. See
`.cargo/mutants.toml`.

**Equivalent under reachable states.** `neighbours`' `||` → `&&`: the test embedder's id is
`format!("deterministic-{dim}")`, so the guard's two operands are perfectly correlated for
every test using it, and `vectors::insert` writes both fields from one `QuantizedVector` —
the backend cannot produce a row where they disagree. `items::merge`'s `UPDATE` scope
predicate: `items.id` is `TEXT PRIMARY KEY` and `get` already refused an out-of-scope target.

**Open.**

- **Six mutants, one missing test.** `keyword.rs`'s bm25 squash and `retrieve.rs`'s
  `VECTOR_WEIGHT` blend are both unpinned; the keyword mutants are *monotone*, so ranking is
  unchanged and magnitude is observable only through the fusion, whose arithmetic is also
  unpinned. One test — an item scoring high on keyword and low on vector, against its
  mirror — kills all six.
- **`aggregates::increment`'s width guard.** `||` → `&&` survives because
  `a_histogram_of_the_wrong_width_…` seeds **both** histograms wrong, so `&&` fires too.
  Seed one correct.
- **`portability::export`'s `AuditFilter.limit`** is droppable with nothing noticing —
  nothing pins how many audit rows an export returns.
- **`SqliteBackend::forget_tenant`** — `pub`, zero callers, body replaceable with `()`.
  Documented as the operator half of tenant deletion. Decide: test it (delete the tenant's
  files, then show a cached handle would have served stale reads) or remove it.
- **Two `TIMEOUT`s, not kills** — `keyword.rs`'s `/` → `*` and `scope_embedder`'s constant
  return. Unresolved verdicts caused by the harness, not the code; checked that neither is a
  real `dim`-0 hang.
- **Five filter boundaries.** `filter_sql_narrows_every_dimension_under_limit_pressure`
  pins each predicate's *existence* by count under limit pressure, which is structurally
  unable to catch an off-by-one — a boundary error returns approximately the right count.
  Only `sensitivity` has an exactness test; `occurred_after >=` → `>` and
  `occurred_before <=` → `<` both survive.

## Reading a `cargo test` run: headers must equal results

**A run with zero `test result: FAILED` lines is not necessarily a green run.** On some
toolchains the targets that link bundled SQLite fail to *link* rather than to test. A
target that never links never runs, so it prints no `test result:` line at all — and a
grep for `FAILED`, or an eye scanning for red, reads that as success. Three people have
now rediscovered this the hard way.

The detection rule is machine-independent, and does not require knowing anything about
what went wrong:

```
cargo test --workspace --all-features --no-fail-fast > /tmp/tf.log 2>&1
grep -cE '^\s*Running |^\s*Doc-tests ' /tmp/tf.log   # headers: targets cargo started
grep -cE '^test result:' /tmp/tf.log                 # results: targets that finished
grep -cE '^test result: FAILED' /tmp/tf.log          # failures
```

**A run is green only if `headers == results` AND failures is 0.** The first equality is
what proves nothing was silently truncated; the second is the ordinary check. Report both
numbers, not just the second. At the time of writing the workspace has 45 of each.

If the two disagree, the missing targets are named by the `Running` lines with no
`test result:` after them, and the cause is usually a link error higher up the log. On a
Nix-based setup the fix is generally to put the toolchain's `libstdc++` on
`LD_LIBRARY_PATH` for the run; the specific store path is machine-local and deliberately
not written down here, because a pinned path rots and a rotted path is worse than none.
Find yours rather than copying someone else's.

## One process note, because it cost a near-miss at the freeze

**The highest-risk moment for a retired pattern is inside the fix for a different one.**

The fix for C1 needed to rebuild a `Scope` from stored columns, and its first draft was
`Scope::new(...).expect("stored scopes are valid")` — the seventh instance of a pattern six
of which had been removed from this crate an hour earlier, for poisoning a tenant's
connection mutex on one corrupt row. Caught before commit.

Not carelessness. **A fix is written in the frame of the thing it fixes**, and that frame
does not include what you were doing before it. The retired pattern is the locally obvious
way to write the line, and the reason it was retired lives in a different part of the file
and a different part of your attention.

No construction retires this one — it is a judgment about your own recent history, which is
the category that cannot be made unrepresentable. The cheap mitigation is a grep of the diff
for the pattern you most recently removed, before committing a fix for anything else.

## Elsewhere

- **Every transaction is `DEFERRED`.** `apply` now opens with a write (`capacity::ensure_row`),
  so the `SQLITE_BUSY_SNAPSHOT` exposure is closed on that path — **by ordering, which no
  test pins**. `TransactionBehavior::Immediate` would make it structural.
- **The `vectors` table stores scope twice.** `subject`/`namespace` are duplicated from
  `items`, and they now have **three** readers, not the one this entry used to claim:
  `vectors::scope_embedder` (`SELECT embedder, dim FROM vectors WHERE subject=?1 AND
  namespace=?2`), the newly-scoped `vectors::delete` (`item_id AND subject AND namespace`),
  and `purge::subject`'s `DELETE FROM vectors WHERE subject = ?1`. `vectors::search` is
  still not among them — it filters through the `items` join and never reads these columns,
  which is why no recall leak or sensitivity-ceiling consequence follows from a divergent
  row. Nothing in the schema ties either column to the referenced item's scope
  (`vectors.item_id` has a foreign key; `subject`/`namespace` are plain `TEXT NOT NULL`),
  and `vectors::insert`'s `ON CONFLICT(item_id) DO UPDATE SET` list omits both, so a
  divergence cannot self-heal. Every call site passes the item's own scope, so the property
  holds today **by convention, not by construction**; it is now pinned by
  `tests::every_vector_rows_scope_columns_agree_with_the_item_it_references` in
  `memorysafe-backend-sqlite`, whose doc comment carries the full consequence list. Schema
  owner's call; the join cost is unmeasured.
- **`Protection::Protected { until }` is constructed nowhere in the suite**, so the
  `protected_until` column is written by no conformance test.

## Carried out of Plan 1's final review — triaged, not fixed

The whole-branch review of Tasks 31-41 raised three Critical and eleven Important findings.
Everything merge-blocking was fixed on that branch. What follows was triaged as
fix-after-merge and is recorded here because the review that decided it is not part of the
repository.

**One finding was withdrawn rather than fixed, and the reason is worth keeping.** The review
graded as Critical that `ProptestConfig::with_cases(24)` in `memorysafe-engine`'s invariants
overrides CI's `PROPTEST_CASES: 64`, so the gate asserted a guarantee it never checked. That is
false for the resolved proptest version: `proptest!` passes the supplied config through
`contextualize_config`, which overwrites `cases` from the environment. Confirmed black-box —
one case runs the invariants in 0.2s, sixty-four in 7.4s, and a 35x spread is impossible if the
literal won. The review's own empirical check printed `Config::with_cases(24).cases` and read
`24`, which is true and is not the question: it measured the constructor, not the effective
config the runner uses. Do not "fix" this without re-measuring end to end.

### The next backend contract batch — one trait revision, not four

These four all block on the same change and should land together, since each separately would
amend the frozen conformance suite:

- **`Backend::list` must carry access statistics.** `gather::admit_context` and `maintain` hand
  the policy `value: 0.5`, `fragility: 0.5`, `last_accessed_at: None`, `access_count: 0` for
  every candidate, because `list` returns bare `MemoryItem`s. `eviction::cost` is
  `value * fragility`, so every candidate ties at `0.25` and the stable sort leaves `list`'s
  order — **"evict the lowest value-weighted retention cost" is, today, "evict the oldest".**
  The byte-budget reclaim and the maintenance job are both wired to a scorer that cannot
  discriminate. Honest and deterministic, but it must be named work before anything here is
  called production-ready.
- **A namespace query.** `mutate::namespaces_of` calls `Backend::export` to pull a subject's
  entire corpus, bodies included, into memory purely to learn a list of namespace names — on
  the erasure path.
- **A pending-only read.** `HardFilters::exclude_pending_embedding` exists in the exclusion
  direction only, so `backfill_embeddings` must page an entire scope to find the stragglers.
- **`AuditFilter`'s `item`, `subject` and `namespace` fields.** The SQLite backend silently
  ignores all three. `remember`'s idempotency replay builds a filter with `item: Some(id)` and
  gets the newest hundred rows in the whole scope instead; the lookup still succeeds because it
  searches by audit id, but it degrades to the approximation path far more often than its doc
  used to admit. No conformance test pins any of the three, so Plan 2's backend may implement or
  ignore them and pass either way.

### Before Plan 3 exposes any of this over HTTP

- **`Engine::import` writes caller-supplied audit rows verbatim.** The import path re-assesses
  items — sensitivity raised, `Pinned` stripped, `Protected.until` clamped — but passes header
  and audit records through untouched. A crafted stream can inject arbitrary audit history into
  the destination tenant, including a fabricated `SubjectPurged` row manufacturing evidence of
  an erasure that never happened, and `Reason::detail` is free text that routes an item body
  into an audit row. The import now records *itself*, which was the merge-blocking half; the
  trust question is this one and it is not closed.
- **`Engine::review` and `Engine::export` are unaudited, unfiltered full-corpus reads.** Neither
  applies a `sensitivity_ceiling` and neither writes an audit record, while `recall` audits even
  when it returns nothing. Deferring these was accepted *on the condition* that Plan 3 expose
  neither without a ceiling and a record. That condition is the reason they are not fixed.
- **`forget` writes one audit row naming every item it erased.** Correct, and the `items` column
  grows with the selector's reach. It wants a bound.
- **Two audit records cannot carry their own counts.** `Imported` and `PolicyChanged` name their
  scope but not their record counts, because `AuditRecord`'s only structured numeric fields are
  `decision` and `assessment`, and both feed `audit_aggregates` buckets that counts would
  corrupt. The real fix is a field on `AuditRecord`.

### Smaller, and safe to leave

- **`AuditRetention::detail` and `::aggregate` are enforced by nothing.** Only `purge_cascade`
  is read; no audit row expires on a schedule. Documented on the type rather than implemented.
- **The sixth invariant does not run under the `invariants` CI job.** Vector/item scope
  consistency lives in `memorysafe-backend-sqlite`'s unit tests, because neither of its
  directions is observable through the `Backend` trait. It therefore gets no release mode and no
  proptest cases. If the plan means six invariants, the job should name it.
- **`memorysafe-backend-sqlite/src/lib.rs` is ~1600 lines, over two-thirds of it `mod tests`.**
  The location is correct — those tests need the private `tenants` field — so the fix is a
  `#[cfg(test)] #[path = "tests.rs"] mod tests;` split, not a relocation.
- **`SCHEMA_VERSION` is gated with no migration path**, and `items::list` sorts on `created_at`
  with no index containing it, so every paged sweep sorts the whole scope.

### A pattern, recorded because it cost more than any single bug

Six times across these tasks, someone made a correct measurement of the wrong thing and reported
it as fact: a test suite counted without `--all-features`; a caller list grepped in one directory
and claimed for the workspace; mutation runs scoped to one target and reported as workspace
uniqueness; a proptest constructor read instead of the runner's effective config; and a stale
count replaced by a mechanical grep that was itself anchored to a receiver rustfmt had split
across lines. None was a careless error and every author was checking something real.

The rule that came out of it: **a claim's scope and its measurement's scope must match, and the
measurement is the half that needs verifying.** Its corollary for this repository: a counted
list in a doc comment is a claim nothing checks, which silently falsifies whenever anyone adds
to the thing it counts. Prefer a pointer to a canonical list, or a count backed by a literal in
the same file where the compiler can see it.
