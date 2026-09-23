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
    start_daemon_with(home, &[])
}

/// A daemon with extra environment. Setting it on the child is safe in a way
/// that setting it on the test process is not: `cargo test` runs tests in
/// threads of one process, and a `set_var` there is unsound while any other
/// thread reads the environment.
fn start_daemon_with(home: &Path, env: &[(&str, &std::ffi::OsStr)]) -> Daemon {
    let mut command = Command::new(bin());
    for (key, value) in env {
        command.env(key, value);
    }
    let child = command
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
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() && daemon_stamp_is_settled(home)
        {
            return daemon;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon never started listening on {}", socket.display());
}

/// The first stamp lands at bind. Hardening rewrites it after restore.
/// Tests that overwrite the stamp must wait until that rewrite is done.
fn daemon_stamp_is_settled(home: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(home.join("daemon.build")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
        return false;
    };
    value.get("memory_locked").is_some()
}

/// Run `conbus hook` with a payload on stdin. Returns (exit code, stderr).
fn run_hook(home: &Path, payload: &str, extra_env: &[(&str, &str)]) -> (i32, String) {
    let (code, _stdout, stderr) = run_hook_full(home, payload, extra_env);
    (code, stderr)
}

/// As `run_hook`, but keeping stdout — which the vendor reads.
fn run_hook_full(home: &Path, payload: &str, extra_env: &[(&str, &str)]) -> (i32, String, String) {
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
        String::from_utf8_lossy(&output.stdout).into_owned(),
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
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"ship the recorder"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"PostToolUse","cwd":"/repo","tool_name":"Read",
            "tool_input":{"file_path":"/etc/hosts"},"tool_response":{"ok":true}}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"Stop","cwd":"/repo","stop_hook_active":false}"#,
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
        lines
            .iter()
            .all(|l| l["v"] == conbus_core::record::SCHEMA_VERSION),
        "every record carries a schema version"
    );
    assert!(
        lines.iter().all(|l| l["harness"] == "claude_code"),
        "every record carries a vendor stamp"
    );
    assert!(
        lines.iter().all(|l| l["workspace"] == "/repo"),
        "every record carries the workspace it happened in: {lines:?}"
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
            "prompt_id":"TURN-1","source":"sdk"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"PostToolUse","tool_name":"Read",
            "tool_input":{},"tool_response":{},"prompt_id":"TURN-1",
            "agent_id":"AGENT-7","agent_type":"code-reviewer",
            "tool_use_id":"toolu_01ABC","duration_ms":37}"#,
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
        lines[1]["author"], "sdk",
        "the author did not cross the socket: {:#?}",
        lines[1]
    );
    assert_eq!(
        lines[2]["vendor_agent_id"], "AGENT-7",
        "the subagent id did not cross the socket, so the log interleaves two agents: {:#?}",
        lines[2]
    );
    assert_eq!(lines[2]["vendor_turn_id"], "TURN-1");
    assert_eq!(
        lines[2]["tool_use_id"], "toolu_01ABC",
        "the tool call's own id did not cross the socket: {:#?}",
        lines[2]
    );
    assert_eq!(lines[2]["duration_ms"], 37, "{:#?}", lines[2]);

    daemon.stop();
}

#[test]
fn parallel_tool_calls_can_be_paired_with_their_results() {
    // Two calls issued together and finishing in the other order. The log's
    // sequence records arrival, so without a join key the results cannot be
    // attributed to their requests — which is what mirroring the request side
    // is for.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"read both","prompt_id":"T-1"}"#,
        &[],
    );
    for id in ["toolu_A", "toolu_B"] {
        run_hook(
            home.path(),
            &format!(
                r#"{{"session_id":"s-1","hook_event_name":"PreToolUse","tool_name":"Read",
                    "tool_input":{{"n":"{id}"}},"tool_use_id":"{id}","prompt_id":"T-1"}}"#
            ),
            &[],
        );
    }
    // B finishes first.
    for id in ["toolu_B", "toolu_A"] {
        run_hook(
            home.path(),
            &format!(
                r#"{{"session_id":"s-1","hook_event_name":"PostToolUse","tool_name":"Read",
                    "tool_input":{{"n":"{id}"}},"tool_response":{{"ok":true}},
                    "tool_use_id":"{id}","duration_ms":9,"prompt_id":"T-1"}}"#
            ),
            &[],
        );
    }

    let lines = log_lines(home.path());
    let kinds: Vec<&str> = lines
        .iter()
        .map(|l| l["event"].as_str().expect("event"))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "turn_started",
            "tool_requested",
            "tool_requested",
            "tool_use",
            "tool_use"
        ],
        "{lines:#?}"
    );

    let issued: Vec<&str> = lines
        .iter()
        .filter(|l| l["event"] == "tool_requested")
        .map(|l| l["tool_use_id"].as_str().expect("id"))
        .collect();
    let finished: Vec<&str> = lines
        .iter()
        .filter(|l| l["event"] == "tool_use")
        .map(|l| l["tool_use_id"].as_str().expect("id"))
        .collect();

    assert_eq!(issued, vec!["toolu_A", "toolu_B"], "issue order");
    assert_eq!(finished, vec!["toolu_B", "toolu_A"], "completion order");
    for id in &issued {
        assert!(
            finished.contains(id),
            "{id} was requested and never resolved"
        );
    }

    daemon.stop();
}

