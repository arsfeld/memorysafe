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
/// The corpus here deliberately exceeds the query's `limit`, with the
/// excluded items worded to dominate the ranking. That is not incidental:
/// with a corpus smaller than `limit` (the original shape of this test — one
/// item per level, `limit: 50`), a backend that applies the sensitivity
/// ceiling inside its query and a backend that runs an unfiltered query and
/// filters the ceiling out afterwards, in Rust, return byte-identical
/// results, because nothing was ever truncated. The two strategies are
/// indistinguishable and the test cannot tell a compliant backend from a
/// dangerous one — which defeats the point of a test named for the ceiling
/// being "enforced in the query". Do not "simplify" this back to one item
/// per level; that silently removes the only thing this test actually
/// checks.
///
/// Five `Restricted` items are worded to match the query text almost
/// verbatim, so they rank at the very top for any reasonable vector or
/// keyword scoring. Two admissible items mention the same topic only in
/// passing, so they rank lower. Querying with `limit: 3` — fewer than the
/// five `Restricted` items — means: a backend that ranks first and applies
/// the sensitivity filter after `LIMIT` fills all 3 slots with `Restricted`
/// rows and returns 0 allowed items; a backend that filters inside the query
/// never considers the `Restricted` rows at all and returns exactly the 2
/// items the caller is entitled to.
pub async fn sensitivity_ceiling_is_enforced_in_the_query<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // Five distinct items (fresh `ItemId`s), identical wording, so each one
    // ranks at the very top of this query.
    for _ in 0..5 {
        let item = fx::item_with(
            &scope,
            "restricted medical record about cats",
            "fact",
            &[],
            SensitivityLevel::Restricted,
        );
        backend
            .apply(fx::admit_txn_embedded(&scope, item))
            .await
            .unwrap();
    }
    for (body, level) in [
        ("a passing mention of cats", SensitivityLevel::Public),
        (
            "cats came up once in conversation",
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
        "expected exactly the 2 admissible items; fewer means the sensitivity \
         ceiling was applied after LIMIT instead of inside the query, and the \
         Restricted rows consumed the limit's slots"
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

/// FTS5 syntax characters in user text must not become query operators.
///
/// The corpus here holds three items, not one. With only one item present —
/// the original shape of this test — "matched everything" and "matched
/// nothing" are the same observation: both produce a result set of size
/// `<= 1`, so the assertion below cannot tell a backend that correctly
/// treats hostile text as an inert literal from one that lets it become an
/// operator and match the whole scope. Three items make the two outcomes
/// different sizes (`<= 1` vs. `3`), so an unescaped hostile string that
/// turns into a match-everything query is actually caught.
pub async fn keyword_search_escapes_user_input<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    for body in [
        "a normal memory",
        "another normal memory",
        "a third normal memory",
    ] {
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
