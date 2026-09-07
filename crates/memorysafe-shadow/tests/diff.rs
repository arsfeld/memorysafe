use memorysafe_core::{ReasonCode, Scope};
use memorysafe_shadow::{Trace, TracedAction, TracedDecision, diff};

fn decision(seq: usize, action: TracedAction) -> TracedDecision {
    TracedDecision {
        seq,
        scope: Scope::new("acme", "user-42", "agent").unwrap(),
        body_digest: format!("{seq:064}"),
        action,
        reason_codes: vec![ReasonCode::NovelContent],
        evicted: 0,
        sensitivity: None,
    }
}

fn trace(decisions: Vec<TracedDecision>) -> Trace {
    Trace {
        scenario: "t".into(),
        policy: memorysafe_core::PolicyId::new("baseline", "1"),
        decisions,
    }
}

#[test]
fn two_identical_traces_diff_to_nothing() {
    let a = trace(vec![decision(
        0,
        TracedAction::Retain {
            protection: memorysafe_shadow::TracedProtection::Normal,
        },
    )]);
    let d = diff(&a, &a).unwrap();
    assert_eq!(d.total, 1);
    assert_eq!(d.identical, 1);
    assert!(d.changed.is_empty());
    assert!(
        d.transitions.is_empty(),
        "no transition means no histogram entry"
    );
}

#[test]
fn a_changed_action_is_reported_with_both_sides_and_counted() {
    let before = trace(vec![
        decision(
            0,
            TracedAction::Retain {
                protection: memorysafe_shadow::TracedProtection::Normal,
            },
        ),
        decision(
            1,
            TracedAction::Retain {
                protection: memorysafe_shadow::TracedProtection::Normal,
            },
        ),
    ]);
    let after = trace(vec![
        decision(
            0,
            TracedAction::Retain {
                protection: memorysafe_shadow::TracedProtection::Normal,
            },
        ),
        decision(1, TracedAction::Reject),
    ]);

    let d = diff(&before, &after).unwrap();
    assert_eq!(d.total, 2);
    assert_eq!(d.identical, 1);
    assert_eq!(d.changed.len(), 1);
    assert_eq!(d.changed[0].seq, 1);
    assert!(matches!(
        d.changed[0].before.action,
        TracedAction::Retain { .. }
    ));
    assert!(matches!(d.changed[0].after.action, TracedAction::Reject));
    assert_eq!(d.transitions.get("retain->reject"), Some(&1));
}

#[test]
fn a_change_in_reasons_alone_still_counts_as_a_change() {
    // Same verdict, different justification. An audit trail that suddenly
    // explains itself differently is a policy change, and a diff that hid it
    // would let one land unnoticed.
    let before = trace(vec![decision(0, TracedAction::Reject)]);
    let mut changed = decision(0, TracedAction::Reject);
    changed.reason_codes = vec![ReasonCode::LowValue];
    let after = trace(vec![changed]);

    let d = diff(&before, &after).unwrap();
    assert_eq!(d.changed.len(), 1);
    assert_eq!(d.transitions.get("reject->reject"), Some(&1));
}

#[test]
fn traces_of_different_lengths_are_an_error_not_a_partial_diff() {
    let before = trace(vec![decision(0, TracedAction::Reject)]);
    let after = trace(vec![]);
    assert!(diff(&before, &after).is_err());
}

#[test]
fn misaligned_sequence_numbers_are_an_error() {
    // Aligning by position when the sequence numbers disagree would compare
    // unrelated writes and report nonsense.
    let before = trace(vec![decision(0, TracedAction::Reject)]);
    let after = trace(vec![decision(7, TracedAction::Reject)]);
    assert!(diff(&before, &after).is_err());
}
