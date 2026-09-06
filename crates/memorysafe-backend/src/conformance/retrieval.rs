use super::{BackendFactory, fx};
use crate::query::{CandidateQuery, HardFilters};
use crate::{Backend, Page};
use memorysafe_core::{Scope, SensitivityLevel};
use memorysafe_embed::Embedder;
use time::{Duration, OffsetDateTime};

fn query(text: &str, filters: HardFilters) -> CandidateQuery {
    CandidateQuery {
        embedding: Some(fx::embedder().embed(text).unwrap()),
        text: Some(text.to_string()),
        filters,
        limit: 50,
    }
}

fn query_with_limit(text: &str, filters: HardFilters, limit: usize) -> CandidateQuery {
    CandidateQuery {
        embedding: Some(fx::embedder().embed(text).unwrap()),
        text: Some(text.to_string()),
        filters,
        limit,
    }
}

/// The security-critical one: a restricted item must never leave the database
/// for a caller cleared only to Personal.
///
/// The corpus needs three properties, each closing a distinct way a ceiling
/// bug could hide behind this test's original, weaker shape:
///
/// 1. **It must exceed the query's `limit`, with the excluded items worded
///    to dominate the ranking.** With a corpus smaller than `limit` (the
///    original shape — one item per level, `limit: 50`), a backend that
///    applies the ceiling inside its query and a backend that runs an
///    unfiltered query and filters the ceiling out afterwards, in Rust,
///    return byte-identical results, because nothing was ever truncated.
///    The two strategies are indistinguishable and the test cannot tell a
///    compliant backend from a dangerous one.
/// 2. **The excluded pool must be large enough to survive a constant-factor
///    over-fetch, not just a naive `SELECT *`.** `CandidateQuery::limit` is
///    itself documented (`query.rs`) as an over-fetch knob the engine
///    "typically sets 5-10x the recall budget," so a backend issuing
///    `LIMIT limit * k` unfiltered and filtering afterwards is not a
///    contrived worst case — it is the natural way to write a hybrid
///    backend's per-arm fetch, and the original 5-item pool (corpus 7,
///    `limit: 3`) fails to catch it: any `k >= 3` fetches the whole corpus
///    unfiltered and returns the correct 2 rows regardless of where
///    filtering happens. With 40 excluded items ranked 1..=40 and the 2
///    admissible items ranked 41 and 42, reaching an admissible row needs
///    `limit * k >= 41`, i.e. `k >= 14` at `limit: 3` — well past the
///    documented 5-10x, so no plausible over-fetch factor leaks the
///    diagnosis. Do not shrink this pool "for speed"; that is exactly the
///    trim that would silently reopen this hole.
/// 3. **The excluded pool must include a level immediately above the
///    ceiling, not only one far above it.** A ceiling of `Personal`
///    (ordinal 2) next to a corpus of only `Restricted` items (ordinal 4)
///    cannot catch a backend whose SQL admits `level_ord <= ceiling_ord +
///    1` — the single most likely off-by-one at the boundary — because
///    ordinal 4 still fails that relaxed check exactly like the correct
///    one (`level_ord <= ceiling_ord`). One `Sensitive` item (ordinal 3,
///    immediately above the ceiling) among the top-ranked pool is wrongly
///    admitted by an off-by-one query and correctly excluded by a
///    compliant one, so both the count assertion and the per-item
///    assertion below catch it.
///
/// The two admissible items share 4 of the query's 5 tokens — differing
/// only in the discriminating first word ("restricted" vs. "public"/
/// "personal") — rather than sharing only one token as the original
/// wording did. That keeps their vector score well above any plausible
/// recall floor in a hybrid backend's vector arm, so a failure here cannot
/// be misdiagnosed as "ceiling applied after LIMIT" when it is really an
/// unrelated recall-floor issue dropping a weakly-matching candidate.
pub async fn sensitivity_ceiling_is_enforced_in_the_query<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // 40 items, verbatim-identical to the query text, so every one of them
    // ranks at the very top under any reasonable vector or keyword scorer.
    // 39 are `Restricted` (ordinal 4); one is `Sensitive` (ordinal 3, the
    // level immediately above the `Personal` ceiling) to catch an
    // off-by-one at the boundary. See point 2 above for why 40, and point 3
    // for why one of them is `Sensitive` rather than all `Restricted`.
    for i in 0..40 {
        let level = if i == 0 {
            SensitivityLevel::Sensitive
        } else {
            SensitivityLevel::Restricted
        };
        let item = fx::item_with(
            &scope,
            "restricted medical record about cats",
            "fact",
            &[],
            level,
        );
        backend
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }
    for (body, level) in [
        ("public medical record about cats", SensitivityLevel::Public),
        (
            "personal medical record about cats",
            SensitivityLevel::Personal,
        ),
    ] {
        let item = fx::item_with(&scope, body, "fact", &[], level);
        backend
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }

    let filters = HardFilters {
        sensitivity_ceiling: SensitivityLevel::Personal,
        ..Default::default()
    };
    let hits = backend
        .retrieve_candidates(
            &scope,
            &query_with_limit("restricted medical record about cats", filters, 3),
        )
        .await
        .unwrap();

    assert_eq!(
        hits.len(),
        2,
        "expected exactly the 2 admissible items; fewer means the ceiling was \
         applied after LIMIT (or with too small an over-fetch margin) instead \
         of inside the query, and more means a level above the ceiling — most \
         likely Sensitive, at the ceiling+1 boundary — leaked through"
    );
    for h in &hits {
        assert!(
            h.item.sensitivity <= SensitivityLevel::Personal,
            "leaked {:?}: {}",
            h.item.sensitivity,
            h.item.body
        );
    }
}

