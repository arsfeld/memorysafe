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

/// Same as `item`, but with a caller-supplied `created_at` instead of the
/// fixed `UNIX_EPOCH` above.
///
/// `item`'s pinned timestamp makes a corpus built from it **fully tied** under
/// `ORDER BY created_at` — exactly the shape a real bulk import produces. That
/// case is not avoided by this builder; it is covered directly, by
/// `retrieval::list_tie_break_is_total_over_identical_timestamps`, which
/// builds its corpus from `item` (and `item_with_id`) precisely *because*
/// everything ties there and the tie-break is then the only thing ordering the
/// pages.
///
/// This builder exists for the two tests that need the opposite corpus —
/// distinct, increasing timestamps:
///
/// - `retrieval::list_pages_are_disjoint_and_complete`, which isolates the
///   offset/limit arithmetic from ordering entirely: with no ties, no
///   tie-break is involved, so an overlap or a dropped row can only be a
///   paging bug.
/// - `retrieval::list_orders_oldest_first_by_created_at`, which can only
///   observe sort *direction* where the primary key actually varies. On a
///   tied corpus an ascending and a descending backend produce identical
///   output, so direction is unobservable there.
///
/// Use this rather than forking `item` or reaching into its fields directly.
pub fn item_at(scope: &Scope, body: &str, created_at: OffsetDateTime) -> MemoryItem {
    let mut i = item(scope, body);
    i.created_at = created_at;
    i
}