#[test]
fn a_batch_boundary_names_the_calls_it_closed_without_repeating_them() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"read both","prompt_id":"T-1"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"PostToolBatch","prompt_id":"T-1","tool_calls":[
            {"tool_name":"Read","tool_input":{"p":"a"},"tool_use_id":"toolu_A","tool_response":{"ok":true}},
            {"tool_name":"Read","tool_input":{"p":"b"},"tool_use_id":"toolu_B","tool_response":{"ok":true}}]}"#,
        &[],
    );

    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 2, "{lines:#?}");

    let batch = &lines[1];
    assert_eq!(batch["event"], "tool_batch_ended");
    assert_eq!(
        batch["tool_calls"],
        serde_json::json!(["toolu_A", "toolu_B"])
    );
    // The bodies belong to the per-call records; repeating them here would put
    // each on disk a third time.
    let line = serde_json::to_string(batch).expect("serialise");
    assert!(!line.contains("tool_input"), "{line}");
    assert!(!line.contains("tool_response"), "{line}");
    assert!(!line.contains("\"p\""), "{line}");

    daemon.stop();
}

#[test]
fn a_refused_tool_leaves_a_reason_rather_than_an_unanswered_request() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"clean up","prompt_id":"T-1"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"PreToolUse","tool_name":"Bash",
            "tool_input":{"command":"rm -rf /"},"tool_use_id":"toolu_01ABC","prompt_id":"T-1"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"PermissionDenied","tool_name":"Bash",
            "tool_input":{"command":"rm -rf /"},"tool_use_id":"toolu_01ABC",
            "reason":"User rejected the command","prompt_id":"T-1"}"#,
        &[],
    );

    let lines = log_lines(home.path());
    let kinds: Vec<&str> = lines
        .iter()
        .map(|l| l["event"].as_str().expect("event"))
        .collect();
    assert_eq!(
        kinds,
        vec!["turn_started", "tool_requested", "tool_denied"],
        "{lines:#?}"
    );

    let denial = &lines[2];
    assert_eq!(
        denial["tool_use_id"], lines[1]["tool_use_id"],
        "the denial must answer the request"
    );
    assert_eq!(denial["reason"], "User rejected the command", "{denial:#?}");
    assert_eq!(
        denial["input"]["kind"], "elided",
        "the refused input reached disk: {denial:#?}"
    );

    daemon.stop();
}

#[test]
fn a_tool_call_that_fails_reaches_the_log_too() {
    // Without this the log's tool history is the *successful* tool history:
    // the vendor sends PostToolUseFailure instead of PostToolUse, so a failed
    // call left no record at all.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"run it","prompt_id":"T-1"}"#,
        &[],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-1","hook_event_name":"PostToolUseFailure","tool_name":"Bash",
            "tool_input":{"command":"false"},"tool_use_id":"toolu_01ABC","prompt_id":"T-1",
            "error":"exit status 1","is_interrupt":false,"duration_ms":12}"#,
        &[],
    );

    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 2, "the failed call left no record: {lines:#?}");

    let failure = &lines[1];
    assert_eq!(failure["event"], "tool_use");
    assert_eq!(failure["outcome"], "failed", "{failure:#?}");
    assert_eq!(failure["tool"], "Bash");
    assert_eq!(failure["tool_use_id"], "toolu_01ABC");
    assert_eq!(failure["duration_ms"], 12);
    assert_eq!(failure["vendor_turn_id"], "T-1");
    // Default retention: the error text is a tool body and stays off disk.
    assert_eq!(failure["error"]["kind"], "elided", "{failure:#?}");
    assert!(
        failure.get("output").is_none(),
        "a failure has no response: {failure:#?}"
    );

    daemon.stop();
}

#[test]
fn a_codex_session_is_read_by_the_codex_adapter_not_merely_labelled_codex() {
    // `--harness codex` used to stamp the label and still parse with the Claude
    // adapter. Codex's correlator is `turn_id`, not `prompt_id`, so every Codex
    // record came out with no correlator at all and every test stayed green —
    // the `failure_reason` bug a second time, one layer up.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let codex = |json: &str| {
        let mut command = Command::new(bin());
        command
            .arg("hook")
            .arg("--harness")
            .arg("codex")
            .env("CONBUS_HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn hook");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(json.as_bytes())
            .expect("write");
        let out = child.wait_with_output().expect("hook exit");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    codex(
        r#"{"session_id":"cx-1","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "model":"gpt-5-codex","permission_mode":"default","prompt":"port the adapter",
            "transcript_path":null,"turn_id":"CX-TURN-1"}"#,
    );
    codex(
        r#"{"session_id":"cx-1","hook_event_name":"PostToolUse","cwd":"/repo","model":"gpt-5-codex",
            "permission_mode":"default","tool_name":"shell","tool_input":{"cmd":"ls"},
            "tool_response":{"ok":true},"tool_use_id":"call_1","transcript_path":null,
            "turn_id":"CX-TURN-1"}"#,
    );

    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines.iter().all(|l| l["harness"] == "codex"), "{lines:#?}");
    assert_eq!(
        lines[0]["vendor_turn_id"], "CX-TURN-1",
        "the Codex correlator was dropped, so the adapter did not read this payload: {:#?}",
        lines[0]
    );
    assert_eq!(lines[1]["event"], "tool_use");
    assert_eq!(lines[1]["tool"], "shell");
    assert_eq!(lines[1]["tool_use_id"], "call_1");
    assert_eq!(lines[1]["vendor_turn_id"], "CX-TURN-1");

    daemon.stop();
}

