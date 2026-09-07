use crate::write::{ItemWrite, WriteTransaction};
use memorysafe_core::{
    Actor, AuditEvent, AuditId, AuditRecord, ItemId, ItemRef, MemoryItem, Protection, Scope,
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

/// The `SubjectPurged` record `Backend::purge_subject` is handed. The engine
/// builds this record; the backend deletes first, then inserts it verbatim
/// under the id it carries — see the echo rule on `Backend`.
///
/// **`id` and `actor` are parameters, not defaults, on purpose.**
/// `lifecycle::purge_subject_persists_the_record_it_was_given` asserts both:
/// against a generated `AuditId::new()` an echoed id and a backend-minted one
/// are indistinguishable, and against `Actor::system()` — which a backend
/// inventing its own record would also plausibly use — so is the actor. `at`
/// is pinned to `UNIX_EPOCH` for the reason `item` pins `created_at`;
/// `AuditRecord::at` is public where a test needs otherwise.
pub fn purge_record(scope: &Scope, id: AuditId, actor: Actor) -> AuditRecord {
    let mut record = AuditRecord::new(
        scope.clone(),
        AuditEvent::SubjectPurged,
        vec![],
        actor,
        OffsetDateTime::UNIX_EPOCH,
    );
    record.id = id;
    record
}

/// The five item ids the ordering tests pin, in ascending order: they share a
/// prefix and differ only in the final character, so `...FA0 < ...FA1 < ... <
/// ...FA4` by inspection.
///
/// A constant rather than a literal inlined at each use, for the reason
/// `AUDIT_ORDER_ULIDS` is one: the premise (these ascend) is checked by
/// `tests::the_literal_ulids_the_ordering_tests_use_parse_and_sort_ascending`,
/// and a consumer that inlines its own copy of the strings is joined to that
/// premise by string equality rather than by a symbol — so editing one and not
/// the other silently detaches the check from the thing it checks.
///
/// `retrieval::list_tie_break_is_total_over_identical_timestamps` inserts all
/// five out of order; `lifecycle::export_orders_the_stream_by_kind_then_by_id`
/// uses the first three.
pub const ITEM_ORDER_ULIDS: [&str; 5] = [
    "01ARZ3NDEKTSV4RRFFQ69G5FA0",
    "01ARZ3NDEKTSV4RRFFQ69G5FA1",
    "01ARZ3NDEKTSV4RRFFQ69G5FA2",
    "01ARZ3NDEKTSV4RRFFQ69G5FA3",
    "01ARZ3NDEKTSV4RRFFQ69G5FA4",
];

/// The four audit ids `lifecycle::audit_filter_narrows_by_event_and_time`
/// pins, in ascending order: three admits, then the eviction.
///
/// `lifecycle::audit_pages_by_the_after_cursor_without_repeating_a_row` builds
/// the same four-record corpus for the same reason — it needs id order and
/// `at` order to disagree — and
/// `lifecycle::export_orders_the_stream_by_kind_then_by_id` uses the first
/// three as audit ids it can insert out of order. Both rely on the ascending
/// order this array is checked for below.
///
/// **Why literals rather than `AuditRecord::new`'s generated ids.** `new` sets
/// `id: AuditId::new()`, the plain ULID generator, while taking `at` as a
/// parameter — so records minted inside one millisecond are ordered
/// *randomly* relative to each other. That test asserts an exact two-element
/// id sequence over records minted microseconds apart, which made its verdict
/// depend on backend speed: it failed roughly half the time against a fast
/// in-memory backend and passed against one with a real fsync between writes.
/// This is `item_with_id`'s argument, on the audit side.
///
/// All four are pinned, not only the two the sequence names. A generated
/// `AuditId` carries today's millisecond timestamp and every literal here
/// carries a 2016 one, so pinning two and generating two would put the
/// generated pair above both literals and reverse the very ordering under
/// test.
pub const AUDIT_ORDER_ULIDS: [&str; 4] = [
    "01ARZ3NDEKTSV4RRFFQ69G5FB0",
    "01ARZ3NDEKTSV4RRFFQ69G5FB1",
    "01ARZ3NDEKTSV4RRFFQ69G5FB2",
    "01ARZ3NDEKTSV4RRFFQ69G5FB3",
];

/// The six audit ids `lifecycle::audit_returns_min_of_the_limit_and_the_rows_that_remain`
/// pins, in ascending order: an admit, then the **eviction**, then four more
/// admits.
///
/// **The eviction is second, not last, and that placement is the test.** That
/// test's sharp half asks for `events: [Forgotten]` under `limit: 3` over six
/// rows, where exactly one row matches. A backend that applies the limit
/// *before* the filter — taking the newest three rows and then filtering them
/// in memory, which is what you get from paging a materialised "recent audit"
/// view — returns nothing, because the newest three are all admits. A
/// backend that filters first and then limits returns the one eviction. With
/// the eviction placed last (newest) the two implementations agree and the
/// test certifies both.
///
/// **Why literals rather than `AuditRecord::new`'s generated ids**, the same
/// argument as [`AUDIT_ORDER_ULIDS`] and `item_with_id`: `new` sets
/// `id: AuditId::new()`, the plain ULID generator, so six records minted
/// microseconds apart are ordered *randomly* relative to each other. "Newest
/// three" would then contain the eviction about half the time and the
/// limit-then-filter backend would pass on a coin flip.
///
/// A separate family from `AUDIT_ORDER_ULIDS` rather than a reuse: that array
/// has four entries with its eviction largest, which is the opposite of what
/// is needed here, and changing it to suit this test would silently destroy
/// `audit_filter_narrows_by_event_and_time`'s own disagreement between
/// `at`-order and id-order.
pub const AUDIT_TRUNCATION_ULIDS: [&str; 6] = [
    "01ARZ3NDEKTSV4RRFFQ69G5FD0",
    "01ARZ3NDEKTSV4RRFFQ69G5FD1",
    "01ARZ3NDEKTSV4RRFFQ69G5FD2",
    "01ARZ3NDEKTSV4RRFFQ69G5FD3",
    "01ARZ3NDEKTSV4RRFFQ69G5FD4",
    "01ARZ3NDEKTSV4RRFFQ69G5FD5",
];

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
            ITEM_ORDER_ULIDS,
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

    // The premise `list_tie_break_is_total_over_identical_timestamps` and
    // every other tied-corpus test rest on: every `item()` shares one
    // `created_at`, so any order those tests observe comes from the
    // tie-break, not from timestamp variation. The literal-ULID sort-order
    // premise just above has its own direct assertion
    // (`the_literal_ulids_the_ordering_tests_use_parse_and_sort_ascending`);
    // this is its sibling for the timestamp side, previously missing, which
    // is what made the asymmetry conspicuous in the first place.
    #[test]
    fn item_pins_created_at_to_the_unix_epoch() {
        let s = scope();
        assert_eq!(
            item(&s, "a note").created_at,
            OffsetDateTime::UNIX_EPOCH,
            "item()'s pinned created_at is the premise the tied-corpus tests \
             build on; a mutation that started sampling the clock would \
             untie that corpus silently"
        );
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
            // This test is about `is_valid()`'s shape check, not embedding
            // state.
            pending_embedding: false,
        });
        assert!(txn.is_valid());
    }

    // `purge_record` is what every purge conformance test hands
    // `Backend::purge_subject`, and two of its arguments are load-bearing
    // exactly because the defaults would look identical to a wrong backend's
    // behaviour: a fixture that dropped the `record.id = id` line would hand
    // the suite a generated id, against which an echoed id and a minted one
    // are the same observation; one that ignored `actor` and wrote
    // `Actor::system()` would match what a backend inventing its own
    // `SubjectPurged` row would most plausibly write. Neither mutation is
    // visible through `is_valid()` — which does not exist for a bare
    // `AuditRecord` at all — so assert the fields, with values that differ
    // from both defaults.
    //
    // Vacuous if the assertion ever compares against `AuditId::new()` or
    // `Actor::system()` instead of the distinct literals below: the fixture
    // would then be free to ignore both arguments.
    #[test]
    fn purge_record_uses_the_given_id_and_actor_not_defaults() {
        let s = scope();
        let id = AuditId::parse("01ARZ3NDEKTSV4RRFFQ69G5FC7").unwrap();
        let actor = Actor {
            kind: memorysafe_core::ActorKind::Human,
            id: Some("dpo-7".into()),
        };
        let rec = purge_record(&s, id.clone(), actor.clone());
        assert_eq!(rec.id, id, "purge_record minted its own id");
        assert_eq!(
            rec.actor, actor,
            "purge_record ignored the actor it was given"
        );
        assert_ne!(
            rec.actor,
            Actor::system(),
            "the fixture's actor must differ from the default a backend \
             inventing its own record would use"
        );
        assert_eq!(rec.event, AuditEvent::SubjectPurged);
        assert_eq!(rec.scope, s);
        assert!(
            rec.items.is_empty(),
            "a purge record names no surviving item"
        );
    }

    // The premise `audit_filter_narrows_by_event_and_time` rests on, and the
    // audit-side twin of
    // `the_literal_ulids_the_ordering_tests_use_parse_and_sort_ascending`:
    // these four literals are canonical ULIDs whose lexicographic order is the
    // ascending order that test writes them in. `AuditId` derives `Ord` over
    // the string, so this is exactly the comparison the assertion there makes
    // — and it is checkable today, unlike the conformance test itself, which
    // does not execute until a backend exists.
    //
    // Vacuous if `AUDIT_ORDER_ULIDS` is ever cut below two entries — a
    // one-element array sorts equal to itself and `ids[3] > ids[2]` would not
    // compile — or if its four literals are made equal to each other, since a
    // constant array sorts to itself either way and no ordering is then under
    // test. The array's own declaration is what prevents both: `[&str; 4]`
    // fixes the length at four, and the four literals differ in their final
    // character (`FB0`/`FB1`/`FB2`/`FB3`), which is also what makes the
    // strict `ids[3] > ids[2]` below fail rather than pass on equal values.
    #[test]
    fn the_literal_audit_ulids_the_ordering_test_uses_parse_and_sort_ascending() {
        let ids: Vec<AuditId> = AUDIT_ORDER_ULIDS
            .iter()
            .map(|s| AuditId::parse(s).expect("literal must be a canonical ULID"))
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            sorted, ids,
            "AUDIT_ORDER_ULIDS is written in the order the ordering test \
             assigns it — three admits, then the eviction last and largest; \
             if it does not sort that way, that test asserts the wrong sequence"
        );
        // And the eviction's id must be strictly the largest, since the whole
        // point is that it is newest by id while being earlier by `at`.
        assert!(
            ids[3] > ids[2],
            "the eviction's id must exceed the third admit's, or `at`-ordering \
             and id-ordering do not disagree and the test cannot tell them apart"
        );
    }

    // `AUDIT_TRUNCATION_ULIDS`' premise, and the reason it is a separate
    // family from `AUDIT_ORDER_ULIDS`: the six literals ascend, and the
    // eviction's id — index 1 — must be strictly below the newest three
    // (indices 3, 4, 5). That is the whole discriminating power of
    // `audit_returns_min_of_the_limit_and_the_rows_that_remain`'s filtered
    // half: a backend that limits before it filters sees only indices 3..5
    // and finds no eviction there.
    //
    // Vacuous if the array is cut below four entries (there would be no
    // "newest three" to exclude index 1 from) or if the literals are made
    // equal (every index would then be in every window). `[&str; 6]` fixes
    // the length, and the assertion below compares against a strictly sorted
    // copy, which equal literals would satisfy — so the strict `<` on the
    // last line is what actually rules equality out.
    #[test]
    fn the_literal_truncation_ulids_ascend_with_the_eviction_below_the_newest_three() {
        let ids: Vec<AuditId> = AUDIT_TRUNCATION_ULIDS
            .iter()
            .map(|s| AuditId::parse(s).expect("literal must be a canonical ULID"))
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            sorted, ids,
            "AUDIT_TRUNCATION_ULIDS is written in the order its test assigns \
             it — admit, eviction, then four admits, ascending; if it does not \
             sort that way the test asserts the wrong rows"
        );
        assert!(
            ids[1] < ids[3],
            "the eviction's id must fall below the newest three, or a backend \
             that applies `limit` before `events` cannot be told from one that \
             filters first, and the test certifies both"
        );
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
