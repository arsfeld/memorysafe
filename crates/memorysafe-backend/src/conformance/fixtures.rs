use crate::write::{ItemWrite, WriteTransaction};
use memorysafe_core::{
    Actor, AuditEvent, AuditRecord, ItemId, ItemRef, MemoryItem, Protection, Scope,
    SensitivityLevel, Source, SourceKind,
};
use memorysafe_embed::{DeterministicEmbedder, Embedder, QuantizedVector};
use time::OffsetDateTime;

/// The one embedder the whole suite uses. Deterministic, no model files.
pub fn embedder() -> DeterministicEmbedder {
    DeterministicEmbedder::new(256)
}

pub fn item(scope: &Scope, body: &str) -> MemoryItem {
    MemoryItem {
        id: ItemId::new(),
        scope: scope.clone(),
        body: body.to_string(),
        kind: "fact".into(),
        source: Source {
            kind: SourceKind::Agent,
            id: Some("conformance".into()),
        },
        occurred_at: None,
        // Fixed, not `now_utc()`: `admit_txn` below feeds this straight into
        // the audit record's `at`, and `AuditRecord::new` documents that `at`
        // is supplied rather than sampled so replays are comparable. A clock
        // read here would make every conformance run's audit rows different
        // from the last.
        created_at: OffsetDateTime::UNIX_EPOCH,
        tags: vec![],
        attrs: Default::default(),
        sensitivity: SensitivityLevel::Internal,
        ttl: None,
        protection: Protection::Normal,
        pending_embedding: false,
    }
}

pub fn item_with(
    scope: &Scope,
    body: &str,
    kind: &str,
    tags: &[&str],
    sensitivity: SensitivityLevel,
) -> MemoryItem {
    let mut i = item(scope, body);
    i.kind = kind.to_string();
    i.tags = tags.iter().map(|t| t.to_string()).collect();
    i.sensitivity = sensitivity;
    i
}

pub fn vector_for(body: &str) -> QuantizedVector {
    QuantizedVector::from_embedding(&embedder().embed(body).unwrap())
}

/// A transaction that admits one item, with its audit record already attached.
pub fn admit_txn(
    scope: &Scope,
    item: MemoryItem,
    vector: Option<QuantizedVector>,
) -> WriteTransaction {
    // The audit fires at the same instant the item was created, not a fresh
    // clock read — `at` is supplied, never sampled, per the convention
    // `AuditRecord::new` documents.
    let audit = AuditRecord::new(
        scope.clone(),
        AuditEvent::Admitted,
        vec![ItemRef::from_item(&item)],
        Actor::system(),
        item.created_at,
    );
    let mut txn = WriteTransaction::new(scope.clone(), audit);
    txn.upsert = Some(ItemWrite { item, vector });
    txn
}

/// Same, but embeds the body so the item is vector-searchable.
pub fn admit_txn_embedded(scope: &Scope, item: MemoryItem) -> WriteTransaction {
    let v = vector_for(&item.body);
    admit_txn(scope, item, Some(v))
}

/// A transaction that evicts items without inserting anything.
pub fn evict_txn(scope: &Scope, evictions: Vec<ItemId>) -> WriteTransaction {
    let audit = AuditRecord::new(
        scope.clone(),
        AuditEvent::Forgotten,
        vec![],
        Actor::system(),
        OffsetDateTime::UNIX_EPOCH,
    );
    let mut txn = WriteTransaction::new(scope.clone(), audit);
    txn.evictions = evictions;
    txn
}

#[cfg(test)]
mod tests {
    use super::*;

    // Task 14's `WriteTransaction::is_valid()` rejects a transaction whose
    // upserted item, or whose audit record, disagrees with the transaction's
    // own scope — because a real backend reads different ones for different
    // rows. These fixtures are exactly what every conformance test builds
    // its transactions from, so a scope bug here is invisible until Task 20
    // runs them against a real backend. Assert validity now, at the one
    // point this crate can actually execute anything.

    fn scope() -> Scope {
        Scope::new("t", "s", "n").unwrap()
    }

    #[test]
    fn admit_txn_produces_a_valid_transaction() {
        let s = scope();
        assert!(admit_txn(&s, item(&s, "a note"), None).is_valid());
    }

    #[test]
    fn admit_txn_with_a_vector_produces_a_valid_transaction() {
        let s = scope();
        let v = vector_for("a note");
        assert!(admit_txn(&s, item(&s, "a note"), Some(v)).is_valid());
    }

    #[test]
    fn admit_txn_embedded_produces_a_valid_transaction() {
        let s = scope();
        assert!(admit_txn_embedded(&s, item(&s, "a note")).is_valid());
    }

    #[test]
    fn admit_txn_with_an_item_with_variant_produces_a_valid_transaction() {
        let s = scope();
        let i = item_with(
            &s,
            "a note",
            "preference",
            &["work", "urgent"],
            SensitivityLevel::Personal,
        );
        assert!(admit_txn(&s, i, None).is_valid());
    }

    #[test]
    fn evict_txn_produces_a_valid_transaction() {
        let s = scope();
        assert!(evict_txn(&s, vec![ItemId::new()]).is_valid());
    }

    // Task 16 introduces three transaction shapes `is_valid()` has never
    // seen exercised: an admit that also carries evictions, a merge-only
    // transaction, and one carrying idempotency fields. Each is built the
    // same way the atomicity conformance tests build it, so a scope or
    // shape bug here would otherwise stay invisible until Task 20 runs a
    // real backend against them.

    #[test]
    fn a_transaction_carrying_evictions_is_valid() {
        let s = scope();
        let mut txn = admit_txn(&s, item(&s, "the new memory"), None);
        txn.evictions = vec![ItemId::new()];
        assert!(txn.is_valid());
    }

    #[test]
    fn a_merge_only_transaction_is_valid() {
        use crate::write::MergeWrite;

        let s = scope();
        let mut txn = admit_txn(&s, item(&s, "doomed"), None);
        txn.upsert = None;
        txn.merge = Some(MergeWrite {
            target: ItemId::new(),
            body: "merged body".into(),
            tags: vec![],
            attrs: Default::default(),
            vector: None,
            byte_size: 11,
        });
        assert!(txn.is_valid());
    }

    #[test]
    fn a_transaction_with_an_idempotency_key_and_payload_digest_is_valid() {
        let s = scope();
        let i = item(&s, "written once");
        let mut txn = admit_txn(&s, i.clone(), None);
        txn.idempotency_key = Some("key-1".into());
        txn.payload_digest = Some(i.digest());
        // `is_valid()` never inspects `idempotency_key` or `payload_digest`,
        // so this test passes identically whether or not those fields are
        // set — no mutation on either field is demonstrable here. It is kept
        // as a guard against a future `is_valid()` that does inspect them,
        // not as evidence of coverage today.
        assert!(txn.is_valid());
    }
}