/// `conbus install --apply` into a throwaway HOME, as a person runs it. Never
/// the runner's or the developer's launchd.
fn install_into(home: &Path) {
    let out = Command::new(bin())
        .args(["install", "--apply"])
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env_remove("CONBUS_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("CODEX_HOME")
        .output()
        .expect("run install");
    assert!(
        out.status.success(),
        "install failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Every command a Claude-shaped hooks file registers for one event.
fn commands_for(settings: &Path, event: &str) -> Vec<String> {
    let text = std::fs::read_to_string(settings).unwrap_or_default();
    let document: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    document["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|block| block["hooks"].as_array().into_iter().flatten())
        .filter_map(|hook| hook["command"].as_str().map(str::to_string))
        .collect()
}

/// Run a registered command the way a vendor does — through a shell, with the
/// payload on stdin — and insist it exited zero.
fn run_registered(command: &str, home: &Path, conbus_home: &Path, payload: &str) {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .env("HOME", home)
        .env("CONBUS_HOME", conbus_home)
        .env_remove("CONBUS_HARNESS")
        .env_remove("CONBUS_INTERNAL_SESSION")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn registered hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("hook exit");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{command}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One Grok turn, an event per registration Grok's file carries, spelled the
/// way `hook::grok` reads them: Claude's `hook_event_name` beside Grok's own
/// camelCase names.
const GROK_TURN: [(&str, &str); 5] = [
    (
        "SessionStart",
        r#"{"hookEventName":"session_start","hook_event_name":"SessionStart",
            "sessionId":"grok-1","source":"startup","cwd":"/repo","workspaceRoot":"/repo"}"#,
    ),
    (
        "UserPromptSubmit",
        r#"{"hookEventName":"user_prompt_submit","hook_event_name":"UserPromptSubmit",
            "sessionId":"grok-1","promptId":"p-1","prompt":"count the twins",
            "cwd":"/repo","workspaceRoot":"/repo"}"#,
    ),
    (
        "PreToolUse",
        r#"{"hookEventName":"pre_tool_use","hook_event_name":"PreToolUse",
            "sessionId":"grok-1","promptId":"p-1","toolName":"Bash",
            "toolInput":{"command":"ls"},"toolUseId":"call_1",
            "cwd":"/repo","workspaceRoot":"/repo"}"#,
    ),
    (
        "PostToolUse",
        r#"{"hookEventName":"post_tool_use","hook_event_name":"PostToolUse",
            "sessionId":"grok-1","promptId":"p-1","toolName":"Bash",
            "toolInput":{"command":"ls"},"toolUseId":"call_1",
            "cwd":"/repo","workspaceRoot":"/repo"}"#,
    ),
    (
        "Stop",
        r#"{"hookEventName":"stop","hook_event_name":"Stop",
            "sessionId":"grok-1","promptId":"p-1","lastAssistantMessage":"one of each",
            "cwd":"/repo","workspaceRoot":"/repo"}"#,
    ),
];

const ONE_GROK_TURN: [&str; 5] = [
    "session_candidate",
    "turn_started",
    "tool_requested",
    "tool_use",
    "turn_ended",
];

#[test]
fn a_grok_event_reaches_the_log_once_though_grok_runs_both_registrations() {
    // Grok reads Claude Code's settings file as well as its own hooks
    // directory, and runs every hook it finds in either. From 2026-09-06 —
    // the day `install` began writing `~/.grok/hooks/conbus.json` — every
    // Grok event reached the live log twice: one conversation, the same ids
    // and body, 30–330 ms apart, consecutive seqs, and both copies stamped
    // grok, because the hook names the harness from the payload rather than
    // from the registration that ran it.
    let user = tempfile::tempdir().expect("home");
    for vendor in [".claude", ".grok"] {
        std::fs::create_dir_all(user.path().join(vendor)).expect("vendor");
    }
    install_into(user.path());
    let registrations = [
        user.path().join(".grok/hooks/conbus.json"),
        user.path().join(".claude/settings.json"),
    ];
    let conbus = tempfile::tempdir().expect("conbus home");
    let daemon = start_daemon(conbus.path());

    for (event, payload) in GROK_TURN {
        let commands: Vec<String> = registrations
            .iter()
            .flat_map(|file| commands_for(file, event))
            .collect();
        assert_eq!(
            commands.len(),
            2,
            "Grok finds the recorder in both files for {event}: {commands:?}"
        );
        for command in &commands {
            run_registered(command, user.path(), conbus.path(), payload);
        }
    }

    let lines = log_lines(conbus.path());
    let kinds: Vec<&str> = lines
        .iter()
        .map(|l| l["event"].as_str().expect("event"))
        .collect();
    assert_eq!(kinds, ONE_GROK_TURN, "{lines:#?}");
    assert!(lines.iter().all(|l| l["harness"] == "grok"), "{lines:#?}");

    daemon.stop();
}

