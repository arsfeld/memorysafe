//! Comparing two traces of one scenario.

use crate::ShadowError;
use crate::trace::{Trace, TracedDecision};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionChange {
    pub seq: usize,
    pub before: TracedDecision,
    pub after: TracedDecision,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceDiff {
    pub total: usize,
    pub identical: usize,
    pub changed: Vec<DecisionChange>,
    /// `"before->after"` action labels, counted. `BTreeMap` so the report is
    /// stable enough to diff two shadow runs against each other.
    pub transitions: BTreeMap<String, usize>,
}

impl TraceDiff {
    pub fn is_clean(&self) -> bool {
        self.changed.is_empty()
    }
}

/// Compares two traces position by position, refusing to align traces that are
/// not two runs of the same scenario.
pub fn diff(before: &Trace, after: &Trace) -> Result<TraceDiff, ShadowError> {
    if before.decisions.len() != after.decisions.len() {
        return Err(ShadowError::Misaligned {
            before: before.decisions.len(),
            after: after.decisions.len(),
        });
    }

    let mut changed = Vec::new();
    let mut transitions: BTreeMap<String, usize> = BTreeMap::new();
    let mut identical = 0;

    for (b, a) in before.decisions.iter().zip(after.decisions.iter()) {
        if b.seq != a.seq {
            return Err(ShadowError::Malformed(format!(
                "decision {} in the first trace lines up with decision {} in the second; these \
                 are not two runs of one scenario",
                b.seq, a.seq
            )));
        }
        if b == a {
            identical += 1;
            continue;
        }
        // Reasons count. A verdict that stays the same but is justified
        // differently is still a change to the audit trail a customer reads.
        *transitions
            .entry(format!("{}->{}", b.action.label(), a.action.label()))
            .or_default() += 1;
        changed.push(DecisionChange {
            seq: b.seq,
            before: b.clone(),
            after: a.clone(),
        });
    }

    Ok(TraceDiff {
        total: before.decisions.len(),
        identical,
        changed,
        transitions,
    })
}
