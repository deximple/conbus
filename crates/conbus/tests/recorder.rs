//! End-to-end: a real `conbus` binary, a real socket, a real log on disk.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

/// A running daemon that is stopped and reaped however the test ends, so a
/// failing assertion cannot leave a process holding the socket.
struct Daemon(Child);

impl Daemon {
    fn stop(mut self) {
        self.kill_and_reap();
    }

    fn kill_and_reap(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

/// Start a daemon rooted at `home` and wait for its socket to answer.
fn start_daemon(home: &Path) -> Daemon {
    let child = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    let daemon = Daemon(child);
    let socket = home.join("conbus.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return daemon;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon never started listening on {}", socket.display());
}

/// Run `conbus hook` with a payload on stdin. Returns (exit code, stderr).
fn run_hook(home: &Path, payload: &str, extra_env: &[(&str, &str)]) -> (i32, String) {
    let mut command = Command::new(bin());
    command
        .arg("hook")
        .env("CONBUS_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let output = child.wait_with_output().expect("hook exit");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn log_lines(home: &Path) -> Vec<serde_json::Value> {
    let dir = home.join("log");
    let mut segments: Vec<_> = std::fs::read_dir(&dir)
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_else(|_| Vec::new());
    segments.sort();
    segments
        .iter()
        .flat_map(|path| {
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_str(line).expect("a log line must be json"))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn a_whole_turn_reaches_the_log_through_the_real_binary() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let (code, stderr) = run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"SessionStart","source":"startup","cwd":"/repo"}"#,
        &[],
    );
    assert_eq!(code, 0, "session start hook failed: {stderr}");
    assert!(
        log_lines(home.path()).is_empty(),
        "a session start alone must not reach the log"
    );

    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"ship the recorder"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"PostToolUse","tool_name":"Read",
            "tool_input":{"file_path":"/etc/hosts"},"tool_response":{"ok":true}}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"Stop","stop_hook_active":false}"#,
        &[],
    );

    let lines = log_lines(home.path());
    let kinds: Vec<&str> = lines
        .iter()
        .map(|l| l["event"].as_str().expect("event"))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "session_candidate",
            "turn_started",
            "tool_use",
            "turn_ended"
        ],
        "got {lines:#?}"
    );

    let bus_ids: Vec<&str> = lines
        .iter()
        .map(|l| l["bus_id"].as_str().expect("bus_id"))
        .collect();
    assert!(
        bus_ids.windows(2).all(|w| w[0] == w[1]),
        "one session must be one conversation: {bus_ids:?}"
    );
    assert!(
        lines.iter().all(|l| l["v"] == 1),
        "every record carries a schema version"
    );
    assert!(
        lines.iter().all(|l| l["harness"] == "claude_code"),
        "every record carries a vendor stamp"
    );

    // Default retention: the tool body must not be on disk.
    let tool = &lines[2];
    assert_eq!(
        tool["input"]["kind"], "elided",
        "tool input reached disk: {tool}"
    );
    assert_eq!(
        tool["output"]["kind"], "elided",
        "tool output reached disk: {tool}"
    );
    assert_eq!(tool["output"]["bytes"], 11);

    daemon.stop();
}

#[test]
fn the_correlators_cross_the_socket_and_stay_with_their_own_records() {
    // `conbus hook` is a separate process, so a field the adapter parses can
    // still be lost between there and the log. And the flushed session start
    // must not inherit the correlator of the prompt that flushed it.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"SessionStart","source":"startup","cwd":"/repo"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"review this",
            "prompt_id":"TURN-1","source":"schedule_wakeup"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"PostToolUse","tool_name":"Read",
            "tool_input":{},"tool_response":{},"prompt_id":"TURN-1",
            "agent_id":"AGENT-7","agent_type":"code-reviewer"}"#,
        &[],
    );

    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 3, "{lines:#?}");

    assert_eq!(
        lines[0]["vendor_turn_id"],
        serde_json::Value::Null,
        "the flushed session start inherited a correlator it never had: {:#?}",
        lines[0]
    );
    assert_eq!(lines[1]["vendor_turn_id"], "TURN-1");
    assert_eq!(
        lines[1]["author"], "schedule_wakeup",
        "the author did not cross the socket: {:#?}",
        lines[1]
    );
    assert_eq!(
        lines[2]["vendor_agent_id"], "AGENT-7",
        "the subagent id did not cross the socket, so the log interleaves two agents: {:#?}",
        lines[2]
    );
    assert_eq!(lines[2]["vendor_turn_id"], "TURN-1");

    daemon.stop();
}

