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

/// Wait until the daemon serves, not merely until its socket exists.
///
/// It binds before it repairs and folds the log, so a hook that connects in
/// between waits on a daemon that is not answering yet, gives up at its 500ms
/// deadline, and drops the record — which on a busy CI runner is often enough
/// that "a segment must exist" failed. The build stamp gains its hardening
/// fields once restore is done, the same signal `recorder.rs` waits for.
fn wait_until_serving(home: &std::path::Path) {
    let socket = home.join("conbus.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let settled = std::fs::read_to_string(home.join("daemon.build"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text.trim()).ok())
            .is_some_and(|stamp| stamp.get("memory_locked").is_some());
        if settled && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("the daemon never started serving on {}", socket.display());
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
    wait_until_serving(home.path());

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
}

#[test]
fn every_real_payload_carries_the_workspace_it_happened_in() {
    // `cwd` is on the common base of every Claude Code hook payload, not on
    // the session start alone. Reading it only there recorded the workspace
    // for 7 of 3,433 conversations in a real log — the rest were backfilled or
    // had their candidate evicted — and the workspace is the key by which a
    // second harness finds the conversation it is continuing.
    //
    // The subject here is deliberately not `SessionStart`: that one would pass
    // under the old behaviour too.
    for hook_event_name in ["UserPromptSubmit", "StopFailure", "SessionEnd"] {
        let record = record_for(&payload(hook_event_name));

        assert_eq!(
            record["workspace"], "/private/tmp/conbus-probe.TRKEF4",
            "{hook_event_name} lost the workspace: {record}"
        );
    }
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

/// Real Codex 0.152.1 traffic: an interactive session in a throwaway checkout
/// (two turns, one `ls -la`), plus the session start and prompt of the
/// memory-writing agent Codex spawned beside it. Username redacted; the
/// agent's 50 KB instruction prompt cut to its first line.
const CODEX: &str = include_str!("fixtures/codex_0.152.1.jsonl");

fn codex_payloads(hook_event_name: &str) -> Vec<serde_json::Value> {
    CODEX
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("fixture is json"))
        .filter(|value| value["hook_event_name"] == hook_event_name)
        .collect()
}

/// As `record_for`, through the Codex adapter: mint with the same session's
/// real prompt, then replay the subject.
fn codex_record_for(subject: &serde_json::Value) -> serde_json::Value {
    let home = tempfile::tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_conbus");
    let mut daemon = Command::new(bin)
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    wait_until_serving(home.path());

    let minting = codex_payloads("UserPromptSubmit")
        .into_iter()
        .find(|p| p["session_id"] == subject["session_id"])
        .expect("the fixture holds a prompt for every session it holds");
    for value in [&minting, subject] {
        let mut child = Command::new(bin)
            .arg("hook")
            .arg("--harness")
            .arg("codex")
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
fn a_real_codex_prompt_mints_with_its_turn_correlator() {
    let prompt = codex_payloads("UserPromptSubmit").remove(0);

    let record = codex_record_for(&prompt);

    assert_eq!(record["harness"], "codex");
    assert_eq!(record["event"], "turn_started");
    assert_eq!(record["prompt"]["text"], "Reply with the single word OK");
    assert_eq!(record["vendor_turn_id"], prompt["turn_id"]);
    assert_eq!(record["workspace"], "/Applications/conbus-hooktest");
    assert_eq!(record["author"], serde_json::Value::Null);
}

#[test]
fn a_real_codex_tool_call_keeps_its_exec_id_on_both_halves() {
    // Codex names a tool call `exec-<uuid>`, and the join between the request
    // and its result is that id — the same join Claude's `toolu_` ids give.
    let request = codex_payloads("PreToolUse").remove(0);
    let result = codex_payloads("PostToolUse").remove(0);
    assert_eq!(request["tool_use_id"], result["tool_use_id"]);

    let requested = codex_record_for(&request);
    let used = codex_record_for(&result);

    assert_eq!(requested["event"], "tool_requested");
    assert_eq!(requested["tool"], "Bash");
    assert!(
        requested["tool_use_id"]
            .as_str()
            .unwrap_or("")
            .starts_with("exec-"),
        "{requested}"
    );
    assert_eq!(used["event"], "tool_use");
    assert_eq!(used["tool_use_id"], request["tool_use_id"]);
    assert_eq!(used["outcome"], "succeeded");
    assert_eq!(used["vendor_turn_id"], request["turn_id"]);
}

#[test]
fn a_real_codex_stop_carries_the_closing_message() {
    let stop = codex_payloads("Stop").remove(0);

    let record = codex_record_for(&stop);

    assert_eq!(record["event"], "turn_ended");
    assert_eq!(record["state"], "complete");
    assert_eq!(record["assistant_message"]["text"], "OK");
    assert_eq!(record["vendor_turn_id"], stop["turn_id"]);
}

#[test]
fn a_real_codex_session_end_keeps_its_reason() {
    let end = codex_payloads("SessionEnd").remove(0);

    let record = codex_record_for(&end);

    assert_eq!(record["event"], "session_ended");
    assert_eq!(record["reason"], "other");
}

#[test]
fn codexs_memory_agent_is_a_session_of_its_own_with_no_transcript() {
    // Codex 0.152.1 spawns a "Memory Writing Agent" beside an interactive
    // session: its own session id, `transcript_path: null`, cwd
    // `~/.codex/memories`, a 50 KB instruction prompt, and no `Stop`. It is a
    // machine's conversation and it must still normalise, or the hook fails
    // on something the person never typed.
    let start = codex_payloads("SessionStart")
        .into_iter()
        .find(|p| p["transcript_path"].is_null())
        .expect("the memory agent's session start");

    let record = codex_record_for(&start);

    assert_eq!(record["event"], "session_candidate");
    assert_eq!(record["trigger"], "startup");
    assert_eq!(record["workspace"], "/Users/user/.codex/memories");
}
