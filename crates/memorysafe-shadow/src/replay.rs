//! Building a `Scenario` from a real audit archive, so a shadow run can
//! replay production traffic instead of a hand-written fixture.
//!
//! An audit row names the items it concerns by id and digest, never by body —
//! that is a load-bearing privacy property (see `memorysafe_core::ItemRef`).
//! So a decision can be replayed only when the item it produced is still in
//! the archive: an `Admitted` row's item is exported alongside it and
//! reconstructs the write; a `Merged` row's new content was folded into its
//! target and no longer exists separately; a `Rejected` row stored nothing.
//! Every row that cannot be replayed is counted in `Scenario::unreplayable`
//! rather than silently dropped, and `Scenario::coverage` reports the
//! fraction that could be.

use crate::ShadowError;
use crate::scenario::{Scenario, ScenarioWrite, Unreplayable, default_dim};
use crate::trace::{Trace, TracedAction, TracedDecision, TracedProtection};
use memorysafe_backend::ExportRecord;
use memorysafe_core::{Action, AuditEvent, AuditRecord, ItemId, MemoryItem, PolicyId, Protection};
use std::collections::HashMap;
use time::OffsetDateTime;

/// The result of turning an export archive back into a `Scenario`.
pub struct Replay {
    pub scenario: Scenario,
    /// The decisions as they were originally recorded, aligned to the
    /// scenario's writes. `None` when the archive carried no audit records —
    /// there is then nothing recorded to diff a replay against.
    pub recorded: Option<Trace>,
}

/// `Protection::Protected { until }` is an absolute wall-clock deadline
/// (`created_at + protection_window_days`); a trace instead carries the
/// window length, which is invariant under the wall clock. See
/// `TracedProtection`'s own doc for why the absolute value cannot be used
/// here — the same reasoning `run.rs` applies to a freshly written item
/// applies unchanged to one read back out of an archive.
fn traced_protection(protection: Protection, created_at: OffsetDateTime) -> TracedProtection {
    match protection {
        Protection::Normal => TracedProtection::Normal,
        Protection::Pinned => TracedProtection::Pinned,
        Protection::Protected { until } => TracedProtection::Protected {
            window_seconds: (until - created_at).whole_seconds(),
        },
    }
}

/// Turns a real export stream into a `Scenario`, and — when the archive
/// carries audit records — the `Trace` those records themselves describe.
pub fn from_export_ndjson(name: &str, ndjson: &str) -> Result<Replay, ShadowError> {
    let mut items: HashMap<ItemId, MemoryItem> = HashMap::new();
    let mut audit: Vec<AuditRecord> = Vec::new();

    for (number, line) in ndjson.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: ExportRecord = serde_json::from_str(line)
            .map_err(|e| ShadowError::Malformed(format!("line {}: {e}", number + 1)))?;
        match record {
            ExportRecord::Header { .. } => {}
            ExportRecord::Item { item, .. } => {
                items.insert(item.id.clone(), *item);
            }
            ExportRecord::Audit { audit: record } => audit.push(*record),
        }
    }

    if audit.is_empty() {
        // No recorded decisions. ULIDs sort by creation time, so the corpus
        // alone still gives a faithful write order.
        let mut ordered: Vec<&MemoryItem> = items.values().collect();
        ordered.sort_by(|a, b| a.id.cmp(&b.id));
        return Ok(Replay {
            scenario: Scenario {
                name: name.to_owned(),
                embedder_dim: default_dim(),
                budgets: vec![],
                writes: ordered.into_iter().map(write_from_item).collect(),
                unreplayable: vec![],
            },
            recorded: None,
        });
    }

    // `AuditId` is a millisecond ULID and therefore a total order; `at` is
    // whole seconds and cannot separate rows written in the same second.
    audit.sort_by(|a, b| a.id.cmp(&b.id));

    let mut writes = Vec::new();
    let mut recorded = Vec::new();
    let mut unreplayable = Vec::new();
    let mut policy: Option<PolicyId> = None;

    for record in &audit {
        let skip = |why: &str| Unreplayable {
            audit_id: record.id.to_string(),
            // `record.event.as_str()`, not `format!("{:?}", ...).to_lowercase()`:
            // `AuditEvent` already carries one canonical snake_case name
            // (`as_str`, documented as the one home for this string), and the
            // two disagree on several variants — `SubjectPurged` debug-lowers
            // to "subjectpurged" but `as_str()` gives "subject_purged", and
            // likewise for `PolicyChanged`/`MaintenanceRun`.
            event: record.event.as_str().to_owned(),
            why: why.to_owned(),
        };

        match record.event {
            AuditEvent::Admitted => {}
            AuditEvent::Merged => {
                unreplayable.push(skip(
                    "the merged content was folded into its target and no longer exists separately",
                ));
                continue;
            }
            AuditEvent::Rejected => {
                unreplayable.push(skip(
                    "nothing was stored, so the rejected body was never written to the archive",
                ));
                continue;
            }
            _ => {
                unreplayable.push(skip("not a write"));
                continue;
            }
        }

        let Some(id) = record.items.first().map(|r| r.id()) else {
            unreplayable.push(skip("the admission record names no item"));
            continue;
        };
        let Some(item) = items.get(id) else {
            unreplayable.push(skip("the admitted item is not in this archive"));
            continue;
        };

        let seq = writes.len();
        writes.push(write_from_item(item));

        let decision = record.decision.as_ref();
        if let Some(d) = decision {
            policy.get_or_insert_with(|| d.policy.clone());
        }
        recorded.push(TracedDecision {
            seq,
            scope: item.scope.clone(),
            body_digest: blake3::hash(item.body.as_bytes()).to_hex().to_string(),
            action: match decision.map(|d| &d.action) {
                Some(Action::Retain { protection }) => TracedAction::Retain {
                    protection: traced_protection(*protection, item.created_at),
                },
                Some(Action::Merge { .. }) => TracedAction::Merge { into_seq: None },
                Some(Action::Reject) => TracedAction::Reject,
                // An admission with no recorded decision (`protect`'s audit
                // row carries none): the event still says what happened.
                None => TracedAction::Retain {
                    protection: traced_protection(item.protection, item.created_at),
                },
            },
            reason_codes: decision
                .map(|d| d.reasons.iter().map(|r| r.code).collect())
                .unwrap_or_default(),
            evicted: decision.map(|d| d.evictions.len()).unwrap_or(0),
            sensitivity: Some(item.sensitivity),
        });
    }

    let scenario = Scenario {
        name: name.to_owned(),
        embedder_dim: default_dim(),
        budgets: vec![],
        writes,
        unreplayable,
    };
    let recorded = Trace {
        scenario: scenario.name.clone(),
        policy: policy.unwrap_or_else(|| PolicyId::new("unrecorded", "0")),
        decisions: recorded,
    };

    Ok(Replay {
        scenario,
        recorded: Some(recorded),
    })
}

fn write_from_item(item: &MemoryItem) -> ScenarioWrite {
    ScenarioWrite {
        scope: item.scope.clone(),
        body: item.body.clone(),
        kind: item.kind.clone(),
        tags: item.tags.clone(),
        // The original hint is not recorded — only the resolved level, which
        // a policy must be free to reach on its own. Replaying with the
        // resolved level as a hint would guarantee agreement and prove
        // nothing.
        sensitivity_hint: None,
        ttl_seconds: item.ttl.map(|d| d.whole_seconds()),
    }
}
