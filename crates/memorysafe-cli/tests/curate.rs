use assert_cmd::Command;
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

fn json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn remember(dir: &Path, body: &str, tag: &str) -> String {
    let output = msafe(dir)
        .args(["--json", "remember", body, "--tag", tag])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json(&output)["item_id"].as_str().expect("an id").to_owned()
}

#[test]
fn forgetting_by_id_and_by_tag() {
    let dir = workspace();
    let id = remember(dir.path(), "a memory to delete by id", "work");
    remember(dir.path(), "a memory to delete by tag", "chores");
    remember(dir.path(), "a memory to keep around", "keep");

    let by_id = msafe(dir.path())
        .args(["--json", "forget", "--id", &id])
        .output()
        .unwrap();
    assert!(by_id.status.success());
    assert_eq!(json(&by_id)["forgotten"].as_array().unwrap().len(), 1);

    let by_tag = msafe(dir.path())
        .args(["--json", "forget", "--tag", "chores"])
        .output()
        .unwrap();
    assert_eq!(json(&by_tag)["forgotten"].as_array().unwrap().len(), 1);

    let left = msafe(dir.path())
        .args(["--json", "review"])
        .output()
        .unwrap();
    assert_eq!(json(&left)["items"].as_array().unwrap().len(), 1);
}

#[test]
fn forget_needs_exactly_one_selector() {
    let dir = workspace();
    msafe(dir.path()).args(["forget"]).assert().failure();
    msafe(dir.path())
        .args(["forget", "--tag", "work", "--kind", "fact"])
        .assert()
        .failure();
}

#[test]
fn protecting_pins_a_memory_and_review_shows_it() {
    let dir = workspace();
    let id = remember(dir.path(), "never forget this one", "important");

    msafe(dir.path())
        .args(["protect", &id, "--level", "pinned"])
        .assert()
        .success()
        .stdout(contains("pinned"));

    let listed = msafe(dir.path())
        .args(["--json", "review"])
        .output()
        .unwrap();
    let items = json(&listed)["items"].as_array().unwrap().clone();
    let pinned = items
        .iter()
        .find(|i| i["id"] == serde_json::json!(id))
        .unwrap();
    assert_eq!(pinned["protection"]["kind"], "pinned");
}

#[test]
fn a_protected_window_needs_a_deadline() {
    let dir = workspace();
    let id = remember(dir.path(), "protect me for a week", "important");
    msafe(dir.path())
        .args(["protect", &id, "--level", "protected"])
        .assert()
        .failure()
        .stderr(contains("until"));
}

#[test]
fn the_audit_command_shows_decisions_and_never_bodies() {
    let dir = workspace();
    remember(
        dir.path(),
        "a body that must not appear in the trail",
        "secret",
    );

    let output = msafe(dir.path())
        .args(["--json", "audit"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        !text.contains("a body that must not appear"),
        "the audit command leaked a body"
    );

    let records = json(&output)["records"].as_array().unwrap().clone();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "admitted");

    msafe(dir.path())
        .args(["audit", "--event", "admitted"])
        .assert()
        .success()
        .stdout(contains("admitted"));

    msafe(dir.path())
        .args(["audit", "--event", "exploded"])
        .assert()
        .failure();
}

#[test]
fn maintenance_runs_and_reports() {
    let dir = workspace();
    for i in 0..3 {
        remember(
            dir.path(),
            &format!("healthy memory {i} about topic {i}"),
            "bulk",
        );
    }
    let output = msafe(dir.path())
        .args(["--json", "maintain"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = json(&output);
    assert_eq!(value["forgotten"], serde_json::json!(0));
    assert!(value["scanned"].as_u64().is_some());
}

#[test]
fn purging_a_subject_requires_confirmation() {
    let dir = workspace();
    remember(dir.path(), "a memory belonging to this subject", "any");

    msafe(dir.path())
        .args(["purge-subject", "user-42"])
        .assert()
        .failure()
        .stderr(contains("--yes"));

    let purged = msafe(dir.path())
        .args(["--json", "purge-subject", "user-42", "--yes"])
        .output()
        .unwrap();
    assert!(purged.status.success());
    assert_eq!(json(&purged)["items_removed"], serde_json::json!(1));

    let left = msafe(dir.path())
        .args(["--json", "review"])
        .output()
        .unwrap();
    assert_eq!(json(&left)["items"].as_array().unwrap().len(), 0);
}

#[test]
fn the_reserved_subject_cannot_be_purged() {
    let dir = workspace();
    msafe(dir.path())
        .args(["purge-subject", "_admin", "--yes"])
        .assert()
        .failure()
        .stderr(contains("_admin"));
}

#[test]
fn creating_a_key_prints_the_secret_once_and_stores_only_a_hash() {
    let dir = workspace();
    let output = msafe(dir.path())
        .args(["keys", "add", "--label", "ci runner"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let printed = String::from_utf8_lossy(&output.stdout);
    let secret = printed
        .split_whitespace()
        .find(|word| word.starts_with("msk_"))
        .expect("the secret is printed")
        .to_owned();

    let config = std::fs::read_to_string(dir.path().join("msafe.toml")).unwrap();
    assert!(
        config.contains("ci runner"),
        "the record was not saved: {config}"
    );
    let tail = secret.rsplit('_').next().unwrap();
    assert!(
        !config.contains(tail),
        "the secret was written to disk: {config}"
    );

    let listed = msafe(dir.path())
        .args(["--json", "keys", "list"])
        .output()
        .unwrap();
    let keys = json(&listed)["keys"].as_array().unwrap().clone();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["label"], "ci runner");
    assert_eq!(keys[0]["tenant"], "acme");
    assert!(keys[0].get("secret").is_none());
}

#[test]
fn creating_a_key_without_a_config_file_says_where_it_would_go() {
    // Silently creating msafe.toml in whatever directory the operator happened
    // to be in is how a key ends up committed to a repository.
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("msafe").unwrap();
    cmd.current_dir(dir.path())
        .env("MSAFE_TENANT", "acme")
        .env("MSAFE_SUBJECT", "user-42")
        .env("MSAFE_NAMESPACE", "agent")
        .args(["keys", "add", "--label", "ci"])
        .assert()
        .failure()
        .stderr(contains("msafe.toml"));
}
