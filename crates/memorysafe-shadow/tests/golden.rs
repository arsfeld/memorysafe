use memorysafe_core::{ReasonCode, SensitivityLevel};
use memorysafe_policy::BaselinePolicy;
use memorysafe_shadow::{Scenario, Trace, TracedAction, run};
use std::path::PathBuf;
use std::sync::Arc;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Run a fixture and compare against its blessed trace. Set `MEMORYSAFE_BLESS=1`
/// to rewrite the blessed file after an intentional policy change — and then
/// read the diff before committing it.
async fn golden(name: &str) -> Trace {
    let scenario = Scenario::load(&fixtures().join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("loading fixture {name}: {e}"));
    let actual = run(&scenario, Arc::new(BaselinePolicy::default()))
        .await
        .unwrap_or_else(|e| panic!("running fixture {name}: {e}"));

    let blessed_path = fixtures().join(format!("{name}.trace.json"));
    if std::env::var_os("MEMORYSAFE_BLESS").is_some() {
        std::fs::write(
            &blessed_path,
            serde_json::to_string_pretty(&actual).unwrap(),
        )
        .unwrap();
        return actual;
    }

    let blessed: Trace =
        serde_json::from_str(&std::fs::read_to_string(&blessed_path).unwrap_or_else(|e| {
            panic!(
                "no blessed trace at {}: {e} — run with MEMORYSAFE_BLESS=1",
                blessed_path.display()
            )
        }))
        .unwrap();

    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&blessed).unwrap(),
        "the baseline policy changed its decisions for fixture '{name}'. If that was intended, \
         rerun with MEMORYSAFE_BLESS=1 and review the diff before committing."
    );
    actual
}

#[tokio::test]
async fn novelty_admits_every_distinct_memory() {
    let trace = golden("novelty").await;
    assert_eq!(trace.decisions.len(), 3);
    for decision in &trace.decisions {
        assert!(
            matches!(decision.action, TracedAction::Retain { .. }),
            "a distinct memory was not retained: {decision:?}"
        );
    }
}

#[tokio::test]
async fn redundancy_rejects_the_exact_duplicate() {
    let trace = golden("redundancy").await;
    assert!(matches!(
        trace.decisions[0].action,
        TracedAction::Retain { .. }
    ));
    assert!(
        matches!(trace.decisions[1].action, TracedAction::Reject),
        "an identical rewrite was admitted: {:?}",
        trace.decisions[1]
    );
    assert!(
        trace.decisions[1]
            .reason_codes
            .contains(&ReasonCode::NearDuplicate)
    );
    // The third write is a near-duplicate. Whether it merges or is retained
    // depends on where the deterministic embedder places it, so the blessed
    // trace pins that; the only property asserted here is that it was explained.
    assert!(!trace.decisions[2].reason_codes.is_empty());
}

#[tokio::test]
async fn capacity_evicts_to_stay_within_the_budget() {
    let trace = golden("capacity").await;
    let evicted: usize = trace.decisions.iter().map(|d| d.evicted).sum();
    assert!(
        evicted > 0,
        "a budget of two over five writes evicted nothing"
    );
}

#[tokio::test]
async fn a_credential_is_stored_restricted_whatever_the_caller_hinted() {
    let trace = golden("sensitivity").await;
    let stored = trace
        .decisions
        .iter()
        .find(|d| matches!(d.action, TracedAction::Retain { .. }))
        .expect("the credential was stored");
    assert_eq!(
        stored.sensitivity,
        Some(SensitivityLevel::Restricted),
        "a caller's low hint lowered the stored level"
    );
}
