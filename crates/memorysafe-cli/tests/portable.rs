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

fn seed(dir: &Path) {
    for body in [
        "the production migration runs on Sundays",
        "the on-call rotation starts Monday morning",
        "the staging cluster is rebuilt every night",
    ] {
        msafe(dir).args(["remember", body]).assert().success();
    }
}

#[test]
fn export_writes_an_archive_a_person_can_read() {
    let dir = workspace();
    seed(dir.path());

    let out = dir.path().join("archive");
    msafe(dir.path())
        .args(["export", out.to_str().unwrap()])
        .assert()
        .success();

    let ndjson = std::fs::read_to_string(out.join("memories.ndjson")).unwrap();
    assert!(
        ndjson.lines().count() >= 4,
        "a header line and three item lines"
    );
    for line in ndjson.lines() {
        let value: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
        assert!(
            value.get("record").is_some(),
            "each line names its record type"
        );
    }

    let markdown = std::fs::read_to_string(out.join("memories.md")).unwrap();
    assert!(markdown.contains("# MemorySafe export"));
    assert!(markdown.contains("the production migration runs on Sundays"));

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["tenant"], "acme");
    assert_eq!(manifest["subject"], "user-42");
    assert_eq!(manifest["namespace"], "agent");
    assert!(manifest["ndjson_blake3"].is_string());
    assert!(manifest["exported_at"].is_i64());
}

#[test]
fn an_archive_round_trips_into_a_fresh_workspace() {
    let source = workspace();
    seed(source.path());
    let archive = source.path().join("archive");
    msafe(source.path())
        .args(["export", archive.to_str().unwrap()])
        .assert()
        .success();

    let target = workspace();
    msafe(target.path())
        .args(["import", archive.to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("3"));

    let listed = msafe(target.path())
        .args(["--json", "review"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let bodies: Vec<&str> = value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies.len(), 3);
    assert!(bodies.contains(&"the production migration runs on Sundays"));
}

#[test]
fn importing_the_same_archive_twice_changes_nothing_the_second_time() {
    let source = workspace();
    seed(source.path());
    let archive = source.path().join("archive");
    msafe(source.path())
        .args(["export", archive.to_str().unwrap()])
        .assert()
        .success();

    let target = workspace();
    msafe(target.path())
        .args(["import", archive.to_str().unwrap()])
        .assert()
        .success();
    let second = msafe(target.path())
        .args(["--json", "import", archive.to_str().unwrap()])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(report["items_imported"], serde_json::json!(0));
    assert_eq!(report["items_skipped_existing"], serde_json::json!(3));
}

#[test]
fn a_corrupted_archive_is_refused_before_anything_is_written() {
    let source = workspace();
    seed(source.path());
    let archive = source.path().join("archive");
    msafe(source.path())
        .args(["export", archive.to_str().unwrap()])
        .assert()
        .success();

    // Drop the last item line: the file still parses, so only the digest can
    // catch it.
    let ndjson = std::fs::read_to_string(archive.join("memories.ndjson")).unwrap();
    let truncated: Vec<&str> = ndjson.lines().take(ndjson.lines().count() - 1).collect();
    std::fs::write(archive.join("memories.ndjson"), truncated.join("\n") + "\n").unwrap();

    let target = workspace();
    msafe(target.path())
        .args(["import", archive.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("digest"));

    let listed = msafe(target.path())
        .args(["--json", "review"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(
        value["items"].as_array().unwrap().len(),
        0,
        "a refused import wrote memories anyway"
    );
}

#[test]
fn a_bare_ndjson_file_can_be_imported_without_a_manifest() {
    // The engine's own export format, handed over by some other route, must
    // still be importable — the manifest is a convenience, not the format.
    let source = workspace();
    seed(source.path());
    let archive = source.path().join("archive");
    msafe(source.path())
        .args(["export", archive.to_str().unwrap()])
        .assert()
        .success();

    let target = workspace();
    msafe(target.path())
        .args(["import", archive.join("memories.ndjson").to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn export_narrows_to_the_scope_unless_told_otherwise() {
    let dir = workspace();
    msafe(dir.path())
        .args(["remember", "a memory in the agent namespace"])
        .assert()
        .success();
    msafe(dir.path())
        .args([
            "--namespace",
            "notes",
            "remember",
            "a memory in the notes namespace",
        ])
        .assert()
        .success();

    let narrow = dir.path().join("narrow");
    msafe(dir.path())
        .args(["export", narrow.to_str().unwrap()])
        .assert()
        .success();
    let narrow_text = std::fs::read_to_string(narrow.join("memories.ndjson")).unwrap();
    assert!(narrow_text.contains("agent namespace"));
    assert!(
        !narrow_text.contains("notes namespace"),
        "the export ignored its scope"
    );

    let wide = dir.path().join("wide");
    msafe(dir.path())
        .args(["export", wide.to_str().unwrap(), "--all-namespaces"])
        .assert()
        .success();
    let wide_text = std::fs::read_to_string(wide.join("memories.ndjson")).unwrap();
    assert!(wide_text.contains("agent namespace"));
    assert!(wide_text.contains("notes namespace"));
}

#[test]
fn export_refuses_to_overwrite_an_archive_that_is_already_there() {
    let dir = workspace();
    seed(dir.path());
    let out = dir.path().join("archive");
    msafe(dir.path())
        .args(["export", out.to_str().unwrap()])
        .assert()
        .success();
    msafe(dir.path())
        .args(["export", out.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("--force"));
    msafe(dir.path())
        .args(["export", out.to_str().unwrap(), "--force"])
        .assert()
        .success();
}
