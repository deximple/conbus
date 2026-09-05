//! `conbus quota` against a stand-in app-server: the real one needs an
//! account, and the shape it answers with was captured on 2026-09-03
//! (see `fixtures/codex_0.152.1_contract.md`).

use std::process::Command;

/// A stand-in `codex app-server`: speaks just enough JSON-RPC over stdio to
/// answer `initialize` and `account/rateLimits/read` with the captured shape.
const FAKE_APP_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if msg.get("method") == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"userAgent": "fake"}}), flush=True)
    elif msg.get("method") == "account/rateLimits/read":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"rateLimits": {
            "limitId": "codex", "limitName": None,
            "primary": {"usedPercent": 22, "windowDurationMins": 300, "resetsAt": 4102444800},
            "secondary": {"usedPercent": 9, "windowDurationMins": 10080, "resetsAt": 4102444800},
            "credits": {"hasCredits": False, "unlimited": False, "balance": "0"},
            "individualLimit": None, "spendControlReached": False, "planType": "plus",
            "rateLimitReachedType": None},
            "rateLimitResetCredits": {"availableCount": 1, "credits": []},
            "accountId": "00000000-0000-0000-0000-000000000000", "rateLimitUpsell": None}}), flush=True)
"#;

fn fake_app_server() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("codex");
    std::fs::write(&path, FAKE_APP_SERVER).expect("write");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (dir, path)
}

#[test]
fn quota_reads_the_accounts_windows_through_the_app_server() {
    let (_dir, codex) = fake_app_server();

    let output = Command::new(env!("CARGO_BIN_EXE_conbus"))
        .arg("quota")
        .env("CONBUS_CODEX_BIN", &codex)
        .output()
        .expect("run quota");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("codex"), "{stdout}");
    assert!(stdout.contains("22%"), "the five-hour window: {stdout}");
    assert!(stdout.contains("9%"), "the weekly window: {stdout}");
    assert!(stdout.contains("plus"), "the plan: {stdout}");
    assert!(
        stdout.contains("not exhausted"),
        "the router's signal: {stdout}"
    );
}

#[test]
fn quota_says_so_when_the_app_server_is_not_there() {
    let output = Command::new(env!("CARGO_BIN_EXE_conbus"))
        .arg("quota")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .output()
        .expect("run quota");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("codex"),
        "the failure must name what could not be run"
    );
}

#[test]
fn doctor_carries_the_quota_line() {
    // The router's signal belongs where every other health fact is.
    let (_dir, codex) = fake_app_server();
    let home = tempfile::tempdir().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_conbus"))
        .arg("doctor")
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_CODEX_BIN", &codex)
        .output()
        .expect("run doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.starts_with("quota ")
            && line.contains("22%")
            && line.contains("not exhausted")),
        "doctor must show the account's windows:\n{stdout}"
    );
}

#[test]
fn quota_prints_the_live_exhausted_signal() {
    // The 2026-09-06 capture, not the "primary" guess. A fake app-server that
    // speaks the live shape is how we know `conbus quota` will say so again.
    let reply = include_str!("fixtures/codex_0.152.1_quota_exhausted.json");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, sys
REPLY = json.loads({reply:?})
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if msg.get("method") == "initialize":
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": {{"userAgent": "fake"}}}}), flush=True)
    elif msg.get("method") == "account/rateLimits/read":
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": REPLY}}), flush=True)
"#
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("codex");
    std::fs::write(&path, script).expect("write");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let output = Command::new(env!("CARGO_BIN_EXE_conbus"))
        .arg("quota")
        .env("CONBUS_CODEX_BIN", &path)
        .output()
        .expect("run quota");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("EXHAUSTED (rate_limit_reached)"),
        "the live signal: {stdout}"
    );
    assert!(stdout.contains("100%"), "the five-hour window: {stdout}");
}

#[test]
fn doctor_says_when_the_quota_cannot_be_read() {
    let home = tempfile::tempdir().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_conbus"))
        .arg("doctor")
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .output()
        .expect("run doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("quota ") && line.contains("unavailable")),
        "an unreadable quota must be said, not omitted:\n{stdout}"
    );
}
