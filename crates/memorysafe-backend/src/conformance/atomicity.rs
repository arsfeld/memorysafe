use super::{BackendFactory, fx};
use crate::{Backend, BackendError, Page};
use memorysafe_core::{AuditFilter, ItemId, Scope};

/// The item insert, the evictions, and the audit row must land together.
pub async fn admit_evict_and_audit_commit_together<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let old = fx::item(&scope, "the old memory");
    backend
        .apply(fx::admit_txn(&scope, old.clone(), None))
        .await
        .unwrap();

    let new = fx::item(&scope, "the new memory");
    let mut txn = fx::admit_txn(&scope, new.clone(), None);
    txn.evictions = vec![old.id.clone()];
    let applied = backend.apply(txn).await.unwrap();

    assert_eq!(applied.evicted, vec![old.id.clone()]);
    assert!(
        backend.get(&scope, &old.id).await.unwrap().is_none(),
        "eviction did not happen"
    );
    assert!(
        backend.get(&scope, &new.id).await.unwrap().is_some(),
        "insert did not happen"
    );

    let audit = backend
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap();
    assert_eq!(audit.len(), 2, "expected one audit row per apply");
}

/// A transaction naming a nonexistent merge target must change nothing.
pub async fn a_failed_transaction_leaves_no_trace<F: BackendFactory>(factory: &F) {
    use crate::write::MergeWrite;
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let existing = fx::item(&scope, "survivor");
    backend
        .apply(fx::admit_txn(&scope, existing.clone(), None))
        .await
        .unwrap();
    let audit_before = backend
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap()
        .len();

    let ghost = ItemId::new();
    let mut txn = fx::admit_txn(&scope, fx::item(&scope, "doomed"), None);
    txn.upsert = None;
    txn.merge = Some(MergeWrite {
        target: ghost,
        body: "merged body".into(),
        tags: vec![],
        attrs: Default::default(),
        vector: None,
        byte_size: 11,
    });
    txn.evictions = vec![existing.id.clone()];

    let err = backend.apply(txn).await.unwrap_err();
    assert!(
        matches!(err, BackendError::MergeTargetMissing(_)),
        "got {err:?}"
    );

    assert!(
        backend.get(&scope, &existing.id).await.unwrap().is_some(),
        "a failed transaction still evicted an item"
    );
    let audit_after = backend
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap()
        .len();
    assert_eq!(
        audit_before, audit_after,
        "a failed transaction still wrote audit"
    );
}

/// A transaction `WriteTransaction::is_valid` rejects must be rejected by the
/// backend too, **and must leave the corpus exactly as it was**.
///
/// **The implementation this rejects:** one that never calls
/// `txn.is_valid()` and treats the transaction's parts as independent
/// options — `if let Some(w) = txn.upsert { insert }`, then
/// `if let Some(m) = txn.merge { rewrite }`, then `for id in txn.evictions
/// { delete }`. That is the shape of a straightforward `apply`, and nothing
/// in this suite has ever handed it a transaction where those parts
/// contradict each other: `is_valid` is exercised only by
/// `memorysafe-backend`'s own unit tests, which call it directly and never
/// go through the trait. Such a backend inserts the new item *and* rewrites
/// the merge target *and* performs the evictions, then reports success.
///
/// Both halves are load-bearing. A backend that validates, returns
/// `InvalidTransaction`, and has already written half the transaction passes
/// a rejection-only assertion — so the corpus is compared against a baseline
/// read immediately before the failed call, on all three of the axes the
/// transaction touches: the item that must not appear, the merge target's
/// body that must not change, and the eviction that must not happen.
///
/// **The violation chosen is `upsert` and `merge` set together**, out of the
/// three `is_valid` rejects, because it is unambiguous: a transaction cannot
/// both introduce a new item and fold content into an existing one, under any
/// reading. The two scope-disagreement cases are equally invalid but describe
/// a *cross-scope write*, and asserting "nothing was written" for those means
/// asserting it in two scopes, which weakens the assertion into a search.
///
/// **The merge target exists.** If it did not, a backend could reject with
/// `MergeTargetMissing` — a rejection for a reason that has nothing to do
/// with validity — and the test would certify a backend that never validates
/// anything. Seeding it also gives the "nothing changed" half something to
/// observe: a body that must still read as it did.
///
/// **Vacuous if** the corpus is empty when the invalid transaction is
/// submitted (there is then nothing for a non-validating backend to damage,
/// and every "unchanged" assertion holds trivially), or if the merge target
/// is a fresh `ItemId::new()` (see above), or if the transaction is softened
/// until `is_valid()` accepts it — at which point `apply` may legitimately
/// succeed and `unwrap_err` panics rather than the test passing. The fixture
/// therefore seeds two items, asserts the baseline is non-empty *before* the
/// failed call, and asserts `!txn.is_valid()` locally so the premise is
/// checked rather than assumed.
pub async fn an_invalid_transaction_is_rejected_and_writes_nothing<F: BackendFactory>(factory: &F) {
    use crate::write::MergeWrite;
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    // The merge target and a second item for the invalid transaction to try
    // to evict. Both must survive untouched.
    let target = fx::item(&scope, "the merge target");
    let bystander = fx::item(&scope, "not part of any of this");
    for item in [target.clone(), bystander.clone()] {
        backend
            .apply(fx::admit_txn(&scope, item, None))
            .await
            .unwrap();
    }

    let before = backend.list(&scope, &Page::default()).await.unwrap();
    assert_eq!(
        before.len(),
        2,
        "the corpus must exist before a 'nothing changed' comparison means anything"
    );
    let audit_before = backend
        .audit(&scope, &AuditFilter::default())
        .await
        .unwrap()
        .len();

    // Upsert and merge together — `WriteTransaction::is_valid` rejects this
    // outright — plus an eviction, so a backend that applies the parts
    // independently damages the corpus in three distinguishable ways.
    let newcomer = fx::item(&scope, "must never be written");
    let mut txn = fx::admit_txn(&scope, newcomer.clone(), None);
    txn.merge = Some(MergeWrite {
        target: target.id.clone(),
        body: "a body the merge target must never acquire".into(),
        tags: vec![],
        attrs: Default::default(),
        vector: None,
        byte_size: 41,
    });
    txn.evictions = vec![bystander.id.clone()];
    assert!(
        !txn.is_valid(),
        "the premise of this test: the transaction it submits must be one \
         `WriteTransaction::is_valid` rejects"
    );

    let err = backend.apply(txn).await.unwrap_err();
    assert!(
        matches!(err, BackendError::InvalidTransaction(_)),
        "an invalid transaction must be refused as invalid, not applied and \
         not refused for some incidental reason: got {err:?}"
    );

    // Nothing was written: no new item, no rewritten body, no eviction, no
    // audit row.
    assert!(
        backend.get(&scope, &newcomer.id).await.unwrap().is_none(),
        "a rejected transaction still inserted its item"
    );
    let survivor = backend
        .get(&scope, &target.id)
        .await
        .unwrap()
        .expect("a rejected transaction deleted the merge target");
    assert_eq!(
        survivor.body, target.body,
        "a rejected transaction still applied its merge"
    );
    assert!(
        backend.get(&scope, &bystander.id).await.unwrap().is_some(),
        "a rejected transaction still applied its evictions"
    );
    let after = backend.list(&scope, &Page::default()).await.unwrap();
    assert_eq!(
        after.len(),
        before.len(),
        "the corpus changed size across a rejected transaction"
    );
    assert_eq!(
        backend
            .audit(&scope, &AuditFilter::default())
            .await
            .unwrap()
            .len(),
        audit_before,
        "a rejected transaction still wrote its audit row"
    );
}

