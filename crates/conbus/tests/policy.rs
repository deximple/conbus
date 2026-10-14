//! End-to-end for `policy.json`: the hook process decides a PreToolUse before
//! the daemon is ever asked, so a down recorder cannot lift a deny.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

/// `conbus hook` with a payload on stdin and no daemon listening: the
/// `home` is a fresh tempdir, so nothing ever answers on its socket.
fn run_hook(home: &Path, harness: &str, payload: &str) -> (i32, String) {
    let mut child = Command::new(bin())
        .arg("hook")
        .arg("--harness")
        .arg(harness)
        .env("CONBUS_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let output = child.wait_with_output().expect("hook exit");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// `conbus policy ...` against the same root.
fn run_policy(home: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(bin())
        .arg("policy")
        .args(args)
        .env("CONBUS_HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn policy")
        .wait_with_output()
        .expect("policy exit");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const CANARY: &str = "conbus-policy-canary-7f3b9a";

fn deny_policy(home: &Path) {
    std::fs::write(
        home.join("policy.json"),
        serde_json::json!({
            "version": 1,
            "rules": [{
                "tool": "Bash",
                "input_contains": [CANARY],
                "decision": "deny",
                "reason": "the canary must never run"
            }]
        })
        .to_string(),
    )
    .expect("write policy");
}

fn claude_pretool(command: &str) -> String {
    serde_json::json!({
        "session_id": "s-1",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "tool_use_id": "toolu_01ABC"
    })
    .to_string()
}

#[test]
fn a_deny_reaches_stdout_even_when_no_daemon_answers() {
    let home = tempfile::tempdir().expect("tempdir");
    deny_policy(home.path());

    let (code, stdout) = run_hook(
        home.path(),
        "claude_code",
        &claude_pretool(&format!("echo {CANARY}")),
    );

    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim(),
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "the canary must never run"
            }
        })
        .to_string()
    );
}

#[test]
fn codex_gets_the_same_decision_with_no_control_fields() {
    // Codex 0.152.1 fails a PreToolUse reply that carries `continue` or
    // `suppressOutput`; the decision object must never grow them.
    let home = tempfile::tempdir().expect("tempdir");
    deny_policy(home.path());
    let payload = serde_json::json!({
        "session_id": "s-1",
        "turn_id": "t-1",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": format!("echo {CANARY}")},
        "tool_use_id": "exec-1"
    })
    .to_string();

    let (code, stdout) = run_hook(home.path(), "codex", &payload);

    assert_eq!(code, 0);
    let reply: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json reply");
    assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(reply.get("continue").is_none(), "{stdout}");
    assert!(reply.get("suppressOutput").is_none(), "{stdout}");
}

#[test]
fn codex_treats_an_ask_as_no_decision() {
    // Codex 0.152.1+ fails a PreToolUse reply that says `ask`; the wire
    // answer for it is Codex's own silent reply, not a malformed decision.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join("policy.json"),
        serde_json::json!({
            "version": 1,
            "rules": [{
                "tool": "Bash",
                "input_contains": [CANARY],
                "decision": "ask",
                "reason": "check first"
            }]
        })
        .to_string(),
    )
    .expect("write policy");
    let payload = serde_json::json!({
        "session_id": "s-1",
        "turn_id": "t-1",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": format!("echo {CANARY}")},
        "tool_use_id": "exec-1"
    })
    .to_string();

    let (code, stdout) = run_hook(home.path(), "codex", &payload);

    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "{}");
}

#[test]
fn a_cursor_payload_is_never_decided() {
    // Cursor runs the same registration with camelCase events; whatever its
    // fields say, policy is a Claude/Codex contract and Cursor's reply stays
    // the silent one.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join("policy.json"),
        serde_json::json!({
            "version": 1,
            "rules": [{
                "tool": "*",
                "decision": "deny",
                "reason": "deny everything"
            }]
        })
        .to_string(),
    )
    .expect("write policy");
    let payload = serde_json::json!({
        "conversation_id": "c-1",
        "generation_id": "g-1",
        "cursor_version": "3.19.13",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "echo hi"}
    })
    .to_string();

    let (code, stdout) = run_hook(home.path(), "claude_code", &payload);

    assert_eq!(code, 0);
    assert!(
        !stdout.contains("permissionDecision"),
        "a Cursor payload was decided: {stdout}"
    );
}

#[test]
fn an_unparseable_policy_decides_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(home.path().join("policy.json"), "{ not json").expect("write");

    let (code, stdout) = run_hook(
        home.path(),
        "claude_code",
        &claude_pretool(&format!("echo {CANARY}")),
    );

    assert_eq!(code, 0);
    assert!(
        !stdout.contains("permissionDecision"),
        "a broken file must fail open: {stdout}"
    );
}

#[test]
fn policy_check_lists_rules_or_names_the_error() {
    let home = tempfile::tempdir().expect("tempdir");
    deny_policy(home.path());
    let (code, stdout, _) = run_policy(home.path(), &["check"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("1 rule"), "{stdout}");

    std::fs::write(home.path().join("policy.json"), "{ not json").expect("write");
    let (code, _, stderr) = run_policy(home.path(), &["check"]);
    assert_ne!(code, 0, "a broken file must fail check: {stderr}");
}

#[test]
fn policy_test_prints_the_decision_or_none() {
    let home = tempfile::tempdir().expect("tempdir");
    deny_policy(home.path());

    let (code, stdout, _) = run_policy(
        home.path(),
        &[
            "test",
            "--harness",
            "claude_code",
            "--tool",
            "Bash",
            "--input",
            &serde_json::json!({"command": format!("echo {CANARY}")}).to_string(),
        ],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("deny"), "{stdout}");

    let (code, stdout, _) = run_policy(
        home.path(),
        &[
            "test",
            "--harness",
            "claude_code",
            "--tool",
            "Read",
            "--input",
            "{}",
        ],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("none"), "{stdout}");
}
