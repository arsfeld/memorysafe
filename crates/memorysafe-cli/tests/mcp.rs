mod support {
    use assert_cmd::Command;
    use std::path::Path;

    pub fn msafe(dir: &Path) -> Command {
        let mut command = Command::cargo_bin("msafe").expect("binary");
        command.current_dir(dir);
        command
    }
}

use support::msafe;

#[test]
fn install_writes_a_committable_local_entry() {
    let dir = tempfile::tempdir().unwrap();
    let out = msafe(dir.path()).args(["mcp", "install"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");

    let written = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
    let entry = &parsed["mcpServers"]["memorysafe"];
    assert_eq!(entry["command"], "msafe");
    assert_eq!(entry["args"][0], "serve");
}

#[test]
fn install_remote_never_writes_a_secret() {
    // `.mcp.json` is the scope Claude Code commits to source control. A
    // credential written here is a credential in git history.
    let dir = tempfile::tempdir().unwrap();
    let out = msafe(dir.path())
        .args(["mcp", "install", "--remote"])
        .env(
            "MEMORYSAFE_API_KEY",
            "msk_shouldnotappear_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let written = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    assert!(
        !written.contains("shouldnotappear"),
        "a secret reached the committed config: {written}"
    );
    assert!(written.contains("headersHelper"), "{written}");
}

#[test]
fn install_remote_portable_uses_env_expansion_rather_than_a_helper() {
    let dir = tempfile::tempdir().unwrap();
    let out = msafe(dir.path())
        .args(["mcp", "install", "--remote", "--portable"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let written = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
    assert!(written.contains("${MEMORYSAFE_API_KEY}"), "{written}");
    assert!(!written.contains("headersHelper"), "{written}");
    assert!(
        written.contains("MemorySafe-Namespace") || written.contains("memorysafe-namespace"),
        "the portable form must pin a namespace: {written}"
    );
}

#[test]
fn headers_emits_the_namespace_for_the_current_directory() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("checkout-service");
    std::fs::create_dir(&project).unwrap();

    let out = msafe(&project)
        .args(["mcp", "headers"])
        .env(
            "MEMORYSAFE_API_KEY",
            "msk_01ARZ3NDEKTSV4RRFFQ69G5FAV_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(parsed["memorysafe-namespace"], "checkout-service");
    assert!(
        parsed["Authorization"]
            .as_str()
            .unwrap()
            .starts_with("Bearer msk_"),
        "{parsed}"
    );
}

#[test]
fn headers_without_a_credential_says_so_on_stderr_and_fails() {
    // A helper that silently emits no Authorization produces a confusing 401
    // inside the MCP client. Fail loudly at the helper instead.
    let dir = tempfile::tempdir().unwrap();
    let out = msafe(dir.path())
        .args(["mcp", "headers"])
        .env_remove("MEMORYSAFE_API_KEY")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("MEMORYSAFE_API_KEY"),
        "{out:?}"
    );
}
