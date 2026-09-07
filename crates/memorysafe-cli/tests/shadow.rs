use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::path::Path;

fn msafe(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("msafe").expect("binary");
    cmd.current_dir(dir);
    cmd.env("MSAFE_TENANT", "acme");
    cmd.env("MSAFE_SUBJECT", "user-42");
    cmd.env("MSAFE_NAMESPACE", "agent");
    cmd
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("msafe.toml"), "data_dir = \"tenants\"\n").unwrap();
    dir
}

fn archive(dir: &Path) -> std::path::PathBuf {
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        msafe(dir).args(["remember", body]).assert().success();
    }
    let out = dir.join("archive");
    msafe(dir)
        .args(["export", out.to_str().unwrap(), "--include-audit"])
        .assert()
        .success();
    out
}

/// A candidate config that rejects everything, as a complete `BaselineConfig`
/// JSON document.
///
/// `BaselineConfig` (`memorysafe-policy`) derives `Deserialize` with no
/// `#[serde(default)]` on the struct or any field — unlike `MsafeConfig`,
/// which does carry `#[serde(default, deny_unknown_fields)]`. A JSON literal
/// naming only the three thresholds this test cares about therefore fails to
/// parse with a "missing field" error for every field it omits. Built from
/// `BaselineConfig::default()` with only the fields this test means to change
/// overridden, so it stays a complete document without hardcoding every
/// unrelated default inline (and without silently drifting from them).
fn reject_everything_config() -> String {
    let mut value = serde_json::to_value(memorysafe_policy::BaselineConfig::default()).unwrap();
    let obj = value.as_object_mut().unwrap();
    obj.insert("duplicate_threshold".into(), serde_json::json!(0.0));
    obj.insert("merge_threshold".into(), serde_json::json!(0.0));
    obj.insert("near_duplicate_floor".into(), serde_json::json!(0.0));
    value.to_string()
}

#[test]
fn shadow_replays_an_archive_against_the_current_policy() {
    let dir = workspace();
    let out = archive(dir.path());

    msafe(dir.path())
        .args(["shadow", out.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("coverage"))
        .stdout(contains("identical").or(contains("changed")));
}

#[test]
fn shadow_diffs_two_policy_configurations_and_reports_the_transitions() {
    let dir = workspace();
    let out = archive(dir.path());

    // A candidate that rejects everything must move every decision.
    let candidate = dir.path().join("candidate.json");
    std::fs::write(&candidate, reject_everything_config()).unwrap();

    let output = msafe(dir.path())
        .args([
            "--json",
            "shadow",
            out.to_str().unwrap(),
            "--candidate-config",
            candidate.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["diff"]["total"], serde_json::json!(3));
    assert!(
        !value["diff"]["changed"].as_array().unwrap().is_empty(),
        "a policy that rejects everything changed nothing: {value}"
    );
    assert!(value["coverage"].as_f64().is_some());
}

#[test]
fn shadow_on_an_archive_without_audit_says_there_is_nothing_to_compare() {
    let dir = workspace();
    msafe(dir.path())
        .args(["remember", "a lone memory"])
        .assert()
        .success();
    let out = dir.path().join("bare");
    msafe(dir.path())
        .args(["export", out.to_str().unwrap()])
        .assert()
        .success();

    msafe(dir.path())
        .args(["shadow", out.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("--include-audit").or(contains("recorded")));
}

#[test]
fn shadow_writes_its_report_to_a_file_when_asked() {
    let dir = workspace();
    let out = archive(dir.path());
    let report = dir.path().join("report.json");

    msafe(dir.path())
        .args([
            "shadow",
            out.to_str().unwrap(),
            "--out",
            report.to_str().unwrap(),
        ])
        .assert()
        .success();

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    assert!(value["diff"]["total"].is_u64());
}