pub async fn tag_and_kind_filters_narrow_results<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let cases = [
        ("alpha about cats", "fact", vec!["work"]),
        ("beta about cats", "preference", vec!["work"]),
        ("gamma about cats", "fact", vec!["home"]),
    ];
    for (body, kind, tags) in cases {
        let item = fx::item_with(&scope, body, kind, &tags, SensitivityLevel::Internal);
        backend
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }

    let by_kind = HardFilters {
        kinds: vec!["fact".into()],
        ..Default::default()
    };
    assert_eq!(
        backend
            .retrieve_candidates(&scope, &query("cats", by_kind))
            .await
            .unwrap()
            .len(),
        2
    );

    let by_tag = HardFilters {
        tags_any: vec!["home".into()],
        ..Default::default()
    };
    assert_eq!(
        backend
            .retrieve_candidates(&scope, &query("cats", by_tag))
            .await
            .unwrap()
            .len(),
        1
    );

    let both = HardFilters {
        kinds: vec!["fact".into()],
        tags_any: vec!["work".into()],
        ..Default::default()
    };
    assert_eq!(
        backend
            .retrieve_candidates(&scope, &query("cats", both))
            .await
            .unwrap()
            .len(),
        1
    );
}

pub async fn vector_search_ranks_by_similarity<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for body in [
        "the cat sat on the mat",
        "the cat sat on a rug",
        "quarterly revenue exceeded projections",
    ] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let probe = fx::embedder().embed("the cat sat on the mat").unwrap();
    let hits = backend.neighbours(&scope, &probe, 3).await.unwrap();

    assert_eq!(hits.len(), 3);
    assert_eq!(
        hits[0].item.body, "the cat sat on the mat",
        "exact match should rank first"
    );
    assert!(
        hits[0].relevance >= hits[1].relevance && hits[1].relevance >= hits[2].relevance,
        "neighbours must come back sorted descending"
    );
    assert_eq!(hits[2].item.body, "quarterly revenue exceeded projections");
}

pub async fn keyword_search_finds_exact_terms<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // A rare token a hash embedder will not usefully cluster.
    for body in [
        "the deployment used zstandard compression",
        "unrelated musings",
    ] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let q = CandidateQuery {
        embedding: None,
        text: Some("zstandard".into()),
        filters: HardFilters::default(),
        limit: 10,
    };
    let hits = backend.retrieve_candidates(&scope, &q).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].item.body.contains("zstandard"));
    assert!(hits[0].keyword_score.is_some());
    assert!(hits[0].vector_score.is_none());
}