#[test]
fn a_grok_event_that_only_claude_codes_registration_carries_is_still_recorded() {
    // Grok installed after conbus: `install` found no `~/.grok` and wrote
    // nothing there, so Claude Code's registration is the only copy Grok
    // runs. Treating that copy as an echo would lose every Grok event.
    let user = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(user.path().join(".claude")).expect("claude");
    install_into(user.path());
    std::fs::create_dir_all(user.path().join(".grok")).expect("grok, installed afterwards");
    let claude = user.path().join(".claude/settings.json");
    let conbus = tempfile::tempdir().expect("conbus home");
    let daemon = start_daemon(conbus.path());

    for (event, payload) in GROK_TURN {
        let commands = commands_for(&claude, event);
        assert_eq!(commands.len(), 1, "{event}: {commands:?}");
        run_registered(&commands[0], user.path(), conbus.path(), payload);
    }

    let lines = log_lines(conbus.path());
    let kinds: Vec<&str> = lines
        .iter()
        .map(|l| l["event"].as_str().expect("event"))
        .collect();
    assert_eq!(kinds, ONE_GROK_TURN, "{lines:#?}");
    assert!(lines.iter().all(|l| l["harness"] == "grok"), "{lines:#?}");

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
    // And say *what*: the event and the session, so a ghost session's end can
    // be told from a lost tool call when the drop log is read back.
    let drops = std::fs::read_to_string(home.path().join("drops.jsonl")).expect("drop log");
    let last: serde_json::Value =
        serde_json::from_str(drops.lines().last().expect("a drop")).expect("json");
    assert_eq!(
        last["detail"], "Stop for session s-orphan",
        "the drop must name the event and the session: {last}"
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

#[test]
fn the_hook_always_says_something_well_formed_on_stdout() {
    // Writing nothing is not the same as saying nothing, and the difference
    // took a session down. Installed for real, a hook that exited zero with
    // zero-byte stdout produced `400 cache_control cannot be set for empty text
    // blocks` and broke the conversation it was attached to — the one thing
    // this client exists to never do.
    //
    // Every path, including the ones that fail, now answers with the vendor's
    // documented JSON control shape. No path may return an empty stdout.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    /// What was tried, the payload, and any extra environment.
    type Path<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

    let paths: [Path; 6] = [
        (
            "a recorded prompt",
            r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"hi","prompt_id":"T"}"#,
            &[],
        ),
        (
            "a deferred session start",
            r#"{"session_id":"s-2","hook_event_name":"SessionStart","source":"startup"}"#,
            &[],
        ),
        (
            "an event we decline",
            r#"{"session_id":"s-1","hook_event_name":"PreCompact","trigger":"auto"}"#,
            &[],
        ),
        (
            "an event we have never seen",
            r#"{"session_id":"s-1","hook_event_name":"SomethingNew"}"#,
            &[],
        ),
        ("a payload we cannot read", "not json at all", &[]),
        (
            "one of our own sessions",
            r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"x","prompt_id":"T"}"#,
            &[("CONBUS_INTERNAL_SESSION", "1")],
        ),
    ];

    for (what, payload, env) in paths {
        let (code, stdout, _) = run_hook_full(home.path(), payload, env);

        assert_eq!(code, 0, "{what}: the hook must never fail the turn");
        assert!(
            !stdout.trim().is_empty(),
            "{what}: empty stdout is what broke a real session"
        );
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("{what}: stdout is not json ({e}): {stdout:?}"));
        assert_eq!(parsed["continue"], true, "{what}: {stdout}");
        // Nothing is injected: the recorder records, it does not speak.
        assert!(
            parsed.get("hookSpecificOutput").is_none(),
            "{what}: the recorder must not inject context: {stdout}"
        );
    }

    daemon.stop();
}

#[test]
fn an_inference_hook_frame_attaches_claude_chat_and_allows() {
    // Official prompt frame from the Inference hooks endpoint docs.
    // Desktop Chat still disableAllHooks; this is the live UUID+turns channel.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());
    let frame = include_str!("fixtures/claude_inference_hook_prompt.json");
    let (code, stdout, stderr) = run_hook_full(home.path(), frame, &[]);
    assert_eq!(
        code, 0,
        "inference hook must not fail the Chat turn: {stderr}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("not json ({e}): {stdout:?}"));
    assert_eq!(parsed["action"], "allow", "{stdout}");
    assert!(parsed.get("continue").is_none(), "{stdout}");

    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["harness"], "claude_chat");
    assert_eq!(
        lines[0]["vendor_session_id"],
        "22222222-2222-2222-2222-222222222222"
    );
    assert_eq!(lines[0]["entrypoint"], "claude_ai_chat");
    assert_eq!(lines[0]["event"], "turn_started");
    assert!(!lines[0].to_string().contains("alice@example.com"));
    daemon.stop();
}

#[test]
fn an_inference_listen_post_attaches_claude_chat() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    drop(listener);
    let mut child = Command::new(bin())
        .args(["hook", "--inference-listen", &addr])
        .env("CONBUS_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("listen");
    let frame = include_str!("fixtures/claude_inference_hook_prompt.json");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut reply = String::new();
    while Instant::now() < deadline {
        if let Ok(mut stream) = std::net::TcpStream::connect(&addr) {
            let req = format!(
                "POST / HTTP/1.1\r\nHost: conbus\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{frame}",
                frame.len()
            );
            if stream.write_all(req.as_bytes()).is_ok() {
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let mut buf = String::new();
                if std::io::Read::read_to_string(&mut stream, &mut buf).is_ok()
                    && buf.contains("allow")
                {
                    reply = buf;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(reply.contains("\"action\":\"allow\""), "{reply}");
    let lines = log_lines(home.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["harness"], "claude_chat");
    assert_eq!(
        lines[0]["vendor_session_id"],
        "22222222-2222-2222-2222-222222222222"
    );
    daemon.stop();
}

#[test]
fn concurrent_hooks_do_not_shred_the_drop_log() {
    // Every hook is its own process, and the vendor's own contract says tool
    // hooks "may run concurrently for parallel tool calls", so simultaneous
    // appends are the normal case. Formatting straight into an unbuffered file
    // issues a syscall per fragment and interleaves at JSON-token granularity —
    // the durable record destroyed by exactly the fault it exists to record.
    //
    // They have to be released together to prove anything: spawned one at a
    // time they simply finish one at a time, and a shredding implementation
    // passes. So every child is started and parked on a read, and only then are
    // all the pipes closed.
    let home = tempfile::tempdir().expect("tempdir");
    // Deliberately no daemon: every one of these drops.
    let payload = r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"x"}"#;
    const HOOKS: usize = 32;

    let mut children: Vec<Child> = Vec::with_capacity(HOOKS);
    let mut pipes: Vec<std::process::ChildStdin> = Vec::with_capacity(HOOKS);
    for _ in 0..HOOKS {
        let mut child = Command::new(bin())
            .arg("hook")
            .env("CONBUS_HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn hook");
        let mut pipe = child.stdin.take().expect("stdin");
        // Parked here: the hook blocks reading stdin until the pipe closes.
        pipe.write_all(payload.as_bytes()).expect("write");
        children.push(child);
        pipes.push(pipe);
    }
    // Released together.
    drop(pipes);
    for mut child in children {
        child.wait().expect("hook exit");
    }

    let text = std::fs::read_to_string(home.path().join("drops.jsonl")).expect("drops file");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), HOOKS, "lines were lost or merged");
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("shredded line ({e}): {line:?}"));
    }
}

