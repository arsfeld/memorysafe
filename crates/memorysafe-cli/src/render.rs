use anyhow::Result;
use memorysafe_core::{Action, MemoryItem, WorkingSet};
use memorysafe_engine::WriteOutcome;
use serde::Serialize;

/// `--json` prints the engine type's own serde form. Anything else risks a
/// second, drifting definition of the wire format.
pub fn emit<T: Serialize>(json: bool, value: &T, human: impl FnOnce()) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        human();
    }
    Ok(())
}

pub fn write_outcome(out: &WriteOutcome) {
    let verb = match &out.action {
        Action::Retain { .. } => "retained",
        Action::Merge { into, .. } => {
            println!("merged into {into}");
            "merged"
        }
        Action::Reject => "rejected",
    };
    if let Some(id) = &out.item_id {
        println!("{verb} {id}");
    } else {
        println!("{verb}");
    }
    for reason in &out.reasons {
        println!("  because {:?}: {}", reason.code, reason.detail);
    }
    for evicted in &out.evicted {
        println!("  evicted {evicted}");
    }
    println!("  audit {}", out.audit_id);
}

pub fn working_set(ws: &WorkingSet) {
    if ws.items.is_empty() {
        println!("nothing recalled");
    }
    for selected in &ws.items {
        println!("{}  {}", selected.item.id, selected.item.body);
        println!("  {:?}: {}", selected.reason.code, selected.reason.detail);
    }
    println!(
        "{} tokens used, {} omitted",
        ws.tokens_used,
        ws.omitted.len()
    );
    if let Some(id) = &ws.audit_id {
        println!("audit {id}");
    }
}

pub fn items(items: &[MemoryItem]) {
    if items.is_empty() {
        println!("nothing stored in this scope");
    }
    for item in items {
        println!(
            "{}  [{}] {:?} {}",
            item.id, item.kind, item.sensitivity, item.body
        );
    }
}
