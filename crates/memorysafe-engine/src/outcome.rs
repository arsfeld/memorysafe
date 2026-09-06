use memorysafe_core::{Action, AuditId, ItemId, Reason};
use serde::{Deserialize, Serialize};

/// What the caller learns from a write. A rejection or a merge is a *success*:
/// governance working, not an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteOutcome {
    pub item_id: Option<ItemId>,
    pub action: Action,
    pub reasons: Vec<Reason>,
    pub merged_into: Option<ItemId>,
    pub evicted: Vec<ItemId>,
    pub audit_id: AuditId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgetOutcome {
    pub forgotten: Vec<ItemId>,
    pub audit_id: AuditId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PurgeOutcome {
    pub items_removed: u64,
    pub audit_rows_removed: u64,
    pub audit_rows_preserved: u64,
}