/// Invariant 4 from the spec, at the backend level.
pub async fn every_mutation_writes_exactly_one_audit_record<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    for i in 0..5 {
        backend
            .apply(fx::admit_txn(
                &scope,
                fx::item(&scope, &format!("memory {i}")),
                None,
            ))
            .await
            .unwrap();
    }
    let items = backend.list(&scope, &Page::default()).await.unwrap();
    backend
        .apply(fx::evict_txn(&scope, vec![items[0].id.clone()]))
        .await
        .unwrap();

    let audit = backend
        .audit(
            &scope,
            &AuditFilter {
                limit: 1000,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        audit.len(),
        6,
        "5 admits + 1 eviction should be 6 audit rows"
    );
}

/// A retried write returns the original outcome instead of admitting a
/// duplicate and evicting something to make room for it.
pub async fn idempotent_writes_replay_the_original_outcome<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let item = fx::item(&scope, "written once");
    let mut txn = fx::admit_txn(&scope, item.clone(), None);
    txn.idempotency_key = Some("key-1".into());
    txn.payload_digest = Some(item.digest());

    let first = backend.apply(txn.clone()).await.unwrap();
    assert!(!first.replayed);

    // Same key, same payload, but a fresh item id — as a real retry would look.
    let retry_item = {
        let mut i = fx::item(&scope, "written once");
        i.id = ItemId::new();
        i
    };
    let mut retry = fx::admit_txn(&scope, retry_item, None);
    retry.idempotency_key = Some("key-1".into());
    retry.payload_digest = Some(item.digest());

    let second = backend.apply(retry).await.unwrap();
    assert!(second.replayed, "retry was not recognised as a replay");
    assert_eq!(
        second.item_id, first.item_id,
        "replay returned a different item"
    );

    assert_eq!(
        backend.list(&scope, &Page::default()).await.unwrap().len(),
        1,
        "the retry created a duplicate"
    );
}

/// Reusing a key with a different payload is a conflict, not a silent replay.
pub async fn idempotency_conflict_on_different_payload<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    let first_item = fx::item(&scope, "original payload");
    let mut txn = fx::admit_txn(&scope, first_item.clone(), None);
    txn.idempotency_key = Some("key-2".into());
    txn.payload_digest = Some(first_item.digest());
    backend.apply(txn).await.unwrap();

    let other = fx::item(&scope, "a completely different payload");
    let mut conflicting = fx::admit_txn(&scope, other.clone(), None);
    conflicting.idempotency_key = Some("key-2".into());
    conflicting.payload_digest = Some(other.digest());

    let err = backend.apply(conflicting).await.unwrap_err();
    assert!(
        matches!(err, BackendError::IdempotencyConflict),
        "got {err:?}"
    );
}