/// Same as `item`, but with a caller-supplied `ItemId` instead of a freshly
/// generated one — `item_at`'s counterpart for the tie-break key.
///
/// **Why any test asserting an id order needs this.** `ItemId::new()` is
/// `ulid::Ulid::generate()`, the plain generator, and nothing in this
/// workspace uses a monotonic one. `ulid_id!`'s own doc comment says
/// lexicographic order equals creation order only "up to the timestamp's
/// millisecond resolution" — so ids minted inside one millisecond are ordered
/// *randomly* relative to each other. A test that inserts items in a tight
/// loop and expects ascending ids is therefore a coin flip, and worse, if the
/// loop happens to straddle a millisecond boundary the ids come out ascending
/// in insertion order and a backend applying no tie-break at all passes
/// deterministically — looking stable while proving nothing.
///
/// Callers pass literal ULIDs through `ItemId::parse`, the same way
/// `memorysafe-core`'s own tests do, and insert them in a deliberately
/// non-ascending order.
pub fn item_with_id(scope: &Scope, id: ItemId, body: &str) -> MemoryItem {
    let mut i = item(scope, body);
    i.id = id;
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

/// Same as `evict_txn`, but with a caller-supplied `at` instead of the fixed
/// `UNIX_EPOCH` above — mirrors `item`/`item_at`. `evict_txn` itself stays
/// pinned so Task 16's tests are unaffected; tests that need distinct,
/// ordered eviction timestamps (Task 18's
/// `audit_filter_narrows_by_event_and_time`) use this builder instead.
pub fn evict_txn_at(scope: &Scope, evictions: Vec<ItemId>, at: OffsetDateTime) -> WriteTransaction {
    let audit = AuditRecord::new(
        scope.clone(),
        AuditEvent::Forgotten,
        vec![],
        Actor::system(),
        at,
    );
    let mut txn = WriteTransaction::new(scope.clone(), audit);
    txn.evictions = evictions;
    txn
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

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

    // `item_at` is the fixture `list_pages_are_disjoint_and_complete` and
    // `list_orders_oldest_first_by_created_at` both depend on for distinct
    // timestamps. A mutation that silently ignores the `created_at` argument
    // (falling back to `item`'s pinned `UNIX_EPOCH`) would make every
    // "distinct, increasing timestamp" in those tests identical again,
    // collapsing both corpora onto the tied one — which would leave sort
    // direction unobservable and the disjointness test unable to separate a
    // paging bug from a tie-break bug. Assert the field directly, not just
    // `is_valid()`: validity never inspects `created_at`, so it cannot catch
    // that mutation.
    #[test]
    fn item_at_uses_the_given_timestamp_not_the_default() {
        let s = scope();
        let t = OffsetDateTime::UNIX_EPOCH + Duration::seconds(42);
        let i = item_at(&s, "a note", t);
        assert_eq!(i.created_at, t);
        assert_ne!(
            i.created_at,
            OffsetDateTime::UNIX_EPOCH,
            "item_at must not silently fall back to the default timestamp"
        );
        assert!(admit_txn(&s, i, None).is_valid());
    }

    // Task 18's `evict_txn_at` is `item_at`'s eviction-side counterpart:
    // `audit_filter_narrows_by_event_and_time` needs the eviction's audit
    // row at its own distinct timestamp, not `evict_txn`'s pinned
    // `UNIX_EPOCH`. Same mutation risk as `item_at`, so the same direct
    // assertion: `is_valid()` never inspects `audit.at`, so only checking
    // validity would pass even if the timestamp argument were silently
    // ignored.
    // `item_with_id` is what makes
    // `list_tie_break_is_total_over_identical_timestamps` and
    // `neighbours_break_ties_before_truncating_at_k` deterministic rather than
    // a coin flip: both assert an order that `ItemId::new()` cannot be relied
    // on to produce, because ULIDs minted inside one millisecond are randomly
    // ordered relative to each other. A builder that silently ignored its `id`
    // argument would hand those tests generated ids again and reintroduce
    // exactly that. `is_valid()` never inspects `id`, so assert the field.
    #[test]
    fn item_with_id_uses_the_given_id_not_a_generated_one() {
        let s = scope();
        let id = ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FA0").unwrap();
        let i = item_with_id(&s, id.clone(), "a note");
        assert_eq!(i.id, id);
        assert!(admit_txn(&s, i, None).is_valid());
    }

    // The premise both id-ordering tests rest on: these literals are valid
    // ULIDs, and their lexicographic order is the ascending numeric order the
    // tests expect. `ItemId` derives `Ord` over the string, so this is the
    // property the assertions there compare against — and it is checkable
    // today, unlike the conformance tests themselves, which do not execute
    // until a backend exists.
    #[test]
    fn the_literal_ulids_the_ordering_tests_use_parse_and_sort_ascending() {
        for family in [
            [
                "01ARZ3NDEKTSV4RRFFQ69G5FA0",
                "01ARZ3NDEKTSV4RRFFQ69G5FA1",
                "01ARZ3NDEKTSV4RRFFQ69G5FA2",
                "01ARZ3NDEKTSV4RRFFQ69G5FA3",
                "01ARZ3NDEKTSV4RRFFQ69G5FA4",
            ],
            [
                "01BX5ZZKBKACTAV9WEVGEMMVR0",
                "01BX5ZZKBKACTAV9WEVGEMMVR1",
                "01BX5ZZKBKACTAV9WEVGEMMVR2",
                "01BX5ZZKBKACTAV9WEVGEMMVR3",
                "01BX5ZZKBKACTAV9WEVGEMMVR9",
            ],
        ] {
            let ids: Vec<ItemId> = family
                .iter()
                .map(|s| ItemId::parse(s).expect("literal must be a canonical ULID"))
                .collect();
            let mut sorted = ids.clone();
            sorted.sort();
            assert_eq!(
                sorted, ids,
                "the literals are written in the order the ordering tests assert; \
                 if they do not sort that way, those tests assert the wrong sequence"
            );
        }
    }

    #[test]
    fn evict_txn_at_uses_the_given_timestamp_not_the_default() {
        let s = scope();
        let t = OffsetDateTime::UNIX_EPOCH + Duration::seconds(42);
        let txn = evict_txn_at(&s, vec![ItemId::new()], t);
        assert_eq!(txn.audit.at, t);
        assert_ne!(
            txn.audit.at,
            OffsetDateTime::UNIX_EPOCH,
            "evict_txn_at must not silently fall back to the default timestamp"
        );
        assert!(txn.is_valid());
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
