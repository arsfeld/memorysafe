use super::{BackendFactory, fx};
use crate::{Backend, Page};
use memorysafe_core::Scope;

/// Two tenants writing identical content must never see each other's items.
pub async fn tenants_are_isolated<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let a = Scope::new("tenant-a", "sub", "ns").unwrap();
    let b = Scope::new("tenant-b", "sub", "ns").unwrap();

    let item_a = fx::item(&a, "tenant a private note");
    backend
        .apply(fx::admit_txn(&a, item_a.clone(), None))
        .await
        .unwrap();

    // The emptiness assertions below only mean something if the write
    // actually landed under tenant a — otherwise a no-op `apply`, or one
    // that silently dropped the row, would pass this test too.
    let listed_a = backend.list(&a, &Page::default()).await.unwrap();
    assert!(
        listed_a.iter().any(|i| i.id == item_a.id),
        "tenant a lost its own item"
    );
    assert_eq!(
        backend
            .get(&a, &item_a.id)
            .await
            .unwrap()
            .as_ref()
            .map(|i| &i.id),
        Some(&item_a.id),
        "tenant a could not fetch its own item by id"
    );

    let listed_b = backend.list(&b, &Page::default()).await.unwrap();
    assert!(
        listed_b.is_empty(),
        "tenant b saw {} of tenant a's items",
        listed_b.len()
    );

    assert!(
        backend.get(&b, &item_a.id).await.unwrap().is_none(),
        "tenant b fetched tenant a's item by id"
    );
}

/// Subjects within one tenant are the right-to-delete unit, so they must be
/// separated just as strictly.
///
/// This test demonstrates that for `get` and `list`. The two methods that
/// answer a recall — `retrieve_candidates` and `neighbours` — are covered by
/// `retrieval_never_crosses_a_scope_boundary` below, which was added when it
/// turned out this module's claim of strict separation had never been checked
/// against the read path a caller actually reaches.
pub async fn subjects_are_isolated<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let a = Scope::new("t", "subject-a", "ns").unwrap();
    let b = Scope::new("t", "subject-b", "ns").unwrap();

    let item_a = fx::item(&a, "subject a note");
    backend
        .apply(fx::admit_txn(&a, item_a.clone(), None))
        .await
        .unwrap();

    // As in `tenants_are_isolated`: prove subject a actually has the item
    // before trusting that subject b's emptiness means isolation and not a
    // dropped write.
    let listed_a = backend.list(&a, &Page::default()).await.unwrap();
    assert!(
        listed_a.iter().any(|i| i.id == item_a.id),
        "subject a lost its own item"
    );
    assert_eq!(
        backend
            .get(&a, &item_a.id)
            .await
            .unwrap()
            .as_ref()
            .map(|i| &i.id),
        Some(&item_a.id),
        "subject a could not fetch its own item by id"
    );

    assert!(backend.list(&b, &Page::default()).await.unwrap().is_empty());
    assert!(backend.get(&b, &item_a.id).await.unwrap().is_none());
}

/// Namespaces are the budget and retrieval-default unit within a subject.
pub async fn namespaces_are_separated<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let a = Scope::new("t", "s", "ns-a").unwrap();
    let b = Scope::new("t", "s", "ns-b").unwrap();

    backend
        .apply(fx::admit_txn(&a, fx::item(&a, "in ns a"), None))
        .await
        .unwrap();
    backend
        .apply(fx::admit_txn(&b, fx::item(&b, "in ns b"), None))
        .await
        .unwrap();

    assert_eq!(backend.list(&a, &Page::default()).await.unwrap().len(), 1);
    assert_eq!(backend.list(&b, &Page::default()).await.unwrap().len(), 1);
}

