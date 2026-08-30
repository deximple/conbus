//! Replays hook payloads captured from a real Claude Code session.
//!
//! `crates/conbus/tests/fixtures/claude_code_2.1.241.jsonl` is real traffic,
//! not a construction: one headless session, its username redacted. Reading
//! field names out of the binary's strings got `StopFailure` wrong, so the
//! adapter is pinned against traffic from here on.

use std::process::{Command, Stdio};

const FIXTURE: &str = include_str!("fixtures/claude_code_2.1.241.jsonl");
/// A second real session, captured with a hook that tees stdin. Adds
/// `MessageDisplay` and shows what `SessionEnd` really carries.
const FIXTURE2: &str = include_str!("fixtures/claude_code_2.1.241_probe2.jsonl");

fn payload_from(fixture: &str, hook_event_name: &str) -> serde_json::Value {
    fixture
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("fixture is json"))
        .find(|value| value["hook_event_name"] == hook_event_name)
        .unwrap_or_else(|| panic!("no {hook_event_name} in the fixture"))
}

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
    let minting = payload_from(
        if subject["session_id"] == payload_from(FIXTURE2, "SessionStart")["session_id"] {
            FIXTURE2
        } else {
            FIXTURE
        },
        "UserPromptSubmit",
    );
    for value in [&minting, subject] {
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

#[test]
fn a_real_session_end_carries_the_turn_that_was_in_flight() {
    // `prompt_id` on `SessionEnd` names the turn that just finished — this
    // conversation's, by the vendor's own definition of the field. It is not
    // stale. The value only belongs to another conversation on a resumed or
    // forked `SessionStart`, which is the single case the adapter drops.
    let session_end = payload_from(FIXTURE2, "SessionEnd");
    let prompt = payload_from(FIXTURE2, "UserPromptSubmit");
    assert_eq!(
        session_end["prompt_id"], prompt["prompt_id"],
        "this test assumes the capture's session end closes the captured turn"
    );

    let record = record_for(&session_end);

    assert_eq!(record["event"], "session_ended");
    assert_eq!(record["vendor_turn_id"], session_end["prompt_id"]);
}

#[test]
fn a_real_stop_failure_keeps_its_turn_correlator() {
    // The event that matters most to the router, and the one whose real payload
    // carries no `prompt` key — so a correlator gated on the wrong field would
    // lose it here and nowhere a synthetic test would look.
    let stop_failure = payload_from(FIXTURE2, "StopFailure");
    assert!(stop_failure.get("prompt").is_none());

    let record = record_for(&stop_failure);

    assert_eq!(record["event"], "turn_ended");
    assert_eq!(record["vendor_turn_id"], stop_failure["prompt_id"]);
}

#[test]
fn a_real_prompt_carries_its_turn_correlator_and_no_author() {
    // 2.1.241 never emits `source` — both UserPromptSubmit emit sites have it
    // compiled out — and this capture is from a `-p` run, which would have to
    // be `sdk` if it were emitted at all.
    let prompt = payload_from(FIXTURE2, "UserPromptSubmit");
    assert!(
        prompt.get("source").is_none(),
        "the vendor started sending source: {prompt}"
    );

    let record = record_for(&prompt);

    assert_eq!(record["event"], "turn_started");
    assert_eq!(record["vendor_turn_id"], prompt["prompt_id"]);
    assert_eq!(
        record["author"],
        serde_json::Value::Null,
        "an author was invented for a payload that named none: {record}"
    );
}

#[test]
fn a_real_session_start_carries_no_correlator() {
    let session_start = payload_from(FIXTURE2, "SessionStart");
    assert!(session_start.get("prompt_id").is_none());

    let record = record_for(&session_start);

    assert_eq!(record["event"], "session_candidate");
    assert_eq!(record["vendor_turn_id"], serde_json::Value::Null);
}