#[test]
fn a_drop_is_recorded_where_doctor_can_find_it() {
    let home = tempfile::tempdir().expect("tempdir");

    let (code, _stdout, _stderr) = run_hook_full(
        home.path(),
        r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"x"}"#,
        &[],
    );
    assert_eq!(code, 0);

    let doctor = Command::new(bin())
        .arg("doctor")
        .env("CONBUS_HOME", home.path())
        .output()
        .expect("doctor");
    let out = String::from_utf8_lossy(&doctor.stdout);
    assert!(out.contains("drops     1 recorded"), "{out}");
    assert!(out.contains("no daemon"), "{out}");
}

#[test]
fn doctor_reports_a_log_it_cannot_append_to() {
    // The failure the check exists for: a daemon that answers and cannot write
    // looks identical to a healthy one. Probing the directory instead of the
    // segment misses it, because POSIX checks different rights for the two.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());
    run_hook(
        home.path(),
        r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"x","prompt_id":"T"}"#,
        &[],
    );
    daemon.stop();

    let segment = home.path().join("log").join("segment-00000000.jsonl");
    let mut readonly = std::fs::metadata(&segment).expect("meta").permissions();
    readonly.set_readonly(true);
    std::fs::set_permissions(&segment, readonly).expect("chmod");

    let doctor = Command::new(bin())
        .arg("doctor")
        .env("CONBUS_HOME", home.path())
        .output()
        .expect("doctor");
    let out = String::from_utf8_lossy(&doctor.stdout);

    assert!(
        out.contains("NOT APPENDABLE"),
        "a read-only segment read as healthy: {out}"
    );
}

/// A whole conversation, so the next session in that checkout has something to
/// be told about.
fn conversation(home: &Path, session: &str, cwd: &str, prompts: &[&str]) {
    run_hook(
        home,
        &format!(
            r#"{{"session_id":"{session}","hook_event_name":"SessionStart",
                "source":"startup","cwd":"{cwd}"}}"#
        ),
        ATTENDED,
    );
    for prompt in prompts {
        run_hook(
            home,
            &format!(
                r#"{{"session_id":"{session}","hook_event_name":"UserPromptSubmit",
                    "cwd":"{cwd}","prompt":"{prompt}"}}"#
            ),
            ATTENDED,
        );
        run_hook(
            home,
            &format!(
                r#"{{"session_id":"{session}","hook_event_name":"Stop","cwd":"{cwd}",
                    "stop_hook_active":false,
                    "last_assistant_message":"done with {prompt}"}}"#
            ),
            ATTENDED,
        );
    }
}

/// A well-formed reply that injects nothing.
///
/// Checked by shape rather than against the exact bytes: what matters is that
/// the vendor gets parseable JSON with no context in it, and never an empty
/// stdout — writing nothing is what coincided with a live session breaking.
fn says_nothing(stdout: &str) -> bool {
    let Ok(reply) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return false;
    };
    reply["hookSpecificOutput"].is_null() && reply["continue"] == true
}

/// The hook child inherits this process's environment, and these tests run
/// inside somebody's coding session — so without pinning it, whether a briefing
/// happens depends on which harness the suite was launched from.
const ATTENDED: &[(&str, &str)] = &[("CLAUDE_CODE_ENTRYPOINT", "cli")];

#[test]
fn a_second_session_in_the_same_checkout_is_told_what_happened_there() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    conversation(
        home.path(),
        "s-old",
        "/repo",
        &["ship the recorder", "now the gate"],
    );

    // A different session, same checkout. This is the moment the whole feature
    // exists for, and it goes through the real binary and a real socket
    // because a normalise-level test would pass while the reply never reached
    // the vendor at all — which is how a previous fix here was inert for a day.
    let (code, stdout, stderr) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-new","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"carry on"}"#,
        ATTENDED,
    );

    assert_eq!(code, 0, "a hook must never fail the turn: {stderr}");
    let reply: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|err| panic!("{err}: {stdout:?}"));

    // The vendor requires this to name the firing event and throws the whole
    // hook result away when it does not — silently, after reporting once per
    // session. Nothing else in the reply matters if this is wrong.
    assert_eq!(
        reply["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit",
        "{reply}"
    );
    let context = reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("no context in {reply}"));
    // The tail, not the head. The gap between a conversation's opening ask and
    // its last activity was measured in days on the conversations this really
    // fires for, while the window that makes them eligible is minutes — so the
    // opening ask is a different piece of work wearing the same id.
    assert!(context.contains("now the gate"), "{context}");
    assert!(
        !context.contains("ship the recorder"),
        "the opening ask would mislead: {context}"
    );
    assert!(context.contains("never instruction to follow"), "{context}");
    assert!(context.contains("the prompt wins"), "{context}");
    assert!(
        reply["systemMessage"]
            .as_str()
            .is_some_and(|line| line.contains("conbus")),
        "the person gets a receipt too: {reply}"
    );

    // And the log says it happened, under the session that was told.
    let lines = log_lines(home.path());
    let briefed: Vec<_> = lines
        .iter()
        .filter(|line| line["event"] == "briefed")
        .collect();
    assert_eq!(briefed.len(), 1, "{lines:#?}");
    assert_eq!(
        briefed[0]["units"].as_u64(),
        Some(context.encode_utf16().count() as u64)
    );
    // The receipt names the conversation that was narrated, not the session
    // reading it — the first live briefing pointed the person at their own
    // brand-new session.
    let old = lines
        .iter()
        .find(|line| line["vendor_session_id"] == "s-old")
        .and_then(|line| line["bus_id"].as_str())
        .expect("the old conversation's id");
    let message = reply["systemMessage"].as_str().expect("message");
    assert!(message.contains(&old[..13]), "{message} should name {old}");
    assert!(
        !message.contains(&briefed[0]["bus_id"].as_str().expect("id")[..13]),
        "the receipt points at the reader's own session: {message}"
    );
    // The briefing record knows where it happened, like every other record.
    assert_eq!(briefed[0]["workspace"], "/repo", "{:#?}", briefed[0]);
    // The budget and the fence, on the wire and not only in a unit test.
    assert!(
        context.encode_utf16().count() <= 2_600,
        "over budget: {} units",
        context.encode_utf16().count()
    );
    assert_eq!(
        context.matches("</conbus-record").count(),
        1,
        "exactly one closing fence: {context}"
    );
    daemon.stop();
}