/// An audit query is scoped too — one subject's decisions are not another's.
pub async fn audit_is_scoped<F: BackendFactory>(factory: &F) {
    use memorysafe_core::AuditFilter;
    let backend = factory.create().await;
    let a = Scope::new("t", "subject-a", "ns").unwrap();
    let b = Scope::new("t", "subject-b", "ns").unwrap();

    backend
        .apply(fx::admit_txn(&a, fx::item(&a, "note"), None))
        .await
        .unwrap();

    assert_eq!(
        backend
            .audit(&a, &AuditFilter::default())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        backend
            .audit(&b, &AuditFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
}

/// Neither retrieval method may return an item from outside the scope it was
/// given — not another tenant's, not another subject's, not another
/// namespace's.
///
/// **The implementation this rejects:** a retrieval query whose scope
/// predicate is absent, incomplete, or bound to the wrong parameter — `WHERE
/// subject = ?1` with `?1` bound to the namespace, a vector search that joins
/// items to vectors and filters only the vector table, a hybrid backend whose
/// keyword arm is scoped and whose vector arm is not. Such a backend returns
/// another subject's memories in a recall, and returns them rather than
/// erroring, so the caller gets someone else's data with no signal.
///
/// **It passed every other test in this suite.** The two methods above are the
/// ones a recall actually reaches, and until this test nothing exercised them
/// across a scope boundary: every test in `conformance::retrieval` builds
/// exactly one scope — `Scope::new("t", "s", "n")` — so none of them can
/// observe a leak, and this module, which does build several scopes, called
/// only `get`, `list` and `audit`. The claim on `subjects_are_isolated` above
/// was therefore broader than its evidence.
///
/// **Why it matters more for Postgres than for SQLite, which is what a
/// conformance suite is for.** Tenant isolation on SQLite is structural — one
/// database file per tenant — so a SQLite backend passes the tenant half
/// without having written any code for it. Subject and namespace isolation is
/// a **query predicate in both backends**, and nothing structural prevents
/// either leak. The tenant case is included anyway, because what is free on
/// one backend is not on the other.
///
/// **Four combinations in one test — two methods across two dimensions, plus
/// tenant — and every assertion names its own method and dimension**, so a
/// failure says which of the four broke rather than only that one did.
/// `retrieve_candidates` and `neighbours` are separate query paths; this
/// round established repeatedly that covering one does not cover the other,
/// and a backend can carry the subject predicate while dropping the namespace
/// one.
///
/// **Vacuity, and this is the fixture detail that decides whether the test
/// works: it passes vacuously if the foreign items rank below the limit.** A
/// leaking backend that truncates at `limit` or at `k` would then drop the
/// leaked rows by luck and pass against exactly the implementation this test
/// exists to catch. So the foreign items are worded as *exact* matches for the
/// probe and the home item is only a partial one: the leak, if present, is
/// ranked first and cannot hide behind truncation, and `k` is set well above
/// the whole corpus so nothing is truncated at all. This is
/// `neighbours_break_ties_before_truncating_at_k`'s property — a wrong backend
/// fails every time rather than sometimes.
///
/// The other vacuity condition is the ordinary one: every "no foreign item
/// present" assertion is satisfied by a backend that returns nothing, so each
/// call also asserts the home scope's **exact** expected count, and the three
/// foreign corpora are read back from their own scopes so the test cannot pass
/// because the writes silently failed.
pub async fn retrieval_never_crosses_a_scope_boundary<F: BackendFactory>(factory: &F) {
    use crate::query::{CandidateQuery, HardFilters};
    use memorysafe_core::ScoredCandidate;
    use memorysafe_embed::Embedder;

    let backend = factory.create().await;

    // One home scope and three neighbours, each differing in exactly one
    // component, so a predicate that omits any one of the three is caught by
    // the neighbour that varies it.
    let home = Scope::new("t", "s", "n").unwrap();
    let dimensions = [
        (Scope::new("other-t", "s", "n").unwrap(), "tenant"),
        (Scope::new("t", "other-s", "n").unwrap(), "subject"),
        (Scope::new("t", "s", "other-n").unwrap(), "namespace"),
    ];

    let probe_text = "the cat sat on the mat";
    let home_body = "the cat sat on a rug";

    backend
        .apply(fx::admit_txn_embedded(&home, fx::item(&home, home_body)))
        .await
        .unwrap();

    // The foreign items are exact matches for the probe and the home item is
    // not, so a backend that ignores scope returns them ahead of the home
    // item. See the vacuity clause: this inversion is the test.
    for (scope, _) in &dimensions {
        backend
            .apply(fx::admit_txn_embedded(scope, fx::item(scope, probe_text)))
            .await
            .unwrap();
    }

    let probe = fx::embedder().embed(probe_text).unwrap();

    let assert_scoped = |method: &str, hits: &[ScoredCandidate]| {
        for (scope, dimension) in &dimensions {
            let leaked: Vec<String> = hits
                .iter()
                .filter(|c| c.item.scope == *scope)
                .map(|c| c.item.body.clone())
                .collect();
            assert!(
                leaked.is_empty(),
                "{method} leaked across the {dimension} boundary: it returned \
                 {leaked:?} from {}, which is not the scope it was given. The \
                 scope predicate is missing, incomplete, or bound to the wrong \
                 parameter",
                scope.key()
            );
        }
    };

    // `retrieve_candidates`, hybrid: the query carries both a vector and text,
    // so a backend whose arms are scoped inconsistently is caught here.
    let hits = backend
        .retrieve_candidates(
            &home,
            &CandidateQuery {
                embedding: Some(probe.clone()),
                text: Some(probe_text.to_string()),
                filters: HardFilters::default(),
                limit: 50,
            },
        )
        .await
        .unwrap();
    assert_scoped("retrieve_candidates", &hits);
    assert_eq!(
        hits.len(),
        1,
        "retrieve_candidates: the home scope holds exactly one item. Asserting \
         the count is what stops 'no foreign item present' from being satisfied \
         by an empty result"
    );
    assert_eq!(hits[0].item.body, home_body);

    // `neighbours`, vector-only, with `k` far above the whole corpus so a leak
    // has room to appear rather than being truncated away.
    let neighbours = backend.neighbours(&home, &probe, 10).await.unwrap();
    assert_scoped("neighbours", &neighbours);
    assert_eq!(
        neighbours.len(),
        1,
        "neighbours: the home scope holds exactly one item, and an empty result \
         would satisfy every leak assertion above without proving anything"
    );

    // The foreign corpora are readable from their own scopes, so no assertion
    // above passed because a write silently failed.
    for (scope, dimension) in &dimensions {
        let theirs = backend.neighbours(scope, &probe, 10).await.unwrap();
        assert_eq!(
            theirs.len(),
            1,
            "the other-{dimension} corpus must exist, or 'nothing leaked from \
             {}' is true of a backend that never stored anything there",
            scope.key()
        );
        assert_eq!(theirs[0].item.body, probe_text);
    }
}
