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
  `items` with exactly one reader, `scope_embedder`; `search` filters through the `items`
  join and never reads them. Schema owner's call; the join cost is unmeasured.
- **`Protection::Protected { until }` is constructed nowhere in the suite**, so the
  `protected_until` column is written by no conformance test.