#[test]
fn a_scripted_session_is_left_alone() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    conversation(
        home.path(),
        "s-old",
        "/repo",
        &["ship the recorder", "now the gate"],
    );

    let (code, stdout, _) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-sdk","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"carry on"}"#,
        &[("CLAUDE_CODE_ENTRYPOINT", "sdk-cli")],
    );

    assert_eq!(code, 0);
    assert!(says_nothing(&stdout), "nobody is reading: {stdout}");
    assert!(
        !log_lines(home.path())
            .iter()
            .any(|line| line["event"] == "briefed")
    );
    daemon.stop();
}

#[test]
fn a_first_session_in_an_empty_checkout_is_told_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let (code, stdout, _) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-first","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"hello"}"#,
        ATTENDED,
    );

    assert_eq!(code, 0);
    assert!(says_nothing(&stdout), "{stdout}");
    daemon.stop();
}

#[test]
fn only_the_first_prompt_of_a_session_is_briefed() {
    // The per-prompt cost is paid once at injection and then carried in history
    // for every turn after it, so a briefing on turn two is paid for twice.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    conversation(
        home.path(),
        "s-old",
        "/repo",
        &["ship the recorder", "now the gate"],
    );
    run_hook(
        home.path(),
        r#"{"session_id":"s-new","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"first"}"#,
        ATTENDED,
    );

    let (_, stdout, _) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-new","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"second"}"#,
        ATTENDED,
    );

    assert!(says_nothing(&stdout), "{stdout}");
    assert_eq!(
        log_lines(home.path())
            .iter()
            .filter(|line| line["event"] == "briefed")
            .count(),
        1
    );
    daemon.stop();
}

#[test]
fn a_briefing_survives_a_daemon_restart() {
    // The chapter index is derived state, and derived state that does not come
    // back is how a restarted daemon tells the next session that its checkout
    // has never been worked in.
    let home = tempfile::tempdir().expect("tempdir");
    let first = start_daemon(home.path());
    conversation(
        home.path(),
        "s-old",
        "/repo",
        &["ship the recorder", "now the gate"],
    );
    first.stop();

    let second = start_daemon(home.path());
    let (_, stdout, _) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-new","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"carry on"}"#,
        ATTENDED,
    );

    let reply: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json");
    assert!(
        reply["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .is_some_and(|text| text.contains("now the gate")),
        "{reply}"
    );
    second.stop();
}

#[test]
fn a_wedged_daemon_does_not_hold_the_turn_open() {
    // The hook must never fail the turn, and it must not delay it either. A
    // daemon that accepts the connection and then answers nothing is the case
    // no other test covers: the socket is up, so the connect succeeds, and
    // without a deadline the read blocks until the vendor's own timeout — 30
    // seconds on Claude Code's UserPromptSubmit, with the prompt stalled the
    // whole time, and one second on Codex's SessionEnd.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path()).expect("mkdir");
    let socket = home.path().join("conbus.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    // Accept and then say nothing, holding the connection open.
    let held = std::thread::spawn(move || {
        let mut kept = Vec::new();
        while let Ok((stream, _)) = listener.accept() {
            kept.push(stream);
        }
    });

    let started = Instant::now();
    let (code, stdout, _) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-wedged","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"hello"}"#,
        ATTENDED,
    );
    let waited = started.elapsed();

    assert_eq!(code, 0, "a hook must never fail the turn");
    assert!(says_nothing(&stdout), "{stdout}");
    assert!(
        waited < Duration::from_secs(1),
        "held the turn open for {waited:?}, past Codex's own SessionEnd limit"
    );
    drop(held);
}

#[test]
fn a_silent_connection_does_not_hold_the_hooks_behind_it() {
    // The daemon serves one connection to its end before it accepts the next.
    // So a client that connects and then says nothing is not its own problem
    // but everybody else's: every hook behind it sits in the backlog, and the
    // hook client gives up after half a second — the turn goes on, and its
    // record is gone. The daemon has to walk away from silence by itself.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let silent =
        std::os::unix::net::UnixStream::connect(home.path().join("conbus.sock")).expect("connect");
    // Long enough for the daemon to accept it and block on the read.
    std::thread::sleep(Duration::from_millis(50));

    let started = Instant::now();
    let (code, stdout, stderr) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-behind","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"still here"}"#,
        ATTENDED,
    );
    let waited = started.elapsed();
    drop(silent);

    assert_eq!(code, 0, "a hook must never fail the turn");
    assert!(says_nothing(&stdout), "{stdout}");
    assert!(waited < Duration::from_secs(1), "waited {waited:?}");
    let prompts = log_lines(home.path())
        .into_iter()
        .filter(|line| line["event"] == "turn_started")
        .count();
    assert_eq!(
        prompts, 1,
        "the hook behind a silent connection lost its record after {waited:?}: {stderr}"
    );
    daemon.stop();
}