/// Syntax characters a keyword arm treats specially must not become query
/// operators when they arrive as ordinary user text — whatever the
/// backend's own search engine is (SQLite FTS5, Postgres full-text search,
/// or anything else). Only the assertions below are the contract; nothing
/// about them names one engine's syntax.
///
/// The corpus here holds three items, not one. With only one item present —
/// the original shape of this test — "matched everything" and "matched
/// nothing" are the same observation: both produce a result set of size
/// `<= 1`, so the assertion below cannot tell a backend that correctly
/// treats hostile text as an inert literal from one that lets it become an
/// operator and match the whole scope. Three items make the two outcomes
/// different sizes (`<= 1` vs. `3`), so an unescaped hostile string that
/// turns into a match-everything query is actually caught.
///
/// None of the three bodies contains a standalone single-letter token.
/// `"a AND b"` is one of the hostile inputs below; a keyword arm with
/// AND-by-default semantics (FTS5's phrase escaping, Postgres's
/// `plainto_tsquery`) treats it as an inert literal phrase and correctly
/// matches nothing, but a *correctly escaped* arm with any-term OR
/// semantics would still match every body containing a standalone `"a"`.
/// The original corpus ("a normal memory" / "a third normal memory") had
/// two such bodies, so it would fail this test for a compliant
/// OR-semantics backend — the suite rejecting a correct implementation,
/// which is worse than missing an incorrect one.
pub async fn keyword_search_escapes_user_input<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    for body in ["alpha memory", "beta memory", "gamma memory"] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    for hostile in ["\"", "OR 1=1", "a AND b", "NEAR/", "*", "(unbalanced"] {
        let q = CandidateQuery {
            embedding: None,
            text: Some(hostile.to_string()),
            filters: HardFilters::default(),
            limit: 10,
        };
        // Must not error and must not match everything.
        let hits = backend.retrieve_candidates(&scope, &q).await.unwrap();
        assert!(
            hits.len() <= 1,
            "hostile input {hostile:?} matched {} of 3 rows",
            hits.len()
        );
    }
}

pub async fn hybrid_returns_both_signal_sources<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for body in ["the cat sat on the mat", "zstandard compression details"] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let hits = backend
        .retrieve_candidates(
            &scope,
            &query("the cat sat on the mat", HardFilters::default()),
        )
        .await
        .unwrap();

    let exact = hits
        .iter()
        .find(|h| h.item.body == "the cat sat on the mat")
        .unwrap();
    assert!(exact.vector_score.is_some(), "vector score missing");
    assert!(exact.keyword_score.is_some(), "keyword score missing");
    assert!(exact.relevance > 0.0);
}

/// Three pages must not overlap and must not drop rows: page *disjointness*
/// and completeness, and nothing else.
///
/// Named for what it proves. It was called `pagination_is_stable`, and the
/// name was part of why a gap survived for so long: "stable" reads as
/// tie-break stability — the property this test does *not* check — so an
/// auditor reading test names ticked that box and moved on. The collected ids
/// are sorted and deduped before the assertions, which is exactly right for
/// disjointness and destroys any evidence of order, direction included.
///
/// **The implementation this exists to reject: one whose `OFFSET` arithmetic
/// is off by one**, so a row appears on two pages or on none. That is a real
/// bug, and it survives a perfect total order — which is why this test keeps
/// its own corpus rather than being merged into the ordering tests.
///
/// The corpus uses `fx::item_at`, so every `created_at` is distinct and
/// nothing ties. That is deliberate: with no ties, no tie-break is involved,
/// so an overlap or a dropped row here can only be a paging bug and never a
/// tie-break bug. The tied case is covered on its own by
/// `list_tie_break_is_total_over_identical_timestamps`, and direction by
/// `list_orders_oldest_first_by_created_at`.
pub async fn list_pages_are_disjoint_and_complete<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for i in 0..25 {
        let created_at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(i);
        let item = fx::item_at(&scope, &format!("memory {i:02}"), created_at);
        backend
            .apply(fx::admit_txn(&scope, item, None))
            .await
            .unwrap();
    }

    let p1 = backend
        .list(
            &scope,
            &Page {
                offset: 0,
                limit: 10,
            },
        )
        .await
        .unwrap();
    let p2 = backend
        .list(
            &scope,
            &Page {
                offset: 10,
                limit: 10,
            },
        )
        .await
        .unwrap();
    let p3 = backend
        .list(
            &scope,
            &Page {
                offset: 20,
                limit: 10,
            },
        )
        .await
        .unwrap();

    assert_eq!((p1.len(), p2.len(), p3.len()), (10, 10, 5));

    let mut ids: Vec<_> = p1
        .iter()
        .chain(&p2)
        .chain(&p3)
        .map(|i| i.id.clone())
        .collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "pages overlapped");
    assert_eq!(total, 25, "pages dropped rows");
}