#[test]
fn the_log_survives_a_daemon_restart_and_the_conversation_continues() {
    let home = tempfile::tempdir().expect("tempdir");

    let daemon = start_daemon(home.path());
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"before"}"#,
        &[],
    );
    daemon.stop();

    let daemon = start_daemon(home.path());
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"after"}"#,
        &[],
    );

    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 2, "the pre-restart turn must still be there");
    assert_eq!(lines[0]["seq"], 0);
    assert_eq!(
        lines[1]["seq"], 1,
        "the sequence must continue past the restart"
    );
    assert_eq!(
        lines[0]["bus_id"], lines[1]["bus_id"],
        "the conversation must survive the restart, not fork into two: {lines:#?}"
    );

    daemon.stop();
}

#[test]
fn the_hook_never_fails_the_session_when_the_daemon_is_down() {
    // A non-zero exit from a `UserPromptSubmit` hook blocks the user's prompt.
    // Losing a log line is acceptable; losing the turn is not.
    let home = tempfile::tempdir().expect("tempdir");

    let (code, stderr) = run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"hello"}"#,
        &[],
    );

    assert_eq!(code, 0, "the hook must not fail the turn");
    assert!(
        !stderr.is_empty(),
        "a dropped record must still say so on stderr rather than vanish"
    );
}

#[test]
fn an_event_the_daemon_cannot_place_is_reported_rather_than_dropped_in_silence() {
    // A tool call or a stop arriving for a session the daemon has no binding
    // for — the normal shape of a daemon restart mid-turn — belongs to no
    // conversation and cannot start one. It is not written, which is correct;
    // vanishing without a word is not.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let (code, stderr) = run_hook(
        home.path(),
        r#"{"session_id":"s-orphan","hook_event_name":"Stop","stop_hook_active":false,
            "prompt_id":"T-1","last_assistant_message":"work nobody will see"}"#,
        &[],
    );

    assert_eq!(code, 0, "the hook must not fail the turn");
    assert!(
        log_lines(home.path()).is_empty(),
        "an unplaceable event must not be written"
    );
    assert!(
        !stderr.is_empty(),
        "an unplaceable event must say so; silence is how a whole turn disappears"
    );

    daemon.stop();
}

#[test]
fn the_hook_excludes_our_own_background_sessions() {
    // Global hooks fire for the summariser sessions we spawn ourselves. Without
    // the marker the log mirrors its own noise.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let (code, _) = run_hook(
        home.path(),
        r#"{"session_id":"s-internal","hook_event_name":"UserPromptSubmit","prompt":"summarise"}"#,
        &[("CONBUS_INTERNAL_SESSION", "1")],
    );

    assert_eq!(code, 0);
    assert!(
        log_lines(home.path()).is_empty(),
        "our own background session must not be mirrored"
    );

    daemon.stop();
}

#[test]
fn a_malformed_payload_is_reported_without_failing_the_session() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let (code, stderr) = run_hook(home.path(), "not json at all", &[]);

    assert_eq!(code, 0, "the hook must not fail the turn");
    assert!(!stderr.is_empty(), "the failure must be visible on stderr");
    assert!(log_lines(home.path()).is_empty());

    daemon.stop();
}