#[test]
fn a_burst_of_hooks_does_not_lose_records_to_a_full_backlog() {
    // Cursor fires tool hooks in parallel into one accept loop. A short
    // listen backlog RSTs the overflow; those hooks see Broken pipe.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());
    run_hook(
        home.path(),
        r#"{"session_id":"s-burst","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"many tools"}"#,
        ATTENDED,
    );

    const HOOKS: usize = 32;
    let mut children: Vec<Child> = Vec::with_capacity(HOOKS);
    let mut pipes: Vec<std::process::ChildStdin> = Vec::with_capacity(HOOKS);
    for i in 0..HOOKS {
        let payload = format!(
            r#"{{"session_id":"s-burst","hook_event_name":"PreToolUse","cwd":"/repo",
                "tool_name":"Read","tool_input":{{"n":"{i}"}},"tool_use_id":"burst_{i}"}}"#
        );
        let mut child = Command::new(bin())
            .arg("hook")
            .env("CONBUS_HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn hook");
        let mut pipe = child.stdin.take().expect("stdin");
        pipe.write_all(payload.as_bytes()).expect("write");
        children.push(child);
        pipes.push(pipe);
    }
    drop(pipes);
    for child in &mut children {
        let status = child.wait().expect("hook exit");
        assert!(status.success(), "hook failed the turn: {status}");
    }

    let requested = log_lines(home.path())
        .into_iter()
        .filter(|line| line["event"] == "tool_requested")
        .count();
    assert_eq!(
        requested,
        HOOKS,
        "a parallel burst lost records; drops={}",
        std::fs::read_to_string(home.path().join("drops.jsonl")).unwrap_or_default()
    );
    daemon.stop();
}

#[test]
fn a_codex_hook_says_only_what_codex_accepts() {
    // Codex 0.152.1, first session with trusted hooks: "PreToolUse hook
    // (failed) — error: PreToolUse hook returned unsupported suppressOutput",
    // and again on PostToolUse, on every tool call. Its reference says which
    // fields fail which event; the empty object is the one reply all of them
    // take. The record still landed — this is about what the person saw.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let (code, stdout, stderr) = run_hook_full(
        home.path(),
        r#"{"session_id":"cx-9","hook_event_name":"PreToolUse","cwd":"/repo","model":"gpt-5.6-sol",
            "permission_mode":"bypassPermissions","tool_name":"Bash","tool_input":{"command":"ls"},
            "tool_use_id":"exec-1","transcript_path":null,"turn_id":"CX-TURN-9"}"#,
        &[("CONBUS_HARNESS", "codex")],
    );

    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout.trim(),
        "{}",
        "Codex fails a PreToolUse hook that says more"
    );

    daemon.stop();
}

#[test]
fn the_daemon_leaves_its_build_where_doctor_can_compare_it() {
    // Deploying is three steps — merge, rename the binary, restart the daemon
    // — and between the last two `doctor` runs new code while the daemon runs
    // old code. Nothing said so on a real machine. Now the daemon writes its
    // build beside its socket, the binary can name its own, and doctor puts
    // the two on one line.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());

    let version = Command::new(bin())
        .arg("--version")
        .output()
        .expect("run --version");
    let version = String::from_utf8_lossy(&version.stdout).trim().to_string();
    // `conbus 0.1.0 (a1b2c3d4e)`: the version, then the commit doctor compares.
    let build = version
        .strip_prefix("conbus ")
        .and_then(|rest| rest.split_once(" ("))
        .and_then(|(_, build)| build.strip_suffix(')'))
        .expect("`conbus <version> (<build>)`")
        .to_string();
    assert!(!build.is_empty() && build != "unknown", "{version}");

    let stamp = std::fs::read_to_string(home.path().join("daemon.build")).expect("daemon.build");
    let stamp: serde_json::Value = serde_json::from_str(stamp.trim()).expect("json");
    assert_eq!(stamp["build"], build, "{stamp}");
    assert_eq!(stamp["pid"], daemon.0.id(), "{stamp}");

    let doctor = Command::new(bin())
        .arg("doctor")
        .env("CONBUS_HOME", home.path())
        .output()
        .expect("run doctor");
    let doctor = String::from_utf8_lossy(&doctor.stdout).into_owned();
    assert!(
        doctor
            .lines()
            .any(|line| line.starts_with("build ") && line.contains(&build)),
        "doctor must name its own build:\n{doctor}"
    );
    assert!(
        doctor.lines().any(|line| line.starts_with("daemon ")
            && line.contains(&build)
            && !line.contains("DIFFERS")),
        "doctor must say the daemon runs the same build:\n{doctor}"
    );

    daemon.stop();
}

#[test]
fn doctor_says_when_the_daemon_runs_a_different_build() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());
    // What a rename-without-restart leaves behind: a stamp from another build,
    // written by a daemon that is still alive.
    std::fs::write(
        home.path().join("daemon.build"),
        format!(
            r#"{{"build":"0000000-older","pid":{},"started":"2026-09-02T08:14:53.000Z"}}"#,
            daemon.0.id()
        ),
    )
    .expect("write stamp");

    let doctor = Command::new(bin())
        .arg("doctor")
        .env("CONBUS_HOME", home.path())
        .output()
        .expect("run doctor");
    let doctor = String::from_utf8_lossy(&doctor.stdout).into_owned();

    assert!(
        doctor
            .lines()
            .any(|line| line.starts_with("daemon ") && line.contains("DIFFERS")),
        "doctor must flag a daemon on another build:\n{doctor}"
    );

    daemon.stop();
}

