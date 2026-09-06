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
