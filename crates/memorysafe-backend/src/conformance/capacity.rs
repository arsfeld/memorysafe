use super::{BackendFactory, fx};
use crate::{Backend, Page};
use memorysafe_core::{Budget, Scope};

pub async fn capacity_accounting_tracks_items_and_bytes<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(100),
                max_bytes: Some(100_000),
            },
        )
        .await
        .unwrap();

    let before = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(before.used_items, 0);
    assert_eq!(before.used_bytes, 0);

    let item = fx::item(&scope, "a memory of some length");
    let size = item.byte_size();
    backend
        .apply(fx::admit_txn(&scope, item, None))
        .await
        .unwrap();

    let after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(after.used_items, 1);
    assert_eq!(after.used_bytes, size);
    assert_eq!(after.budget.max_items, Some(100));
}

pub async fn eviction_releases_capacity<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(10),
                max_bytes: None,
            },
        )
        .await
        .unwrap();

    for i in 0..3 {
        backend
            .apply(fx::admit_txn(
                &scope,
                fx::item(&scope, &format!("memory {i}")),
                None,
            ))
            .await
            .unwrap();
    }
    assert_eq!(backend.capacity_state(&scope).await.unwrap().used_items, 3);

    let items = backend.list(&scope, &Page::default()).await.unwrap();
    backend
        .apply(fx::evict_txn(&scope, vec![items[0].id.clone()]))
        .await
        .unwrap();

    let after = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(after.used_items, 2);
    assert_eq!(
        after.used_bytes,
        items[1].byte_size() + items[2].byte_size()
    );
}

/// The correctness detail the spec calls out: without a lock on the accounting
/// row, concurrent admits both conclude there is room and the count drifts.
///
/// `F::B: 'static` is required, not part of the original test logic: without
/// it, `tokio::spawn` cannot accept a future closing over `Arc<F::B>`, since
/// nothing in `BackendFactory` otherwise promises the backend outlives the
/// borrow of `factory`. Every real backend (SQLite's own connection, a
/// Postgres pool) owns its state and satisfies this trivially.
pub async fn concurrent_admits_do_not_double_count<F: BackendFactory>(factory: &F)
where
    F::B: 'static,
{
    use std::sync::Arc;
    let backend = Arc::new(factory.create().await);
    let scope = Scope::new("t", "s", "n").unwrap();
    backend
        .set_budget(
            &scope,
            Budget {
                max_items: Some(1000),
                max_bytes: None,
            },
        )
        .await
        .unwrap();

    let mut handles = Vec::new();
    for i in 0..20 {
        let b = Arc::clone(&backend);
        let s = scope.clone();
        handles.push(tokio::spawn(async move {
            b.apply(fx::admit_txn(
                &s,
                fx::item(&s, &format!("concurrent {i}")),
                None,
            ))
            .await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }

    let state = backend.capacity_state(&scope).await.unwrap();
    assert_eq!(
        state.used_items, 20,
        "capacity accounting drifted under concurrency"
    );

    let listed = backend
        .list(
            &scope,
            &Page {
                offset: 0,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        listed.len() as u64,
        state.used_items,
        "accounting disagrees with reality"
    );
}

pub async fn scope_stats_reflect_the_corpus<F: BackendFactory>(factory: &F) {
    let backend = factory.create().await;
    let scope = Scope::new("t", "s", "n").unwrap();

    assert_eq!(backend.scope_stats(&scope).await.unwrap().item_count, 0);

    for body in ["first memory", "second memory", "third memory"] {
        backend
            .apply(fx::admit_txn_embedded(&scope, fx::item(&scope, body)))
            .await
            .unwrap();
    }

    let stats = backend.scope_stats(&scope).await.unwrap();
    assert_eq!(stats.item_count, 3);
    assert!(stats.total_bytes > 0);
    assert!(stats.median_item_bytes > 0);
}