#[test]
fn a_session_the_person_just_cleared_is_not_briefed() {
    // `/clear` says "forget it". Opening the next prompt by handing back what
    // was just dismissed is the opposite of what was asked, and the vendor
    // says exactly when it happened: `SessionStart.source == "clear"`.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());
    conversation(
        home.path(),
        "s-old",
        "/repo",
        &["ship the recorder", "now the gate"],
    );

    run_hook(
        home.path(),
        r#"{"session_id":"s-cleared","hook_event_name":"SessionStart","source":"clear","cwd":"/repo"}"#,
        ATTENDED,
    );
    let (code, stdout, _) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-cleared","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"start over"}"#,
        ATTENDED,
    );

    assert_eq!(code, 0);
    assert!(
        says_nothing(&stdout),
        "the person asked to forget: {stdout}"
    );
    let lines = log_lines(home.path());
    assert!(!lines.iter().any(|line| line["event"] == "briefed"));
    assert!(
        lines
            .iter()
            .any(|line| line["vendor_session_id"] == "s-cleared" && line["event"] == "turn_started"),
        "the cleared session is still recorded: {lines:#?}"
    );
    daemon.stop();
}

#[test]
fn a_person_can_switch_the_briefing_off() {
    // The one lever toward silence the first audit found missing. Read by the
    // hook from its own environment, carried on the wire, honoured by the
    // daemon: nothing is injected, and nothing claims to have been.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_daemon(home.path());
    conversation(
        home.path(),
        "s-old",
        "/repo",
        &["ship the recorder", "now the gate"],
    );

    let (code, stdout, _) = run_hook_full(
        home.path(),
        r#"{"session_id":"s-quiet","hook_event_name":"UserPromptSubmit","cwd":"/repo",
            "prompt":"carry on"}"#,
        &[
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("CONBUS_BRIEFING", "off"),
        ],
    );

    assert_eq!(code, 0);
    assert!(says_nothing(&stdout), "switched off: {stdout}");
    let lines = log_lines(home.path());
    assert!(!lines.iter().any(|line| line["event"] == "briefed"));
    assert!(
        lines
            .iter()
            .any(|line| line["vendor_session_id"] == "s-quiet" && line["event"] == "turn_started"),
        "off means no briefing, not no recording: {lines:#?}"
    );
    daemon.stop();
}

/// A stub vendor CLI that takes its time.
fn slow_stub(dir: &Path, name: &str, seconds: u32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nsleep {seconds}\n")).expect("stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[test]
fn a_landing_that_blocks_does_not_hold_the_next_hook() {
    // On 2026-09-08 sixty hooks in five minutes gave up at their 500ms
    // deadline. The daemon serves one connection to its end before it accepts
    // the next, and `landing::enter` was the last pending action still run on
    // that loop — it waits for `cursor -n` to exit. A chat order that is only
    // an order ("work in cursor from here", while already in Cursor) is what
    // queues one.
    let home = tempfile::tempdir().expect("tempdir");
    let cursor = slow_stub(home.path(), "cursor", 3);
    let workspace = home.path().join("repo");
    std::fs::create_dir(&workspace).expect("repo");
    let daemon = start_daemon_with(
        home.path(),
        &[("CONBUS_CURSOR_APP_BIN", cursor.as_os_str())],
    );

    let order = format!(
        r#"{{"cursor_version":"3.19.13","hook_event_name":"beforeSubmitPrompt",
            "conversation_id":"conv-landing","generation_id":"g1",
            "workspace_roots":["{}"],"prompt":"이제 커서로 작업할거야"}}"#,
        workspace.display()
    );
    let (code, stderr) = run_hook(home.path(), &order, &[]);
    assert_eq!(code, 0, "the order hook failed: {stderr}");

    // The landing is now in flight. The next hook must not wait behind it.
    let began = Instant::now();
    let (code, stderr) = run_hook(
        home.path(),
        r#"{"cursor_version":"3.19.13","hook_event_name":"preToolUse",
            "conversation_id":"conv-landing","generation_id":"g2",
            "tool_name":"Read","tool_use_id":"t1","tool_input":{}}"#,
        &[],
    );
    let waited = began.elapsed();
    daemon.stop();

    assert_eq!(code, 0, "{stderr}");
    assert!(
        waited < Duration::from_secs(2),
        "the hook waited {waited:?} behind a landing; the daemon accepts one at a time"
    );
    assert!(
        !stderr.contains("record dropped"),
        "the hook behind the landing lost its record: {stderr}"
    );
}

#[test]
fn every_line_of_the_daemon_log_says_when_it_happened() {
    // launchd points both of the daemon's streams at one file and appends
    // across restarts. That file held 691 lines and no times at all, so the
    // outage of 2026-09-08 could not be placed against anything.
    let home = tempfile::tempdir().expect("tempdir");
    let log = home.path().join("daemon.log");
    let child = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .stdout(Stdio::from(std::fs::File::create(&log).expect("create")))
        .stderr(Stdio::from(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&log)
                .expect("append"),
        ))
        .spawn()
        .expect("spawn daemon");
    let daemon = Daemon(child);
    let socket = home.path().join("conbus.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(20));
    }

    // The other line the outage would have needed: a connection that says
    // nothing, which the daemon drops after its silence bound.
    let quiet = std::os::unix::net::UnixStream::connect(&socket).expect("connect");
    std::thread::sleep(Duration::from_millis(400));
    drop(quiet);
    std::thread::sleep(Duration::from_millis(200));
    daemon.stop();

    let text = std::fs::read_to_string(&log).expect("daemon.log");
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert!(lines.len() >= 2, "expected a banner and a drop: {text}");
    for line in &lines {
        let stamp = line.get(..24).unwrap_or_default();
        assert!(
            stamp.parse::<conbus_core::clock::Timestamp>().is_ok(),
            "a daemon.log line with no time on it: {line:?}"
        );
    }
    assert!(
        text.contains("sent no request"),
        "the silence drop should be in the log: {text}"
    );
}
