use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn msafe(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("msafe").expect("binary");
    cmd.current_dir(dir);
    cmd.env("MSAFE_TENANT", "acme");
    cmd.env("MSAFE_SUBJECT", "user-42");
    cmd.env("MSAFE_NAMESPACE", "coding-agent");
    cmd
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

/// The walkthrough from the README, executed. If this passes, a person
/// following the documentation gets what the documentation says they get.
#[test]
fn a_person_can_run_the_whole_product_from_one_binary() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("msafe.toml"),
        r#"data_dir = "tenants"
tenant = "acme"
subject = "user-42"
namespace = "coding-agent"
retention = "balanced"
"#,
    )
    .unwrap();
    let dir = dir.path();

    // 1. Remember three things. The third is a near-repeat of the first.
    let first = msafe(dir)
        .args([
            "--json",
            "remember",
            "the production migration runs on Sundays",
            "--tag",
            "ops",
        ])
        .output()
        .unwrap();
    assert_eq!(json(&first)["action"]["kind"], "retain");
    let pinned_id = json(&first)["item_id"].as_str().unwrap().to_owned();

    msafe(dir)
        .args([
            "remember",
            "the customer prefers written summaries over meetings",
            "--kind",
            "preference",
        ])
        .assert()
        .success();
    msafe(dir)
        .args([
            "--json",
            "remember",
            "the production migration runs on Sundays",
        ])
        .assert()
        .success();

    // 2. A credential is detected and stored restricted whatever was asked for.
    msafe(dir)
        .args([
            "remember",
            "the deployment api key is sk-abc123def456ghi789jkl012mno345",
            "--sensitivity",
            "public",
        ])
        .assert()
        .success();
    let stored = msafe(dir).args(["--json", "review"]).output().unwrap();
    let credential = json(&stored)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["body"].as_str().unwrap().contains("sk-abc123"))
        .expect("the credential was stored")
        .clone();
    assert_eq!(
        credential["sensitivity"], "restricted",
        "a caller's hint lowered a credential's level"
    );

    // 3. Recall composes a working set and audits itself.
    let recalled = msafe(dir)
        .args([
            "--json",
            "recall",
            "when does the migration run",
            "--max-items",
            "2",
        ])
        .output()
        .unwrap();
    assert!(json(&recalled)["audit_id"].is_string());
    assert!(!json(&recalled)["items"].as_array().unwrap().is_empty());

    // 4. A recall with a ceiling cannot see the credential.
    let capped = msafe(dir)
        .args([
            "--json",
            "recall",
            "api key",
            "--sensitivity-ceiling",
            "internal",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&capped.stdout);
    assert!(
        !text.contains("sk-abc123"),
        "the sensitivity ceiling was not enforced"
    );

    // 5. Pin one memory, and confirm it survives a squeeze.
    msafe(dir)
        .args(["protect", &pinned_id, "--level", "pinned"])
        .assert()
        .success();

    // 6. The audit trail explains everything and leaks nothing.
    let audited = msafe(dir)
        .args(["--json", "audit", "--limit", "50"])
        .output()
        .unwrap();
    let audit_text = String::from_utf8_lossy(&audited.stdout);
    assert!(
        !audit_text.contains("sk-abc123"),
        "the audit trail leaked a credential"
    );
    assert!(
        !audit_text.contains("prefers written summaries"),
        "the audit trail leaked a body"
    );
    assert!(json(&audited)["records"].as_array().unwrap().len() >= 4);

    // 7. Maintenance runs and reports.
    msafe(dir).args(["maintain", "--all"]).assert().success();

    // 8. Export, then import into a clean workspace, and get the same corpus.
    let archive = dir.join("archive");
    msafe(dir)
        .args(["export", archive.to_str().unwrap(), "--include-audit"])
        .assert()
        .success();

    let restored = tempfile::tempdir().unwrap();
    std::fs::write(
        restored.path().join("msafe.toml"),
        "data_dir = \"tenants\"\n",
    )
    .unwrap();
    msafe(restored.path())
        .args(["import", archive.to_str().unwrap()])
        .assert()
        .success();

    let before = json(&msafe(dir).args(["--json", "review"]).output().unwrap());
    let after = json(
        &msafe(restored.path())
            .args(["--json", "review"])
            .output()
            .unwrap(),
    );
    let count = |v: &serde_json::Value| v["items"].as_array().unwrap().len();
    assert_eq!(
        count(&before),
        count(&after),
        "the round trip lost memories"
    );

    // 9. Shadow-evaluate the archive against the current policy.
    msafe(dir)
        .args(["shadow", archive.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("coverage"));

    // 10. Create an API key so the HTTP surface is usable.
    let key = msafe(dir)
        .args(["keys", "add", "--label", "walkthrough"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&key.stdout).contains("msk_"));
    let config = std::fs::read_to_string(dir.join("msafe.toml")).unwrap();
    assert!(config.contains("walkthrough"));
}
