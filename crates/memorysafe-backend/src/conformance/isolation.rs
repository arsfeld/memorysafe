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