/// `list` orders **ascending** by `created_at`: page one is the *oldest* ten,
/// not the newest.
///
/// **The implementation this exists to reject: a backend that pages
/// descending.** Such a backend passes the entire rest of the suite. Look at
/// what the other `list` callers actually assert:
/// `list_pages_are_disjoint_and_complete` sorts and dedups the collected ids
/// before asserting, so direction is not merely unchecked — the evidence is
/// destroyed; `purge_subject_*` and `pending_embedding_*` only count rows;
/// `export_import_round_trips_exactly` re-sorts both sides by `ItemId` before
/// comparing. Every one of them is insensitive to direction by accident.
/// `Backend::list`'s own doc comment names "SQLite paging ascending while
/// Postgres paged descending" as the drift this suite exists to prevent, and
/// until this test nothing in the suite prevented it.
///
/// **Why this cannot be merged with the tie-break test.** It needs distinct
/// timestamps, because on a fully tied corpus an ascending and a descending
/// backend produce *identical* output: the primary key contributes nothing and
/// the tie-break alone orders the rows. Run this corpus through the tied one
/// and the discrimination disappears entirely. The two tests read as
/// duplication and are not: see
/// `list_tie_break_is_total_over_identical_timestamps` for the other half of
/// the argument.
///
/// All three pages are asserted, not just the first. A backend that pages
/// descending but sorts each page ascending internally would produce a correct
/// first page of the *wrong ten rows*, which only a whole-corpus assertion
/// catches.
pub async fn list_orders_oldest_first_by_created_at<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // Twelve items, one second apart, inserted oldest-first. Insertion order
    // is irrelevant to the assertion — `created_at` is what orders them — but
    // the bodies are numbered by age so a failure message reads directly as
    // "this backend handed back the newest rows first".
    for i in 0..12i64 {
        let created_at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(i);
        let item = fx::item_at(&scope, &format!("memory {i:02}"), created_at);
        backend
            .apply(fx::admit_txn(&scope, item, None))
            .await
            .unwrap();
    }

    let mut seen: Vec<String> = Vec::new();
    for offset in [0usize, 4, 8] {
        let page = backend
            .list(&scope, &Page { offset, limit: 4 })
            .await
            .unwrap();
        assert_eq!(page.len(), 4, "page at offset {offset} was not full");
        seen.extend(page.into_iter().map(|i| i.body));
    }

    let expected: Vec<String> = (0..12i64).map(|i| format!("memory {i:02}")).collect();
    assert_eq!(
        seen, expected,
        "list must order ascending by created_at — page one is the oldest four. \
         A backend paging descending returns memory 11 first and passes every \
         other list test in this suite."
    );
}

