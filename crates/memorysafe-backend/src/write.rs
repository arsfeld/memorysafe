use memorysafe_core::{AuditRecord, ItemId, MemoryItem, Scope};
use memorysafe_embed::QuantizedVector;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An item plus its already-computed vector. Backends never embed.
///
/// `item` is not `Option`: `QuantizedVector` carries no `ItemId`, so
/// `ItemWrite { item: None, vector: Some(v) }` would name no row to attach
/// the vector to — an unaddressable state with no legitimate construction.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemWrite {
    pub item: MemoryItem,
    pub vector: Option<QuantizedVector>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeWrite {
    pub target: ItemId,
    pub body: String,
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, serde_json::Value>,
    pub vector: Option<QuantizedVector>,
    pub byte_size: u64,
}

/// One atomic unit of change. Item write, evictions, and the audit record
/// commit together or not at all — the backend has no API for doing them
/// separately.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteTransaction {
    pub scope: Scope,
    pub upsert: Option<ItemWrite>,
    pub merge: Option<MergeWrite>,
    pub evictions: Vec<ItemId>,
    pub audit: AuditRecord,
    pub idempotency_key: Option<String>,
    pub payload_digest: Option<String>,
}

impl WriteTransaction {
    pub fn new(scope: Scope, audit: AuditRecord) -> Self {
        Self {
            scope,
            upsert: None,
            merge: None,
            evictions: vec![],
            audit,
            idempotency_key: None,
            payload_digest: None,
        }
    }

    /// A transaction is invalid if it both inserts and merges, or if its
    /// scope-bearing fields disagree about which scope this write belongs to.
    ///
    /// `scope`, the upserted item's own `scope`, and `audit.scope` are three
    /// independently-settable public fields, and a real backend reads
    /// different ones for different rows: the item row is keyed on the
    /// item's own scope, the vector row and the tenant file are keyed on
    /// `scope`, and the audit row is keyed on `audit.scope`. If they
    /// disagreed, the row, its vector, and its own audit trail would each be
    /// filed under a different subject or namespace — the compliance surface
    /// this product sells would point somewhere else. `MergeWrite` carries
    /// no scope of its own, so there is nothing to check there; a merge is
    /// always addressed by `scope`.
    pub fn is_valid(&self) -> bool {
        if self.upsert.is_some() && self.merge.is_some() {
            return false;
        }
        if let Some(w) = &self.upsert
            && w.item.scope != self.scope
        {
            return false;
        }
        self.audit.scope == self.scope
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedWrite {
    pub item_id: Option<ItemId>,
    pub audit_id: memorysafe_core::AuditId,
    pub evicted: Vec<ItemId>,
    /// True when an idempotency key matched and the stored outcome was
    /// returned instead of applying anything.
    pub replayed: bool,
    pub replayed_outcome: Option<String>,
}

/// `audit_rows_removed + audit_rows_preserved` accounts for every audit row
/// that existed for the subject *before* the purge ran — not for any row
/// the purge itself may add. `AuditEvent::SubjectPurged` exists, so a
/// conformant backend may write its own audit row recording the purge; that
/// row did not exist to be removed or preserved and is not counted in
/// either field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeReport {
    pub items_removed: u64,
    pub vectors_removed: u64,
    pub audit_rows_removed: u64,
    pub audit_rows_preserved: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::{
        Actor, AuditEvent, AuditRecord, Protection, Scope, SensitivityLevel, Source, SourceKind,
    };
    use time::OffsetDateTime;

    fn scope() -> Scope {
        Scope::new("t", "s", "n").unwrap()
    }

    fn audit() -> AuditRecord {
        audit_for(scope())
    }

    fn audit_for(scope: Scope) -> AuditRecord {
        AuditRecord::new(
            scope,
            AuditEvent::Admitted,
            vec![],
            Actor::system(),
            OffsetDateTime::UNIX_EPOCH,
        )
    }

    fn item(scope: Scope) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope,
            body: "an item".into(),
            kind: "fact".into(),
            source: Source {
                kind: SourceKind::Agent,
                id: None,
            },
            occurred_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            tags: vec![],
            attrs: Default::default(),
            sensitivity: SensitivityLevel::Internal,
            ttl: None,
            protection: Protection::Normal,
            pending_embedding: false,
        }
    }

    #[test]
    fn a_write_transaction_always_carries_exactly_one_audit_record() {
        let txn = WriteTransaction::new(scope(), audit());
        assert!(txn.upsert.is_none());
        assert!(txn.merge.is_none());
        assert!(txn.evictions.is_empty());
        assert_eq!(txn.audit.event, AuditEvent::Admitted);
    }

    #[test]
    fn upsert_and_merge_are_mutually_exclusive() {
        let mut txn = WriteTransaction::new(scope(), audit());
        txn.merge = Some(MergeWrite {
            target: memorysafe_core::ItemId::new(),
            body: "merged".into(),
            tags: vec![],
            attrs: Default::default(),
            vector: None,
            byte_size: 6,
        });
        assert!(txn.is_valid());
        txn.upsert = Some(ItemWrite {
            item: item(scope()),
            vector: None,
        });
        assert!(
            !txn.is_valid(),
            "a transaction may not both insert and merge"
        );
    }

    #[test]
    fn a_transaction_is_invalid_when_the_upserted_items_scope_disagrees() {
        let mut txn = WriteTransaction::new(scope(), audit());
        txn.upsert = Some(ItemWrite {
            item: item(scope()),
            vector: None,
        });
        assert!(txn.is_valid(), "a well-formed upsert must be valid");

        // Same tenant, different subject: the item would be written under one
        // subject while the transaction — and its vector row — are filed
        // under another.
        txn.upsert = Some(ItemWrite {
            item: item(Scope::new("t", "other-subject", "n").unwrap()),
            vector: None,
        });
        assert!(
            !txn.is_valid(),
            "an upserted item whose scope disagrees with the transaction's scope must be rejected"
        );
    }

    #[test]
    fn a_transaction_is_invalid_when_the_audit_records_scope_disagrees() {
        let valid = WriteTransaction::new(scope(), audit());
        assert!(valid.is_valid(), "a well-formed transaction must be valid");

        // Same tenant, different namespace: the audit trail would point at a
        // namespace this write never touched.
        let mismatched = WriteTransaction::new(
            scope(),
            audit_for(Scope::new("t", "s", "other-ns").unwrap()),
        );
        assert!(
            !mismatched.is_valid(),
            "an audit record whose scope disagrees with the transaction's scope must be rejected"
        );
    }
}
