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

/// Two pages must not overlap or drop rows.
///
/// `fx::item` pins every `created_at` to `OffsetDateTime::UNIX_EPOCH` so most
/// conformance runs are deterministic — see its doc comment. That is exactly
/// wrong for this test: with all 25 rows tied on `created_at`, a `list()`
/// that orders by timestamp alone (the natural choice) sorts them in no
/// stable order at all, and `LIMIT`/`OFFSET` over an unstable sort can
/// return the same row on two pages while dropping another entirely. That
/// would fail here looking like a backend bug when it is really a fixture
/// bug — the corpus, not the backend, created the tie. `fx::item_at` gives
/// each item its own increasing timestamp so this corpus contains no ties.
/// `Backend::list`'s doc comment (`memorysafe-backend/src/lib.rs`) still
/// requires implementations to break ties with a unique key, because a real
/// corpus (a bulk import, say) can absolutely produce them — this fixture
/// change makes the present test deterministic, it does not remove that
/// requirement.
pub async fn pagination_is_stable<F: BackendFactory>(factory: &F) {
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