/// The tie-break is **total**: over a corpus where every `created_at` is
/// identical, `list` still returns a single, reproducible order — ascending by
/// `ItemId`.
///
/// **The implementation this exists to reject: a backend whose tie-break is
/// not total** — one that orders by `created_at` alone and lets the storage
/// engine's natural row order decide the rest. `Backend::list`'s doc names
/// this exact case: a bulk import leaves many rows with an identical
/// `created_at`, and `LIMIT`/`OFFSET` over an unstable sort can return the
/// same row on two pages while dropping another.
///
/// **The corpus is built from `fx::item`, whose `created_at` is pinned to
/// `UNIX_EPOCH`, so every row ties and the tie-break is the *only* thing
/// ordering the pages.** That is the whole design of the test, not an
/// oversight inherited from the fixture.
///
/// **The ids are literals, and that is load-bearing.** `ItemId::new()` is
/// `ulid::Ulid::generate()`, and `ulid_id!`'s doc says lexicographic order
/// equals creation order only "up to the timestamp's millisecond resolution" —
/// so ids minted in a tight loop are randomly ordered relative to each other.
/// Built from `fx::item()`, this test would pass or fail by coin flip, and on
/// the runs where the loop straddled a millisecond boundary it would pass
/// *deterministically for the wrong reason*: ids ascending in insertion order
/// let a backend with no tie-break at all through. Do not simplify this back
/// to `fx::item()`.
///
/// **Why this cannot be merged with the direction test.** On this corpus an
/// ascending and a descending backend produce identical output — the primary
/// key contributes nothing — so the tied corpus cannot catch a descending
/// backend; that is `list_orders_oldest_first_by_created_at`'s job. Conversely
/// a distinct-timestamp corpus never ties, so totality is unobservable there.
/// Neither subsumes the other. Against a compliant `(created_at ASC, id ASC)`
/// backend the four wrong combinations split like this: `(ASC, DESC)` caught
/// here only, `(DESC, ASC)` caught by the direction test only, `(DESC, DESC)`
/// caught by both, and no-tie-break-at-all caught here only. This test carries
/// three of the four, which is why it asserts the exact sequence rather than
/// the weaker "the pages did not overlap".
pub async fn list_tie_break_is_total_over_identical_timestamps<F: BackendFactory>(factory: &F) {
    use memorysafe_core::ItemId;

    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // `fx::ITEM_ORDER_ULIDS`: five ULIDs sharing a prefix and differing only in
    // the final character, so their ascending order is `...FA0 < ...FA1 < ...
    // < ...FA4` by inspection.
    // `fixtures::tests::the_literal_ulids_the_ordering_tests_use_parse_and_sort_ascending`
    // checks that premise against the same constant, in a test that actually
    // runs today.
    let ascending: Vec<ItemId> = fx::ITEM_ORDER_ULIDS
        .iter()
        .map(|s| ItemId::parse(s).unwrap())
        .collect();

    // Inserted in a deliberately non-ascending order. A backend that applies
    // no tie-break returns its natural row order, which for most storage
    // engines is insertion order — so this ordering is what makes such a
    // backend fail here deterministically rather than by luck.
    for i in [3usize, 0, 4, 1, 2] {
        let item = fx::item_with_id(&scope, ascending[i].clone(), &format!("tied memory {i}"));
        backend
            .apply(fx::admit_txn(&scope, item, None))
            .await
            .unwrap();
    }

    let mut seen: Vec<ItemId> = Vec::new();
    for (offset, expected_len) in [(0usize, 2usize), (2, 2), (4, 1)] {
        let page = backend
            .list(&scope, &Page { offset, limit: 2 })
            .await
            .unwrap();
        assert_eq!(
            page.len(),
            expected_len,
            "page at offset {offset} had the wrong size"
        );
        seen.extend(page.into_iter().map(|i| i.id));
    }

    assert_eq!(
        seen, ascending,
        "with every created_at tied, the pages must come back in ascending ItemId \
         order. A backend that orders by created_at alone returns its natural row \
         order here — insertion order, which this corpus deliberately made \
         non-ascending — and a backend whose tie-break is descending returns the \
         exact reverse."
    );
}

/// `exclude_pending_embedding` must narrow the candidate set, not empty it.
///
/// The assertion also pins the exact surviving count, not just "nothing
/// pending leaked through": `hits.iter().all(...)` is vacuously true over an
/// empty result set, so a backend that (wrongly) excludes every item
/// whenever the flag is set would pass a leak-only check. Asserting
/// `hits.len() == 1` catches that failure mode as well as the leak.
pub async fn pending_embedding_items_are_excluded_when_asked<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let mut pending = fx::item(&scope, "written while the embedder was down, about cats");
    pending.pending_embedding = true;
    backend
        .apply(fx::admit_txn(&scope, pending.clone(), None))
        .await
        .unwrap();
    backend
        .apply(fx::admit_txn_embedded(
            &scope,
            fx::item(&scope, "a normal memory about cats"),
        ))
        .await
        .unwrap();

    // Visible to review regardless.
    assert_eq!(
        backend.list(&scope, &Page::default()).await.unwrap().len(),
        2
    );

    let filters = HardFilters {
        exclude_pending_embedding: true,
        ..Default::default()
    };
    let hits = backend
        .retrieve_candidates(&scope, &query("cats", filters))
        .await
        .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly the non-pending item to survive the filter"
    );
    assert!(hits.iter().all(|h| !h.item.pending_embedding));
}

