# Task T7 report

**Status: DONE** (fix round 1 applied — see bottom section)

**Commit:** `72864b5` on branch `sdd-conformance-contract` (parent `acebe76`).

**Test count:** unchanged. `cargo test --workspace --no-fail-fast`, both before and after the
edit: **494 passed, 1 failed, 495 total**. The one failure is
`sqlite_passes_the_backend_conformance_suite`, panicking inside
`conformance::lifecycle::audit_filter_narrows_by_item` ("filtering by item A's id also returned
item B's admit record") — this is the expected, pre-existing failure from an earlier task in
this lane (SQLite lacks `AuditFilter.item`, fixed on another branch). Not caused by this task;
this task touched only `docs/known-gaps.md`. `git status --porcelain` confirms no other file
changed. `cargo fmt --check` and `cargo clippy --all-targets --all-features -- -D warnings`
both clean. — **measured**.

## Members 1 and 2, as required by Steps 1–2

- **Member 1 (`serde_json`, CLOSED) — verified.** `crates/memorysafe-policy/Cargo.toml` has
  `serde_json.workspace = true` under `[dev-dependencies]`, not `[dependencies]`. Independently
  re-counted the "eleven uses" claim: `grep -c serde_json crates/memorysafe-policy/src/*.rs`
  finds 12 hits; 11 are `serde_json::json!` calls in `value.rs`, all below its `#[cfg(test)]`
  line; the 12th is a doc comment in `eviction.rs`. Matches the brief exactly. — **measured**.
- **Member 2 (`forget_tenant`, OPEN) — verified.** `grep -rn "forget_tenant" crates/
  --include=*.rs` returns exactly two lines: the `pub fn forget_tenant` definition and the
  module-doc reference ("Call [`SqliteBackend::forget_tenant`] first") in the same file. No
  other occurrence anywhere in `crates/`. — **measured**.
- **Member 3 (engine stats-cache half, OPEN, cite `e5d810c`) — verified**, off-branch, via
  `git grep ... e5d810c -- crates/memorysafe-engine/`. Reproduced every grep the brief gave:
  `put_stats`/`stats` defined in `cache.rs`; every call site in `maintain.rs` (boundary line
  316) and `mutate.rs` (boundary line 340) sits below the file's `#[cfg(test)]` module; the rest
  are in `tests/cache.rs`; production reads (`gather.rs`, `maintain.rs`, `read.rs`) call
  `backend.scope_stats(...)` directly. — **measured** (against `e5d810c`, not this branch's
  HEAD).

## Additional claims I independently verified beyond what the brief asked me to re-check

- `AuditFilter.subject` (added as a fourth, severe member): read `audit::query` in
  `crates/memorysafe-backend-sqlite/src/audit.rs` — its base predicate is
  `WHERE subject = ?1 AND namespace = ?2` built from the `scope` argument; it only conditionally
  extends for `filter.events/since/until/after`. `filter.subject` (and `.namespace`) are never
  referenced. `Backend::audit`'s trait doc documents `item`/`since`/`until`/`after`/`limit` in
  detail and says nothing about `subject`. — **inspected**, not run.
- `MergeWrite.byte_size`: read `items::merge` (no `byte_size` parameter at all; computes
  `before`/`after` from stored rows) and its caller in `SqliteBackend::apply`
  (`items::merge(&tx, &txn.scope, &m.target, &m.body, &m.tags, &m.attrs)` — `m.byte_size` is
  never passed). — **inspected**.
- `portability::import`'s scope-from-item behaviour, `vectors::insert`'s current
  `ON CONFLICT DO UPDATE SET` (does not touch subject/namespace), `purge::subject`'s DELETE,
  `vectors::scope_embedder`'s SELECT, `vectors::delete` at `vectors.rs:43` on this branch and
  absent at `0163047` (0 hits via `git show 0163047:...vectors.rs | grep -c "pub fn delete"`).
  — all **measured/inspected** as appropriate, matching the brief's own greps exactly.
- `Backend::import`'s Task 24 doc claim: found the actual doc in
  `crates/memorysafe-backend/src/lib.rs` ("This is a contract Task 24 must implement — today's
  code does not enforce it"), and confirmed `portability::import` in the SQLite crate does now
  enforce it (`saw_header` + `MalformedImport("import stream carries no Header record")` +
  a version check inside the `Header` arm). Doc is stale/false, as the brief said. — **verified
  by reading both files**, i.e. inspected.
- `PURGED_COMPONENT`'s doc and `memorysafe-auth/src/store.rs`'s guard: reproduced the brief's
  grep exactly — `ADMIN_COMPONENT` imported, `PURGED_COMPONENT` absent, guard is
  `subject == ADMIN_COMPONENT || namespace == ADMIN_COMPONENT`. — **measured**.
- Instance 1 (too-literal grep) and Instance 4 (reader-count survey) of the instrument-class
  entry: reproduced both grep pairs verbatim; results matched the brief exactly (0 then 1 hit;
  two production readers — `scope_embedder` and `purge::subject` — not one). — **measured**.

I did not independently re-verify the "4 tests / 5 tests" figures inside the SQLite lane's
supplied `vectors::delete` replacement text, or the piped-exit-code (`| tail`) incident from
Instance 2 of the instrument class — both used/described as given, per the brief's instruction
not to redraft the supplied text. Those are **relayed**.

## One resolved ambiguity — flagging as instructed

The brief contains an internal contradiction: the "Member 3" section (lines ~106–139)
explicitly instructs naming `EngineCache::stats`/`put_stats`, `CacheConfig`, `stats_capacity`,
and `stats_ttl`, citing `e5d810c` "so a reader knows where to look" — but the later "Must Not"
list says "Name `EngineCache`, `CacheConfig`, `stats_capacity`, `stats_ttl`, `put_stats`, or any
engine symbol. They are uncitable today," which reads as a stale holdover from before Task 37's
cache work landed. I resolved this conservatively toward the **more restrictive, later-stated**
rule: I did **not** name any of those symbols in `known-gaps.md`. The new class entry instead
describes the mechanism generically ("the engine crate's read-through cache for per-scope
statistics — the write half," "the cache's config type exposes two public, defaulted fields")
while still citing `e5d810c` and the exact file paths (`cache.rs`, `maintain.rs`, `mutate.rs`,
`gather.rs`, `read.rs`) so the entry stays locatable and reviewable without using the forbidden
identifiers. Flagging this so you can tell me if the "Must Not" was in fact stale and the
symbols should be named — that's a one-line follow-up edit if so.

## What was written

Four deliverables, all in `docs/known-gaps.md`, one commit:

1. A new "A class, not a lone mutant: the declared-but-unreachable API" section (moves
   `forget_tenant` out of "Surviving mutants"'s Open list; ranks members by promise size; links
   to the retired `vectors::delete` entry as the same failure from the storage side).
2. A new "An instrument that produces a wrong answer indistinguishable from a real result"
   section (four instances; states the rule; cross-references the plan's Global Constraints and
   the existing `passes`-allowlist entry; corrects "Elsewhere"'s undercounted-reader line as its
   own worked example, without duplicating the analysis there).
3. A new short "Derive from what exists, not from what the caller claims" section (three
   instances; explains why `MergeWrite.byte_size` is dead rather than merely a broken promise).
4. A new "A mark this file needs on itself: measured / inspected / relayed" section placed near
   the top, next to the file's existing "every line was verified by running something" line —
   covering the provenance convention itself, the cross-branch wording axis (with the
   `vectors::delete`/`ce8168e`/`0163047` worked example), and the deferral-rot sub-case (Task 24
   and `PURGED_COMPONENT`, both independently verified, failing in opposite directions).
5. Replaced the `vectors::delete -> Ok(())` allowlist line under "Surviving mutants" with the
   backend-sqlite lane's supplied verbatim text (commit-anchored to `ce8168e`), used as given
   per instruction — did not redraft it.

No disposition was proposed beyond what the brief explicitly supplied (`forget_tenant`'s
existing text, preserved verbatim; `MergeWrite.byte_size`'s "delete the field, a tidy-up," which
the brief itself stated as fact rather than asking me to invent). `AuditFilter.subject` and the
engine stats-cache member are recorded with no disposition, as instructed.

## Concerns

- The Member-3 naming resolution above — please confirm which way you want it; easy to flip.
- Untouched but adjacent: "Elsewhere"'s first bullet ("Every transaction is `DEFERRED`") reads
  as in tension with "Surviving mutants"'s "all four transactions now opening `Immediate`" line
  a few sections earlier. This predates my change, is outside this task's scope, and I left it
  alone — flagging only in case nobody else has noticed it.
- `.cargo/mutants.toml` was not touched, per instruction; I did not check whether that file's
  own retirement entry (the other lane's half of this same edit) has landed yet on this branch
  — it hadn't as of my read, and the brief said not to touch it either way.

## Fix round 1

Review came back Approved with two Important, mechanical findings. Fixed both, nothing else.
New commit: `<filled in below>`.

**Finding 1 — name the engine-cache symbols.** The "Must Not ... uncitable today" clause was
confirmed stale (its premise died when `e5d810c` landed) and struck from the brief; the
Member-3 section governs. Rewrote the `EngineCache::put_stats` bullet in "A class, not a lone
mutant" to name `EngineCache::stats`, `EngineCache::put_stats`, `crates/memorysafe-engine/src/cache.rs`,
`tests/cache.rs`, `CacheConfig`, `stats_capacity`, and `stats_ttl` directly — replacing the
periphrasis ("the cache's populate function", "the cache's config type", "two public,
defaulted fields", "the crate's dedicated cache test file") with the actual symbols, still
citing `e5d810c`. No other wording in that bullet or entry changed.

**Finding 2 — provenance tags.** Swapped every non-canonical "Verified" for the tag that
actually applies, and fixed one substantive mismatch:

- Instrument-class Instance 1 (too-literal grep): `Verified` → **Measured** (I ran both greps
  myself and the output matched).
- Instrument-class Instance 2 (piped exit code): `Verified` → **Relayed**. I chose *not* to
  reproduce it — it's a historical incident report, not a standing mechanism to re-run — and
  said so explicitly in the text ("not reproduced for this entry; reported to have happened
  twice independently the same day"), matching what my original report already admitted. This
  is the mismatch the coordinator flagged: the file previously said "Verified" while my own
  report said "relayed." Now consistent, and a reader of the file alone can tell.
- Instrument-class Instance 3 (`vectors::count`'s unpinned predicate): rather than relabel the
  lowercase "verified by reading the file at that commit" without checking it, I went back and
  actually ran `git show 0163047:crates/memorysafe-backend-sqlite/src/vectors.rs` and read the
  `count` function, the surviving-mutant doc comment, and the closing test in full. Every
  specific claim in that bullet — the `?1 IS NOT NULL AND ?2 IS NOT NULL` mutant, the verbatim
  "exercised at 0 and at 1 was exercise, not coverage" comment, the three-scope positive-control
  test structure — is present at that commit exactly as written. So this one is now genuinely
  **Measured**, not just relabeled; citation updated to name the exact command run.
- Instrument-class Instance 4 (reader-count survey): `Verified` → **Measured** (I ran that grep
  myself and it produced the two readers named).
- `MergeWrite.byte_size` bullet's lead sentence: `Verified:` → **Inspected:** (this is a
  structural fact about a function signature and call site, established by reading
  `items::merge` and its caller in `SqliteBackend::apply`, not by running anything).

Left untouched, per the "NOT open" list: all spot-checked claims, the verbatim
`vectors::delete` replacement text, `forget_tenant`'s move, line-number-free citations, and the
voice of the deferral-rot / derive-from-what-exists sections. Also left alone: the pre-existing
"verified by running something" line in the file's own opening (quoting the file, not one of my
tags) and "verified/relayed/predicted" in the provenance section (naming the *other*,
pre-existing convention used by task reports, for explicit contrast — not a stray use of my
tag).

**Verification after the fix:** `cargo test --workspace --no-fail-fast` — still **494 passed,
1 failed, 495 total**; the one failure is `sqlite_passes_the_backend_conformance_suite`
panicking in `conformance::lifecycle::audit_filter_narrows_by_item` (the same expected,
pre-existing failure). `cargo fmt --check` clean. `cargo clippy --all-targets --all-features --
-D warnings` clean. `git status --porcelain` shows only `docs/known-gaps.md` modified.
