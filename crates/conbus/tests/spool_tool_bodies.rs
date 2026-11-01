//! Under `encrypted_tools` a tool body that goes through the spool cannot be
//! sealed: the hook has no key and must not store plaintext, so it keeps the
//! size. That is a real loss, and it has to be somewhere a person will see it.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SESSION: &str = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";
const KEK: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

fn start(home: &Path, retention: &str) -> Child {
    let mut child = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .env("CONBUS_RETENTION", retention)
        .env("CONBUS_KEK_HEX", KEK)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("daemon");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let stamp: serde_json::Value = std::fs::read_to_string(home.join("daemon.build"))
            .ok()
            .and_then(|text| serde_json::from_str(text.trim()).ok())
            .unwrap_or_default();
        if stamp["pid"].as_u64() == Some(u64::from(child.id())) && stamp.get("serving").is_some() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("the daemon never served");
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn hook(home: &Path, payload: &serde_json::Value) {
    let mut child = Command::new(bin())
        .args(["hook", "--harness", "claude_code"])
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write");
    child.wait().expect("wait");
}

fn prompt() -> serde_json::Value {
    serde_json::json!({
        "session_id": SESSION, "hook_event_name": "UserPromptSubmit",
        "prompt": "work", "prompt_id": "p0", "cwd": "/repo"
    })
}

fn tool_call(id: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": SESSION, "hook_event_name": "PreToolUse", "tool_name": "Bash",
        "tool_input": {"command": "the secret command"}, "tool_use_id": id, "cwd": "/repo"
    })
}

fn tool_record(home: &Path, id: &str) -> serde_json::Value {
    let mut paths: Vec<_> = std::fs::read_dir(home.join("log"))
        .expect("log")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
        .iter()
        .flat_map(|path| {
            String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default())
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .collect::<Vec<_>>()
        })
        .find(|record| record["tool_use_id"] == id)
        .expect("the tool record")
}

fn drain(home: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let left = std::fs::read_dir(home.join("spool"))
            .map(|dir| dir.filter_map(Result::ok).count())
            .unwrap_or(0);
        if left == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the spool never drained");
}

fn drops(home: &Path) -> String {
    std::fs::read_to_string(home.join("drops.jsonl")).unwrap_or_default()
}

fn doctor(home: &Path) -> String {
    let output = Command::new(bin())
        .arg("doctor")
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_AGY_BIN", "/nonexistent/agy")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .expect("doctor");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_spooled_tool_body_that_could_not_be_sealed_is_counted_where_a_person_looks() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path(), "encrypted_tools");
    hook(home.path(), &prompt());
    hook(home.path(), &tool_call("live"));
    stop(daemon);
    hook(home.path(), &tool_call("spooled"));

    let entries: Vec<String> = std::fs::read_dir(home.path().join("spool"))
        .expect("spool")
        .filter_map(Result::ok)
        .map(|entry| std::fs::read_to_string(entry.path()).expect("entry"))
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(
        !entries[0].contains("the secret command"),
        "no plaintext in the spool: {}",
        entries[0]
    );

    let daemon = start(home.path(), "encrypted_tools");
    drain(home.path());
    stop(daemon);

    assert_eq!(
        tool_record(home.path(), "live")["input"]["kind"],
        "sealed",
        "what reached the daemon is sealed"
    );
    assert_eq!(
        tool_record(home.path(), "spooled")["input"]["kind"],
        "elided"
    );
    let said = drops(home.path());
    assert_eq!(said.lines().count(), 1, "one record, one line: {said}");
    assert!(said.contains("elided, not sealed"), "{said}");
    assert!(
        said.contains("tool_requested for session ea1d23e9"),
        "{said}"
    );
    assert!(!said.contains("the secret command"), "{said}");
    let doctor = doctor(home.path());
    assert!(doctor.contains("elided, not sealed"), "{doctor}");
}

#[test]
fn a_key_that_could_not_be_read_is_counted_the_same_way() {
    // A value that is not a key stands in for a Keychain that would not answer.
    let home = tempfile::tempdir().expect("tempdir");
    let mut daemon = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .env("HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .env("CONBUS_RETENTION", "encrypted_tools")
        .env("CONBUS_KEK_HEX", "not a key")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("daemon");
    let socket = home.path().join("conbus.sock");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(20));
    }

    hook(home.path(), &prompt());
    hook(home.path(), &tool_call("live"));
    let _ = daemon.kill();
    let _ = daemon.wait();

    assert_eq!(tool_record(home.path(), "live")["input"]["kind"], "elided");
    assert!(drops(home.path()).contains("elided, not sealed"));
}

#[test]
fn eliding_is_no_loss_where_it_is_what_retention_asked_for() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path(), "conversation_only");
    hook(home.path(), &prompt());
    stop(daemon);
    hook(home.path(), &tool_call("spooled"));

    let daemon = start(home.path(), "conversation_only");
    drain(home.path());
    stop(daemon);

    assert_eq!(
        tool_record(home.path(), "spooled")["input"]["kind"],
        "elided"
    );
    assert_eq!(drops(home.path()), "");
}

#[test]
fn a_prompt_that_went_through_the_spool_has_no_tool_body_to_lose() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path(), "encrypted_tools");
    stop(daemon);
    hook(home.path(), &prompt());

    let daemon = start(home.path(), "encrypted_tools");
    drain(home.path());
    stop(daemon);

    assert_eq!(drops(home.path()), "");
}
