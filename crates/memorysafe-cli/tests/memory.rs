use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::Path;

fn msafe(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("msafe").expect("the msafe binary builds");
    cmd.current_dir(dir);
    cmd.env("MSAFE_TENANT", "acme");
    cmd.env("MSAFE_SUBJECT", "user-42");
    cmd.env("MSAFE_NAMESPACE", "agent");
    cmd
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("msafe.toml"),
        "data_dir = \"tenants\"\nembedder = \"deterministic\"\nembedding_dim = 256\n",
    )
    .expect("write config");
    dir
}

fn json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn remembering_prints_the_decision_and_exits_zero() {
    let dir = workspace();
    let output = msafe(dir.path())
        .args([
            "--json",
            "remember",
            "the production migration runs on Sundays",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = json(&output);
    assert_eq!(value["action"]["kind"], "retain");
    assert!(value["item_id"].is_string());
    assert!(value["audit_id"].is_string());
}

#[test]
fn a_rejected_write_still_exits_zero() {
    // A shell script looping over candidate memories must not stop because
    // governance did its job.
    let dir = workspace();
    let body = "the deploy key rotates every ninety days";
    msafe(dir.path())
        .args(["remember", body])
        .assert()
        .success();

    let output = msafe(dir.path())
        .args(["--json", "remember", body])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "a governance decision set a failure exit code"
    );
    let action = json(&output)["action"]["kind"].as_str().unwrap().to_owned();
    assert!(action == "reject" || action == "merge", "{action}");
}

#[test]
fn human_output_says_what_happened_and_why() {
    let dir = workspace();
    msafe(dir.path())
        .args(["remember", "the on-call rotation starts Monday morning"])
        .assert()
        .success()
        .stdout(contains("retained").or(contains("Retained")))
        .stdout(contains("because").or(contains("novel")));
}

#[test]
fn recalling_returns_a_working_set_and_reports_the_budget_it_used() {
    let dir = workspace();
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        msafe(dir.path())
            .args(["remember", body])
            .assert()
            .success();
    }

    let output = msafe(dir.path())
        .args([
            "--json",
            "recall",
            "when does the migration run",
            "--max-items",
            "2",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value = json(&output);
    assert!(value["audit_id"].is_string(), "every recall is audited");
    let items = value["items"].as_array().unwrap();
    assert!(
        !items.is_empty(),
        "a matching query returned nothing: {value}"
    );
    assert!(items.len() <= 2);
}

// **Corrected from the task brief.** The brief's own version of this test
// ("recall_with_no_query_is_allowed_and_still_governed") asserted that
// `msafe recall` with no query string succeeds. It does not, and should
// not: `memorysafe_engine::Engine::recall` deliberately rejects a queryless,
// filterless recall — "a recall needs a query; filter-only recall is not
// supported in v1" — a rule pinned by its own dedicated test
// (`memorysafe-engine/tests/read_query_validation.rs::
// a_recall_with_no_query_is_a_validation_error`) and already inherited
// as-is by both finished adapters: `memorysafe-api::memories::recall` and
// `memorysafe-mcp`'s recall tool both accept an optional `query` at their
// own wire level and let the very same engine rejection surface through
// unchanged (`memorysafe-api` maps it to a 400; nothing in either crate
// pre-validates the field to avoid it). The CLI keeps the same shape
// (`RecallArgs::query: Option<String>`) for the same reason — a caller may
// spell `msafe recall` with no argument — but a missing query is a
// caller-side usage problem, not a governance decision, so it is a real
// failure (non-zero exit), not a silently-empty success.
#[test]
fn recall_with_no_query_and_no_filters_is_a_clear_usage_error() {
    let dir = workspace();
    msafe(dir.path())
        .args(["remember", "a memory with no particular topic"])
        .assert()
        .success();

    let output = msafe(dir.path())
        .args(["--json", "recall"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a queryless, filterless recall should surface the engine's own rejection, not succeed"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("query"),
        "the error must explain what is missing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn review_lists_what_is_stored() {
    let dir = workspace();
    msafe(dir.path())
        .args(["remember", "alpha memory about deployments"])
        .assert()
        .success();
    msafe(dir.path())
        .args(["remember", "beta memory about the kitchen"])
        .assert()
        .success();

    let output = msafe(dir.path())
        .args(["--json", "review"])
        .output()
        .unwrap();
    let items = json(&output)["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    assert!(items[0]["body"].is_string());

    msafe(dir.path())
        .args(["review"])
        .assert()
        .success()
        .stdout(contains("alpha memory"))
        .stdout(contains("beta memory"));
}

#[test]
fn two_namespaces_do_not_see_each_other() {
    let dir = workspace();
    msafe(dir.path())
        .args(["remember", "a memory in the agent namespace"])
        .assert()
        .success();

    let output = msafe(dir.path())
        .args(["--namespace", "notes", "--json", "review"])
        .output()
        .unwrap();
    assert_eq!(json(&output)["items"].as_array().unwrap().len(), 0);
}

#[test]
fn a_missing_scope_is_a_usage_error_not_a_default() {
    let dir = workspace();
    let output = Command::cargo_bin("msafe")
        .unwrap()
        .current_dir(dir.path())
        .args(["remember", "nowhere in particular"])
        .env_remove("MSAFE_TENANT")
        .env_remove("MSAFE_SUBJECT")
        .env_remove("MSAFE_NAMESPACE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("tenant"),
        "the error must name what is missing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_reserved_scope_is_refused_from_the_command_line_too() {
    let dir = workspace();
    let output = msafe(dir.path())
        .args([
            "--subject",
            "_admin",
            "--namespace",
            "_admin",
            "remember",
            "forged",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("_admin"));
}

// Not in the task brief's own test file, added here. Standing constraint #2
// is explicit that the reserved set is `_admin` **and** `_purged`, and that a
// hand-rolled `== ADMIN_COMPONENT` comparison (which only ever catches
// `_admin`) "has drifted twice already" in this plan's history. The test
// above alone cannot tell a correct `check_reserved` call apart from exactly
// that drifted, `_admin`-only comparison — both pass it identically. This one
// exercises the half a drifted check would silently accept.
#[test]
fn the_purged_scope_is_refused_from_the_command_line_too() {
    let dir = workspace();
    let output = msafe(dir.path())
        .args([
            "--subject",
            "_purged",
            "--namespace",
            "agent",
            "remember",
            "forged",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("_purged"));
}

#[test]
fn the_config_file_supplies_the_defaults_the_flags_override() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("msafe.toml"),
        "data_dir = \"tenants\"\ntenant = \"acme\"\nsubject = \"from-config\"\nnamespace = \"agent\"\n",
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("msafe").unwrap();
    cmd.current_dir(dir.path())
        .env_remove("MSAFE_TENANT")
        .env_remove("MSAFE_SUBJECT")
        .env_remove("MSAFE_NAMESPACE")
        .args(["--json", "remember", "stored under the configured subject"])
        .assert()
        .success();

    let mut listed = Command::cargo_bin("msafe").unwrap();
    let output = listed
        .current_dir(dir.path())
        .env_remove("MSAFE_SUBJECT")
        .args(["--json", "review"])
        .output()
        .unwrap();
    assert_eq!(json(&output)["items"].as_array().unwrap().len(), 1);

    let mut overridden = Command::cargo_bin("msafe").unwrap();
    let elsewhere = overridden
        .current_dir(dir.path())
        .args(["--subject", "someone-else", "--json", "review"])
        .output()
        .unwrap();
    assert_eq!(json(&elsewhere)["items"].as_array().unwrap().len(), 0);
}