/// `neighbours` applies the tie-break **before** truncating at `k`, not after.
///
/// **The implementation this exists to reject: one that takes the top `k` rows
/// in whatever order its index produced and only then sorts them.** With a tie
/// straddling position `k`, that backend and a compliant one return *different
/// neighbour sets* — not the same set in a different order — and
/// `Backend::neighbours`' doc says that is precisely what the tie-break exists
/// to prevent: the policy's `best()` neighbour is the merge target, so a
/// different set is a different merge decision and a different audit record.
///
/// **Nothing else in the suite constructs this case.**
/// `vector_search_ranks_by_similarity` calls `neighbours` with `k` equal to its
/// corpus size, so nothing is ever truncated, and its three bodies are all
/// distinct, so relevance never ties. Both conditions have to hold at once for
/// the boundary to be observable, and only here do they.
///
/// The corpus: one item whose body is the probe text verbatim (it ranks first
/// under any scorer), and **four items sharing one identical body**, which the
/// deterministic embedder maps to one identical vector — an exact tie, not an
/// approximate one. With `k = 3` against a corpus of 5, two of those four must
/// come back and two must be cut, so the tie straddles the boundary. Ascending
/// `ItemId` decides which two.
///
/// **The assertion is set membership, not order.** An order assertion would
/// pass while the membership was wrong — a backend that truncated first and
/// then sorted returns a correctly *ordered* list of the wrong rows, which is
/// the whole reason the doc specifies tie-break before truncation. The ids are
/// literals for the reason `fx::item_with_id` documents: generated ULIDs are
/// ordered only to millisecond resolution, so "which two survive" would
/// otherwise be a coin flip, and the insertion order below is deliberately not
/// ascending so a truncate-first backend fails deterministically.
pub async fn neighbours_break_ties_before_truncating_at_k<F: BackendFactory>(factory: &F) {
    use memorysafe_core::ItemId;
    use std::collections::BTreeSet;

    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let probe_body = "the cat sat on the mat";
    // One identical body for all four tied items: the deterministic embedder
    // is a pure function of the text, so these four score exactly equally
    // against any probe. A tie that merely happens to be close would not
    // exercise the tie-break at all.
    let tied_body = "quarterly revenue exceeded projections";

    let exact = ItemId::parse("01BX5ZZKBKACTAV9WEVGEMMVR9").unwrap();
    let tied: Vec<ItemId> = [
        "01BX5ZZKBKACTAV9WEVGEMMVR0",
        "01BX5ZZKBKACTAV9WEVGEMMVR1",
        "01BX5ZZKBKACTAV9WEVGEMMVR2",
        "01BX5ZZKBKACTAV9WEVGEMMVR3",
    ]
    .iter()
    .map(|s| ItemId::parse(s).unwrap())
    .collect();

    backend
        .apply(fx::admit_txn_embedded(
            &scope,
            fx::item_with_id(&scope, exact.clone(), probe_body),
        ))
        .await
        .unwrap();
    // Insertion order 2, 3, 0, 1: the two ids that must survive the tie-break
    // are the two inserted *last*, so a backend that truncates in natural row
    // order keeps the wrong pair every time rather than sometimes.
    for i in [2usize, 3, 0, 1] {
        backend
            .apply(fx::admit_txn_embedded(
                &scope,
                fx::item_with_id(&scope, tied[i].clone(), tied_body),
            ))
            .await
            .unwrap();
    }

    let probe = fx::embedder().embed(probe_body).unwrap();
    let hits = backend.neighbours(&scope, &probe, 3).await.unwrap();

    assert_eq!(
        hits.len(),
        3,
        "k is 3 over a corpus of 5; neighbours must return exactly k"
    );
    let got: BTreeSet<ItemId> = hits.into_iter().map(|h| h.item.id).collect();
    let expected: BTreeSet<ItemId> = [exact, tied[0].clone(), tied[1].clone()]
        .into_iter()
        .collect();
    assert_eq!(
        got, expected,
        "with a four-way tie straddling k, the two lowest ItemIds must survive. \
         A backend that truncates at k before applying the tie-break returns a \
         different set — here, the two tied items it happened to reach first."
    );
}

