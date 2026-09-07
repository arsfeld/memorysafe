use memorysafe_core::{Budget, ReasonCode, Scope};
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use memorysafe_shadow::{Scenario, ScenarioWrite, TracedAction, run};
use std::sync::Arc;

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn write(body: &str) -> ScenarioWrite {
    ScenarioWrite {
        scope: scope(),
        body: body.to_owned(),
        kind: "fact".into(),
        tags: vec![],
        sensitivity_hint: None,
        ttl_seconds: None,
    }
}

fn scenario(name: &str, writes: Vec<ScenarioWrite>) -> Scenario {
    Scenario {
        name: name.into(),
        embedder_dim: 256,
        budgets: vec![],
        writes,
        unreplayable: vec![],
    }
}

#[tokio::test]
async fn a_scenario_of_distinct_writes_traces_one_decision_each() {
    let s = scenario(
        "novelty",
        vec![
            write("the production migration runs on Sundays"),
            write("the on-call rotation starts Monday morning"),
            write("the staging cluster is rebuilt every night"),
        ],
    );

    let trace = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();

    assert_eq!(trace.scenario, "novelty");
    assert_eq!(trace.decisions.len(), 3);
    for (i, decision) in trace.decisions.iter().enumerate() {
        assert_eq!(decision.seq, i);
        assert!(
            matches!(decision.action, TracedAction::Retain { .. }),
            "decision {i} was {:?}",
            decision.action
        );
        assert!(!decision.reason_codes.is_empty());
        assert_eq!(decision.body_digest.len(), 64, "a blake3 hex digest");
    }
}

#[tokio::test]
async fn a_trace_carries_no_bodies() {
    let s = scenario(
        "secrecy",
        vec![write("a body that must never reach a diff artifact")],
    );
    let trace = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    let json = serde_json::to_string(&trace).unwrap();
    assert!(
        !json.contains("must never reach"),
        "the trace carried a body"
    );
}

#[tokio::test]
async fn the_same_scenario_and_policy_produce_identical_traces() {
    // Without this the harness cannot tell a policy change from run-to-run
    // noise, and every diff is meaningless.
    let s = scenario(
        "stability",
        vec![
            write("the production migration runs on Sundays"),
            write("the on-call rotation starts Monday morning"),
        ],
    );

    let first = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    let second = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap(),
        "two runs of one scenario disagreed"
    );
}

#[tokio::test]
async fn an_exact_duplicate_is_traced_as_a_rejection_with_its_reason() {
    let body = "the deploy key rotates every ninety days";
    let s = scenario("redundancy", vec![write(body), write(body)]);

    let trace = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    assert!(matches!(
        trace.decisions[0].action,
        TracedAction::Retain { .. }
    ));
    assert!(
        matches!(trace.decisions[1].action, TracedAction::Reject),
        "an identical rewrite was not rejected: {:?}",
        trace.decisions[1]
    );
    // `ReasonCode::ExactDuplicate` was renamed to `NearDuplicate` before this
    // task (see `memorysafe-policy::config` and `::compose`'s own notes on
    // the rename); the plan text here still names the pre-rename variant.
    assert!(
        trace.decisions[1]
            .reason_codes
            .contains(&ReasonCode::NearDuplicate)
    );
    assert_eq!(
        trace.decisions[0].body_digest, trace.decisions[1].body_digest,
        "identical bodies must digest identically"
    );
}

#[tokio::test]
async fn a_merge_records_the_seq_that_created_its_target_not_its_own_seq() {
    // Regression: `outcome.item_id` on a `Merge` names the EXISTING target,
    // not a freshly created item (`memorysafe-backend-sqlite`'s `apply` sets
    // it to the merge target's own id). Recording every `outcome.item_id`
    // into `id_to_seq` unconditionally overwrote the target's real creating
    // seq with the merging write's own seq, so `into_seq` reported each
    // merge as having merged into itself.
    let cfg = BaselineConfig {
        // Wide open: any shared token beats `merge_threshold`, and nothing
        // reaches `duplicate_threshold` short of a literal byte-identical
        // resend, so every write after the first merges into the one item
        // this scenario ever creates.
        merge_threshold: 0.0,
        duplicate_threshold: 1.01,
        ..Default::default()
    };
    let s = scenario(
        "merge-seq",
        vec![
            write("alpha beta gamma delta"),
            write("alpha beta gamma epsilon"),
            write("alpha beta zeta eta"),
        ],
    );

    let trace = run(&s, Arc::new(BaselinePolicy::new(cfg))).await.unwrap();

    assert!(
        matches!(trace.decisions[0].action, TracedAction::Retain { .. }),
        "seq 0 (first write, no neighbours) was not a retain: {:?}",
        trace.decisions[0]
    );
    for (i, decision) in trace.decisions.iter().enumerate().skip(1) {
        match &decision.action {
            TracedAction::Merge { into_seq } => {
                assert_eq!(
                    *into_seq,
                    Some(0),
                    "seq {i} merged into seq {into_seq:?}, not the write that created its \
                     target (seq 0)"
                );
            }
            other => panic!("seq {i} was not a merge under a wide-open config: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_budget_forces_evictions_that_the_trace_counts() {
    let mut s = scenario(
        "capacity",
        (0..4)
            .map(|i| write(&format!("distinct memory {i} concerning topic {i}")))
            .collect(),
    );
    s.budgets = vec![(
        scope(),
        Budget {
            max_items: Some(2),
            max_bytes: None,
        },
    )];

    let trace = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    let evicted: usize = trace.decisions.iter().map(|d| d.evicted).sum();
    assert!(
        evicted > 0,
        "a budget of 2 over 4 writes evicted nothing: {trace:?}"
    );
}

#[tokio::test]
async fn a_different_policy_configuration_produces_a_different_trace() {
    // The whole point of the harness. If a threshold change cannot move a
    // trace, the harness cannot detect a policy regression either.
    let s = scenario(
        "sensitivity-to-config",
        vec![
            write("the production migration runs on Sundays"),
            write("an entirely unrelated topic about kitchens"),
        ],
    );

    let permissive = run(&s, Arc::new(BaselinePolicy::default())).await.unwrap();
    let paranoid = run(
        &s,
        Arc::new(BaselinePolicy::new(BaselineConfig {
            duplicate_threshold: 0.0,
            merge_threshold: 0.0,
            ..Default::default()
        })),
    )
    .await
    .unwrap();

    assert_ne!(
        serde_json::to_value(&permissive).unwrap(),
        serde_json::to_value(&paranoid).unwrap(),
        "a policy that rejects everything traced the same as one that accepts"
    );
}

#[tokio::test]
async fn a_scenario_round_trips_through_json() {
    let s = scenario("round-trip", vec![write("something to serialise")]);
    let text = serde_json::to_string_pretty(&s).unwrap();
    let parsed: Scenario = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, s);
}
