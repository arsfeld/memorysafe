# Known gaps at the conformance freeze

State at `6e75fe4`, after the SQLite backend passed the fifty-test suite and the suite was
frozen at `8187bd9` — both tree claims, not diffs: `6e75fe4`'s own diff touches only
`scripts/README.md`, unrelated to either claim; check the state instead with
`git show <sha>:crates/memorysafe-backend-sqlite/tests/conformance.rs`, whose doc comment reads
"the suite is frozen as of this task" at both commits. Every line was verified by running
something, not by reading.

**Why this file exists.** These were tracked in a cross-session conversation and a
git-ignored workspace. Both are `/tmp` with extra steps — the same failure as the audit
tools recovered from a dead session this week. **A number that lives in a conversation dies
with it.**

## A mark this file needs on itself: measured, inspected, or relayed

This file opens by claiming *every line was verified by running something, not by reading* —
that standard is **measured**, and it held for every measured line. It did not hold for one:
"Elsewhere"'s claim that the `vectors` table's `subject`/`namespace` columns have "exactly one
reader, `scope_embedder`." Nobody ran anything to produce that count; someone read the code
and counted. Nothing on the page distinguished it from the measured lines around it, so a
later reader took it as established and built an analysis on it — reasonably, since nothing
said not to. (The count is corrected under "Elsewhere" below; why it happened is in "An
instrument that produces a wrong answer," below.)

**The remedy, and it costs one word per claim:**

- **measured** — a command was run and its output read. Give the command where it's short.
- **inspected** — code was read and counted, traced, or concluded. Nothing was run.
- **relayed** — someone else reported it, unconfirmed independently.

They decay differently, which is the whole reason to keep them apart. A **measured** number
goes *stale* — true when taken, overtaken by a later commit; it has a commit it was true at,
and a reader can re-run it. An **inspected** count can be *wrong the day it is written*:
"exactly one reader" was wrong from the start, since `purge::subject`'s
`DELETE FROM vectors WHERE subject = ?1` predates that entry entirely — there was never a
commit at which it held. A stale measurement and a false inspection look identical on the page
and need opposite responses: re-run the first, distrust the second. This is the same
verified/relayed/predicted discipline already required of task reports, applied to this file —
which had no way to carry a claim that was read rather than run, because its own opening line
asserted that everything in it was run.

**A second axis: where a claim was true, not only how it was established.** Three lanes work
three branches at once; a claim about another lane's code is true on the branch that wrote it
and can become false at the merge, and nothing checks that automatically, because each branch
reviews green against its own tree. Prefer wording true in both states; where that's
impossible, name the commit. Durable: "As of `667909d`, the unscoped `vectors::delete` call in
the eviction loop was retired." Branch-dependent, therefore wrong on some branch: "There is no
`vectors::delete`" — the function is `vectors.rs`'s `pub fn delete`, still present on the
branch this file ships from, and absent at `49fca6d` (a tree claim — check with
`git show 49fca6d:crates/memorysafe-backend-sqlite/src/vectors.rs`). Either present-tense form
is false somewhere.

**The quietest sub-case: a comment naming a future task as its remedy.** That deferral goes
stale exactly when the named task *succeeds* — worse than a branch merge, because a merge is
at least an event somebody attends, and a task closing is not. Two on this branch, and they
fail in opposite directions:

- `Backend::import`'s doc names Task 24 as what "must implement" `Header`/`format_version`
  enforcement: "today's code does not enforce it." **Inspected, and false:** Task 24 shipped
  and did implement it — `portability::import` (the SQLite crate) sets `saw_header` in the
  `Header` match arm, checks it after the loop
  (`MalformedImport("import stream carries no Header record")`), and compares
  `format_version` inside that same arm. The doc has been wrong since the task that would have
  falsified it closed.
- `PURGED_COMPONENT`'s doc names Plan 3 Task 1 as the remedy that enforces the `_purged`
  reservation; that task shipped covering `_admin` only. **Inspected, and true:** the doc is
  right and the work is undone. `memorysafe-auth/src/store.rs` imports `ADMIN_COMPONENT`, not
  `PURGED_COMPONENT` — the string appears nowhere in the file — and its guard is
  `subject == ADMIN_COMPONENT || namespace == ADMIN_COMPONENT`; both its tests exercise
  `ADMIN_COMPONENT` only. `PURGED_COMPONENT` is a legal component: a leading `_` passes
  `validate_component`, which rejects only a leading `.`.

The second is the better exhibit, because it wrote its own falsifiability in. From
`PURGED_COMPONENT`'s doc in `memorysafe-core`'s `ids.rs`, verbatim:

> **What enforces it, named rather than gestured at:** Plan 3's adapter deliverable —
> `docs/superpowers/plans/2026-09-05-adapters-and-shadow.md`, Task 1 ("Core — the reserved
> admin scope; `memorysafe-auth`"), whose Global Constraints already state that a
> caller-supplied **subject or namespace** equal to `_admin` is rejected before the scope
> reaches the engine. `_purged` belongs in that same check, on the same two component kinds. A
> reader can go to that task and see whether it covers `_purged` alongside `_admin`; **a
> deferral that named no referent could never be found unfulfilled.**

It named the file, the task, and the exact check, and predicted the mechanism by which someone
would catch it later — then Task 1 shipped covering `_admin` only, and the check its own
author invited is exactly the one that found it. One more nuance, stated as a predictor rather
than an explanation, because the same doc argues it: `_purged` is *not* deferrable for the same
reason `_admin` is. `_admin` guards an authorisation risk that exists only at the adapter;
`_purged` guards a collision risk already live in Plan 1 — a trusted caller can innocently name
a namespace `_purged`, after which ordinary audit rows sit alongside purge records — tolerated
only because the collision is recoverable. The deferral was documented, at the time it was
written, as the *weaker* of two obligations travelling together, which generalises: **a
deferral documented as the lesser case is the one closed last, and therefore the one most
likely to rot.** When two obligations are deferred together and one is described as the
weaker, that is the one to check first.

Both examples were unverified in opposite directions, which is why the pair is worth keeping
rather than either alone: from outside, you cannot tell which way a stale deferral has failed.
One reads as outstanding work that is already done; the other as finished work that is still
outstanding. The cheap remedy: **closing a task includes grepping for deferrals that name
it** — one grep, at the one moment someone is guaranteed to already be looking at that task's
name. Nothing else in the process ever revisits a comment that names a task, which is exactly
why this class stays quiet.

**A third instance shows the same class failing the other way. Measured** (see "Elsewhere,"
below, for the grep): this file's own "Elsewhere" section named `TransactionBehavior::Immediate`
as the pending upgrade from an ordering-based closure to a structural one; the fix landed and
the entry kept naming it pending until this pass corrected it — same remedy, now shown to fail
whichever direction a deferral ages. **Inspected:** a sibling instance lives in `aggregates.rs`'s
module doc, in another lane's crate — not this file's to fix, but the same class once more.

## The freeze, and what it costs to change

`git diff crates/memorysafe-backend/src/conformance/` between `ce1af6b` (where the SQLite
crate began) and `8187bd9` is **two files, zero non-comment lines** — both documentation.
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

**Masked by a working first line — allowlist, with procedure.** `retrieve::passes -> true` and
its four boundary mutants. Not excuses: to mutation-test `filter_sql`, **disable `passes`
first** — with it enabled these cannot fail. See `.cargo/mutants.toml`.

**Retired in the backend-sqlite defect lane (commit `667909d`; a diff claim — check with
`git show 667909d`).** The `vectors::delete -> Ok(())` entry described a mutant masked by
`ON DELETE CASCADE`, and justified keeping the explicit call as defence against a future
schema that drops the cascade. That justification did not survive measurement: dropping
`ON DELETE CASCADE` from the schema fails 4 tests and
disabling the `foreign_keys` pragma fails 5, so the second line of defence guarded a failure
the suite already catches loudly — while its unscoped, unconditional call in the eviction loop
was itself the defect it now records. On an out-of-scope eviction it **widowed a live item in
another scope**: `items::delete` correctly refuses, which is precisely why the cascade never
fires, and the unscoped delete then strips the vector from an item still alive elsewhere. That
item stays readable through `get` and `list` while being permanently invisible to vector
search, with no error. Note the direction: an *orphaned vector* — a vector row with no item —
is structurally impossible while the cascade holds. See `.cargo/mutants.toml`'s retired entry
for the measurements and the three tests that pin the cascade and the pragma.

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
- **Two `TIMEOUT`s, not kills** — `keyword.rs`'s `/` → `*` and `scope_embedder`'s constant
  return. Unresolved verdicts caused by the harness, not the code; checked that neither is a
  real `dim`-0 hang.
- **Five filter boundaries.** `filter_sql_narrows_every_dimension_under_limit_pressure`
  pins each predicate's *existence* by count under limit pressure, which is structurally
  unable to catch an off-by-one — a boundary error returns approximately the right count.
  Only `sensitivity` has an exactness test; `occurred_after >=` → `>` and
  `occurred_before <=` → `<` both survive.

## A class, not a lone mutant: the declared-but-unreachable API

`SqliteBackend::forget_tenant` was recorded here as a lone surviving mutant. It isn't one — it
is a member of a recurring class, and naming the class is worth more than the entry it
replaces.

**The class: a broken promise.** The API declares a capability the code does not deliver, and
a reader cannot tell which parts of the surface are real. Not merely unused internally —
**exposed**, so a consumer can find it, depend on it, or configure it, and get nothing. The
defect is not the unused code; it is that the observable surface and the actual behaviour have
quietly separated. That is the same failure the retired `vectors::delete` entry above
describes from the storage side: there, an *orphaned* vector row — one with no item — would be
invisible to every read path, and is now structurally prevented by the FK cascade; here, a
capability sits on the surface with no path that reaches it. Two shapes of one failure.

Members differ in the size of the promise they break, ranked accordingly.

**"This mechanism runs" — broken. Severe: the capability is not delivered at all.**

- **`AuditFilter.subject`. Open.** Inspected: `crates/memorysafe-backend-sqlite/src/audit.rs`'s
  `query` builds its base predicate as `WHERE subject = ?1 AND namespace = ?2` from the
  **scope** argument, and extends it only for `filter.events`, `filter.since`, `filter.until`
  and `filter.after` — `filter.subject` is never read (neither is `filter.namespace`, on the
  same predicate). `AuditFilter::subject`'s own doc calls narrowing by subject "THE compliance
  query," inexpressible without it once a purge has taken the item ids away; `Backend::audit`'s
  trait doc, which spells out `item`, `since`, `until`, `after` and `limit` in detail, says
  nothing about `subject` at all. "Gaps that the freeze locks in" above already records that no
  test sets these fields; this is the same field found dead in the implementation, not merely
  untested.
- **`EngineCache::put_stats` — the write half of its per-scope statistics cache. Open. Cite
  `c3e0d01`** (Task 37's cache work; this branch is based on `8dd7ee6` and does not contain the
  file — a tree claim, verified at that commit via
  `git show c3e0d01:crates/memorysafe-engine/src/cache.rs`, not this one). `EngineCache::stats`
  and `EngineCache::put_stats` are defined in `crates/memorysafe-engine/src/cache.rs`. Every
  call to `put_stats` is `#[cfg(test)]`: in `crates/memorysafe-engine/src/maintain.rs` and
  `crates/memorysafe-engine/src/mutate.rs` every call sits below each file's `#[cfg(test)]`
  boundary, and the rest are in `tests/cache.rs` — so the stats map is never written outside a
  test. Production reads bypass it entirely: `gather.rs`, `maintain.rs` and `read.rs` each call
  `backend.scope_stats(...)` directly. Its sharp edge: `CacheConfig` exposes `stats_capacity`
  and `stats_ttl` as public, defaulted, tunable fields — operator-facing knobs for a cache that
  is never populated. `stats_ttl`'s own doc comment prices the governance cost of a stale
  corpus mean skewing every assessment made against it, which shows the author had already
  reasoned this through; wiring it now would ratify a decision already made, not fix an
  oversight. No disposition recorded here — that call belongs to the engine lane.
- **`SqliteBackend::forget_tenant`. Open.** `pub`, zero callers in `crates/` other than the
  module doc at the top of the same file instructing an operator to "Call
  [`SqliteBackend::forget_tenant`] first" — a doc reference, not a call. Measured:
  `grep -rn "forget_tenant" crates/ --include=*.rs` returns exactly those two lines.
  Documented as the operator half of tenant deletion. Decide: test it (delete the tenant's
  files, then show a cached handle would have served stale reads) or remove it.

**"This value is used" — broken. Milder: the mechanism runs, a caller-supplied value just
never feeds it.**

- **`MergeWrite.byte_size`. Open; disposition is deleting the field, a tidy-up rather than a
  defect fix.** Inspected: `items::merge` ignores the caller-supplied `byte_size` entirely — it
  is not even a parameter — and instead measures `before = existing.byte_size()` and
  `after = updated.byte_size()` at the storage layer, returning the difference, which
  accumulates into `delta_bytes` and lands in the single `capacity::adjust` at the end of
  `apply`'s transaction. The accounting is correct, and it ignores the field deliberately: a
  caller's claim about how many bytes it is writing cannot be verified, while a row diff can.
  Crate-local test: `a_merge_folds_the_item_and_adjusts_capacity_by_the_delta_not_the_new_size`.
  **Do not read this as a capacity-accounting defect** — that was the hypothesis that routed it
  here, and the code refutes it; the promise it breaks is "this value is used," not "this
  mechanism runs." See "Derive from what exists, not from what the caller claims" below for
  why the field went dead rather than merely untested.

**Closed — the member that shows the class is fixable, not just a complaint.**

- **`serde_json` in `memorysafe-policy`.** Measured: it sat under `[dependencies]` while every
  real use was test-only (`grep -c` for `serde_json` across
  `crates/memorysafe-policy/src/*.rs` finds twelve hits; eleven are calls in `value.rs` below
  its `#[cfg(test)]` line, the twelfth is a doc comment in `eviction.rs`) — so every downstream
  consumer of the policy crate compiled a JSON parser it could never reach. Fixed by another
  lane: `crates/memorysafe-policy/Cargo.toml` now carries `serde_json.workspace = true` under
  `[dev-dependencies]`.

**Why the configuration-surface shape is the worst of the three.** Dead code wastes space; a
tunable knob for a mechanism that does not run **misinforms** — it invites an operator to
conclude a thing works because they configured it, which is a stronger and falser belief than
simply not knowing the code exists.

## Derive from what exists, not from what the caller claims

Three instances in this crate; the first two verified by reading, the third a proposal that
was rejected for violating what the first two establish.

1. **Import takes a vector's scope from the item it belongs to, not from the stream.**
   `portability::import`'s SQLite implementation reconstructs `scope: Scope` from
   `item.scope.clone()` and passes that same `scope` into
   `vectors::insert(&tx, &item.id, &scope, &q)` — not a scope carried separately on the
   vector's own stream record. This is the path where trusting the payload would have been
   most tempting, because the stream record is right there.
2. **`items::merge` takes bytes from the rows, not from `MergeWrite.byte_size`.** It computes
   `before = existing.byte_size()` and `after = updated.byte_size()` and returns the
   difference; the caller-supplied field is not even a parameter. That is exactly why the
   field is dead — see the class entry above — rather than merely a broken promise with no
   explanation: the mechanism it would feed already has a source of truth it trusts more.
3. **A proposed repair that was rejected for violating it.** Adding
   `subject=excluded.subject, namespace=excluded.namespace` to `vectors::insert`'s
   `ON CONFLICT(item_id) DO UPDATE SET` — which today updates only `embedder`, `dim`, `scale`
   and `q` — would have made the `vectors` row agree with whatever scope the caller passed
   rather than with what `items` says, propagating a wrong scope as readily as correcting a
   stale one, and silently healing the symptom on every re-embed so the underlying bug gets
   *harder* to find.

The first two are why the third is wrong: this codebase already has an established practice of
deriving scope and size from what is actually stored rather than from what a caller claims, so
a repair that inverts that practice for the sake of a scope column agreeing with the caller
would be a regression dressed as a fix. That is the durable form of the rejection — it survives
someone re-proposing the repair in six months when nobody remembers the conversation.

## An instrument that produces a wrong answer indistinguishable from a real result

Four instances, from three separate lanes on one day. No lane could see the pattern from where
it sat.

1. **A too-literal grep.** Measured, and self-demonstrating:

       grep -c "vectors::insert storing a wrong" docs/superpowers/plans/2026-09-05-engine-and-sqlite.md
       -> 0
       grep -o '`vectors::insert` storing a wrong `namespace`' docs/superpowers/plans/2026-09-05-engine-and-sqlite.md
       -> `vectors::insert` storing a wrong `namespace`

   The real text carries backticks the unbackticked literal never will, so the first grep's
   zero read exactly like "the claim is false" and came within one keystroke of retracting a
   correct finding. The second grep is the point: it shows the instrument *can* report
   presence, which is what proves the first zero was the instrument's fault, not the world's.
2. **A piped exit code. Relayed** — not reproduced for this entry; reported to have happened
   twice independently the same day. `cargo test … | tail` reports the pipe's exit status, not
   cargo's. In this lane, a baseline run piped through `tail -30` silently discarded the test
   summary while the command still reported success, so parsing the truncated output produced
   "0 passed, 0 failed" — a manufactured absence that read as a measurement. General trap, not
   one lane's mistake: `| tail` masks the real exit status and truncates the evidence in the
   same stroke.
3. **`vectors::count`'s own scope predicate was unpinned. Closed. Cite `49fca6d`** (the tip of
   `worktree-sdd-sqlite-defects`; this branch is based on `8dd7ee6` and does not contain the
   fix — measured via `git show 49fca6d:crates/memorysafe-backend-sqlite/src/vectors.rs`, not
   this branch's HEAD). `vectors::count` is the accessor built
   specifically to detect scope leaks, which is what makes this the sharpest of the four: the
   instrument *was* the leak detector, and its own predicate went untested. The surviving
   mutant replaced the entire `WHERE` with `?1 IS NOT NULL AND ?2 IS NOT NULL` — the bound
   params still referenced, so the code reads as using them, while the predicate restricts
   nothing, turning a per-scope count into a whole-tenant-file count. Why it survived, in the
   fix's own words: **"exercised at 0 and at 1" was exercise, not coverage** — every test
   reading through `count` kept exactly one vector row in the entire tenant at a time, so a
   per-scope count and a whole-file count are numerically identical. Closed by
   `count_is_scoped_by_subject_and_namespace_not_the_whole_tenant_file`, which plants a vector
   in three scopes differing from `home` in exactly one coordinate each, asserts the tenant
   file genuinely holds three rows first (a positive control, so counts of 1 prove scoping
   rather than an empty file), then asserts each scope counts 1 — a predicate dropping the
   whole `WHERE`, or just its subject half, or just its namespace half, each makes a different
   one of the three come out too high.
4. **A survey that searched for the wrong statement shape.** Measured, and the most
   instructive of the four. This file's own "Elsewhere" entry said the `vectors` table's
   `subject`/`namespace` are duplicated "with exactly one reader, `scope_embedder`" — wrong,
   and wrong before any of today's work:
   `grep -rn "FROM vectors\|JOIN vectors\|INTO vectors\|UPDATE vectors" crates/memorysafe-backend-sqlite/src/*.rs`
   finds two production readers at this branch's HEAD, not one — `vectors::scope_embedder` and
   **`purge::subject`** (`DELETE FROM vectors WHERE subject = ?1`), which predates the entry
   entirely; a third reader is arriving in another lane's in-flight work, unlanded and not
   named here. **A `WHERE` clause reads a column whether or not the statement is a `SELECT`**
   — the entry counted `SELECT`s and missed a `DELETE`. In the other three instances the faulty
   instrument was a tool — grep, a pipe, a test run; here it was a human-authored question
   ("which queries read this column?") that silently excluded a whole category of caller. The
   class is not about tooling; it is about any procedure that can return absence. See
   "Elsewhere" below for the corrected count; this entry owns the error class, that one owns
   the correction.

**The rule that falls out, stated as procedure:** when an instrument reports absence, first
show the instrument can report presence. A grep that finds nothing should be shown finding
something. A counter used to detect a difference should be shown returning different numbers.
A test should be shown failing before it is trusted passing — TDD's premise, generalised past
tests to every instrument. The formulation to use: *absence of evidence manufactured by the
instrument, not by the world.*

**Two things this file and the plan already contain turn out to be the same class, stated once
each.** The engine-and-sqlite plan's Global Constraints already warn about one costume of this
without naming the general case: under "Mutation-test every mechanism you add, and paste the
output," they require grepping for `error[E` and `could not compile` in the same pass as test
failures, because a mutation that fails to compile and a mutation nothing catches produce the
same silence. The project already knew the specific instance and paid for the general one
three times in one day. And "Surviving mutants" above's "Masked by a working first line" entry
— `retrieve::passes -> true` and its four boundary mutants, unfailable while `passes` is
enabled — is the same class with its remedy already applied by hand to one case: disable
`passes` before mutation-testing `filter_sql` *is* "show the instrument can report presence,"
written before the rule was.

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

- **Every transaction now opens `TransactionBehavior::Immediate`. Correction, measured**
  (see "Surviving mutants" above): this entry read "every transaction is `DEFERRED`," true only
  while `apply`'s opening write (`capacity::ensure_row`) closed the `SQLITE_BUSY_SNAPSHOT`
  exposure by ordering alone — **by ordering, which no test pinned** — and it stayed unrevised
  after the fix landed.
  `grep -rn "TransactionBehavior::Immediate" crates/memorysafe-backend-sqlite/src/` finds four
  call sites: `purge.rs:25`, `portability.rs:152`, `lib.rs:111`, `lib.rs:198`. The exposure is
  now closed structurally, not by the position of one call.
- **The `vectors` table stores scope twice.** `subject`/`namespace` are duplicated from
  `items`. **Correction, measured** (see "An instrument that produces a wrong answer" above):
  the original "exactly one reader, `scope_embedder`" count was an inspection, not a
  measurement, and was wrong from the start.
  `grep -rn "FROM vectors\|JOIN vectors\|INTO vectors\|UPDATE vectors" crates/memorysafe-backend-sqlite/src/*.rs`
  finds two production readers at this branch's HEAD — `vectors::scope_embedder` and
  `purge::subject` (`DELETE FROM vectors WHERE subject = ?1`), which predates this entry
  entirely; a third is arriving in another lane's in-flight, unlanded work. `search` still
  filters through the `items` join and never reads these columns. Schema owner's call; the
  join cost is unmeasured.
- **`Protection::Protected { until }` is constructed nowhere in the suite**, so the
  `protected_until` column is written by no conformance test.