/// Vectors from a different embedder must be refused, not silently compared.
pub async fn cross_model_vectors_are_rejected<F: BackendFactory>(factory: &F) {
    use memorysafe_embed::DeterministicEmbedder;
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    backend
        .apply(fx::admit_txn_embedded(
            &scope,
            fx::item(&scope, "stored with the 256-dim model"),
        ))
        .await
        .unwrap();

    let other = DeterministicEmbedder::new(384)
        .embed("a probe from another model")
        .unwrap();
    let result = backend.neighbours(&scope, &other, 5).await;

    match result {
        Err(crate::BackendError::EmbedderMismatch { .. }) => {}
        Ok(hits) => assert!(
            hits.is_empty(),
            "cross-model probe returned {} hits instead of erroring or returning nothing",
            hits.len()
        ),
        Err(e) => panic!("unexpected error {e:?}"),
    }
}

/// A recall bumps the access statistics of the items it names, and a later
/// retrieve reflects them.
///
/// **The implementations this exists to reject.** Three, and the corpus is
/// built so each fails a different assertion:
///
/// 1. A backend that never writes the statistics at all — the two columns both
///    backend schemas already declare, which nothing reads or writes today.
///    It fails on the recalled item's `access_count`.
/// 2. A backend that seeds `last_accessed_at` from `created_at` rather than
///    leaving it unset. `fx::item` pins `created_at` to `UNIX_EPOCH`, so
///    "never accessed" and "accessed at creation" would be *literally the same
///    value* in every fixture in this suite — which is why the assertion below
///    is `is_none()`, not a comparison against some expected timestamp. There
///    is no timestamp that could distinguish them.
/// 3. A backend that bumps every item in the scope rather than the ones
///    `record.items` names — the natural shape if the increment is written as
///    a scope-wide `UPDATE` alongside the audit insert. The untouched item is
///    in the corpus solely to catch it, and it is retrievable by the same
///    query, so it cannot be missed for want of matching.
///
/// The recall's `at` is deliberately not `UNIX_EPOCH`: `last_accessed_at` must
/// come from the audit record's own `at` (see `Backend::record_recall`), and a
/// recall stamped at the epoch would agree with a backend that ignored `at`
/// and wrote `created_at` instead.
pub async fn recall_updates_access_statistics<F: BackendFactory>(factory: &F) {
    use memorysafe_core::{Actor, AuditEvent, AuditRecord, ItemRef};

    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let recalled = fx::item(&scope, "the cat sat on the mat");
    let untouched = fx::item(&scope, "the cat sat on a rug");
    for item in [recalled.clone(), untouched.clone()] {
        backend
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }

    let before = backend
        .retrieve_candidates(&scope, &query("cat", HardFilters::default()))
        .await
        .unwrap();
    assert_eq!(
        before.len(),
        2,
        "both items must be retrievable before the recall, or the comparison \
         after it proves nothing"
    );
    for c in &before {
        assert_eq!(
            c.access_count, 0,
            "an item that has never been recalled has access_count 0"
        );
        assert!(
            c.last_accessed_at.is_none(),
            "a never-accessed item is (None, 0), never (created_at, 0): got {:?}",
            c.last_accessed_at
        );
    }

    // One recall, naming exactly one of the two items.
    let at = OffsetDateTime::UNIX_EPOCH + Duration::seconds(3_600);
    backend
        .record_recall(AuditRecord::new(
            scope.clone(),
            AuditEvent::Recalled,
            vec![ItemRef::from_item(&recalled)],
            Actor::system(),
            at,
        ))
        .await
        .unwrap();

    let after = backend
        .retrieve_candidates(&scope, &query("cat", HardFilters::default()))
        .await
        .unwrap();
    assert_eq!(after.len(), 2, "the recall removed an item from the corpus");

    let hit = after
        .iter()
        .find(|c| c.item.id == recalled.id)
        .expect("the recalled item must still be retrievable");
    assert_eq!(
        hit.access_count, 1,
        "record_recall did not increment the recalled item's access_count"
    );
    assert_eq!(
        hit.last_accessed_at,
        Some(at),
        "last_accessed_at must be the audit record's own `at`, not a clock read \
         and not created_at"
    );

    let skipped = after
        .iter()
        .find(|c| c.item.id == untouched.id)
        .expect("the item the recall did not name must still be retrievable");
    assert_eq!(
        skipped.access_count, 0,
        "record_recall bumped an item its record never referenced"
    );
    assert!(
        skipped.last_accessed_at.is_none(),
        "record_recall stamped an item its record never referenced: {:?}",
        skipped.last_accessed_at
    );
}
