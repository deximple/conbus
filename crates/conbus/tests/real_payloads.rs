//! Replays hook payloads captured from a real Claude Code session.
//!
//! `crates/conbus/tests/fixtures/claude_code_2.1.241.jsonl` is real traffic,
//! not a construction: one headless session, its username redacted. Reading
//! field names out of the binary's strings got `StopFailure` wrong, so the
//! adapter is pinned against traffic from here on.

use std::process::{Command, Stdio};

const FIXTURE: &str = include_str!("fixtures/claude_code_2.1.241.jsonl");

fn payload(hook_event_name: &str) -> serde_json::Value {
    FIXTURE
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("fixture is json"))
        .find(|value| value["hook_event_name"] == hook_event_name)
        .unwrap_or_else(|| panic!("no {hook_event_name} in the fixture"))
}

/// Run the payload through `conbus hook` against a live daemon and return the
/// record it produced.
fn record_for(subject: &serde_json::Value) -> serde_json::Value {
    let home = tempfile::tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_conbus");
    let mut daemon = Command::new(bin)
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let socket = home.path().join("conbus.sock");
    for _ in 0..250 {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Mint a conversation first: only a prompt binds a `bus_id`.
    for value in [&payload("UserPromptSubmit"), subject] {
        let mut child = Command::new(bin)
            .arg("hook")
            .env("CONBUS_HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn hook");
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(value.to_string().as_bytes())
            .expect("write");
        child.wait().expect("hook exit");
    }

    let _ = daemon.kill();
    let _ = daemon.wait();

    let segment = home.path().join("log").join("segment-00000000.jsonl");
    let text = std::fs::read_to_string(&segment).expect("a segment must exist");
    let last = text.lines().rfind(|l| !l.is_empty()).expect("a record");
    serde_json::from_str(last).expect("record is json")
}

#[test]
fn the_stop_failure_reason_survives_the_real_payload() {
    // The field is `error`. `failure_reason` is a string that appears in the
    // binary but is not what the hook sends, which is exactly the kind of
    // silent degradation that looks like it works.
    let record = record_for(&payload("StopFailure"));

    assert_eq!(record["event"], "turn_ended");
    assert_eq!(
        record["reason"], "authentication_failed",
        "the vendor's reason was lost: {record}"
    );
}

#[test]
fn the_last_assistant_message_is_recorded_without_touching_the_transcript() {
    // Claude Code puts the assistant's closing text on the payload itself and
    // says why: "Avoids the need to read and parse the transcript file." So the
    // log can hold the conversation without breaking the live-path rule.
    let record = record_for(&payload("StopFailure"));

    assert_eq!(
        record["assistant_message"]["text"],
        "Failed to authenticate: OAuth session expired and could not be refreshed",
        "the assistant's closing message was dropped: {record}"
    );
}

#[test]
fn a_real_session_start_and_prompt_still_normalise() {
    let record = record_for(&payload("SessionStart"));

    assert_eq!(record["event"], "session_candidate");
    assert_eq!(record["trigger"], "startup");
    assert_eq!(record["cwd"], "/private/tmp/conbus-probe.TRKEF4");
}

#[test]
fn a_real_session_end_keeps_its_reason() {
    let record = record_for(&payload("SessionEnd"));

    assert_eq!(record["event"], "session_ended");
    assert_eq!(record["reason"], "other");
}
