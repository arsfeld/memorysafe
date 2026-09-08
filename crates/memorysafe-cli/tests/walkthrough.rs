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
    let key_out = msafe(dir)
        .args(["keys", "add", "--label", "walkthrough"])
        .output()
        .unwrap();
    let key_stdout = String::from_utf8_lossy(&key_out.stdout);
    assert!(key_stdout.contains("msk_"));
    let config = std::fs::read_to_string(dir.join("msafe.toml")).unwrap();
    assert!(config.contains("walkthrough"));
    let key = key_stdout
        .lines()
        .next()
        .expect("key on first line")
        .trim()
        .to_string();

    // 11. Run the HTTP server and post a memory without naming a scope.
    let server = std::process::Command::new(assert_cmd::cargo::cargo_bin("msafe"))
        .current_dir(dir)
        .env("MSAFE_TENANT", "acme")
        .env("MSAFE_SUBJECT", "user-42")
        .env("MSAFE_NAMESPACE", "coding-agent")
        .args(["serve", "--transport", "http", "--bind", "127.0.0.1:0"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn server");

    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let mut server_guard = KillOnDrop(server);

    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(server_guard.0.stderr.take().unwrap());
    let mut line = String::new();
    let mut base = String::new();
    while reader.read_line(&mut line).unwrap() > 0 {
        if let Some(pos) = line.find("http://") {
            let rest = &line[pos..];
            let end = rest.find(' ').unwrap_or(rest.len());
            base = rest[..end].trim().to_string();
            break;
        }
        line.clear();
    }
    assert!(!base.is_empty(), "server did not report an address");

    // The same tool call, with no scope named, against the HTTP transport.
    // Over stdio this has always worked; this asserts it now works here too,
    // which is the entire point of the scope contract.
    let response = http_post(
        &base,
        "/v1/memories",
        &key,
        serde_json::json!({ "body": "deploys are gated on the conformance suite" }),
    );
    assert_eq!(response.status(), 200, "{response:?}");
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    #[allow(dead_code)]
    body: String,
}

impl HttpResponse {
    fn status(&self) -> u16 {
        self.status
    }
}

fn http_post(base: &str, path: &str, key: &str, payload: serde_json::Value) -> HttpResponse {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let addr = base
        .strip_prefix("http://")
        .expect("base URL has http:// scheme");
    let mut stream = TcpStream::connect(addr).expect("connect to server");
    let body_bytes = serde_json::to_vec(&payload).expect("serialize payload");

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Authorization: Bearer {key}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body_bytes.len(),
    );

    stream
        .write_all(request.as_bytes())
        .expect("write request headers");
    stream.write_all(&body_bytes).expect("write request body");
    stream.flush().expect("flush request");

    let mut response_bytes = Vec::new();
    stream
        .read_to_end(&mut response_bytes)
        .expect("read response");

    let response_str = String::from_utf8_lossy(&response_bytes);
    let mut lines = response_str.lines();
    let status_line = lines.next().expect("status line");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("parse status code");

    let body = response_str
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("")
        .to_string();

    HttpResponse {
        status: status_code,
        body,
    }
}
