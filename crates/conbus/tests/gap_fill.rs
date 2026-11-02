//! The start-up gap fill, through the real daemon: what a start may add to an
//! append-only log, and what it must leave out. Root and `HOME` are tempdirs,
//! and Codex is a path that does not exist.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PARENT: &str = "9b24e37f-0000-4000-8000-000000000001";
const CHILD: &str = "437e9522-0000-4000-8000-000000000002";
const T0: &str = "2026-11-01T00:00:00.000Z";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

fn env_for(command: &mut Command, home: &Path) {
    command
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .env("CONBUS_AGY_BIN", "/nonexistent/agy")
        .env_remove("CODEX_HOME")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null());
}

fn daemon_command(home: &Path) -> Command {
    let mut command = Command::new(bin());
    command
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    env_for(&mut command, home);
    command
}

fn stamp(home: &Path) -> Value {
    std::fs::read_to_string(home.join("daemon.build"))
        .ok()
        .and_then(|text| serde_json::from_str(text.trim()).ok())
        .unwrap_or(Value::Null)
}

/// Wait until the stamp says this daemon serves: its start-up fill is done.
fn wait_serving(home: &Path, daemon: Child) -> Child {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let stamped = stamp(home);
        if stamped["pid"].as_u64() == Some(u64::from(daemon.id()))
            && stamped.get("serving").is_some()
        {
            return daemon;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the daemon never served");
}

fn start(home: &Path) -> Child {
    wait_serving(home, daemon_command(home).spawn().expect("daemon"))
}

fn stop(mut daemon: Child) {
    let _ = daemon.kill();
    let _ = daemon.wait();
}

fn hook(home: &Path, payload: &str) {
    let mut command = Command::new(bin());
    command.args(["hook", "--harness", "claude_code"]);
    env_for(&mut command, home);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    child.wait().expect("wait");
}

fn prompt(session: &str, id: &str, text: &str) -> String {
    json!({"session_id": session, "hook_event_name": "UserPromptSubmit", "prompt": text,
           "prompt_id": id, "cwd": "/repo"})
    .to_string()
}

fn ended(session: &str, id: &str, text: &str) -> String {
    json!({"session_id": session, "hook_event_name": "Stop", "last_assistant_message": text,
           "prompt_id": id, "cwd": "/repo"})
    .to_string()
}

fn user(session: &str, id: &str, text: &str) -> String {
    json!({"type": "user", "sessionId": session, "timestamp": T0, "version": "2.1.316",
           "cwd": "/repo", "entrypoint": "cli", "promptId": id,
           "message": {"role": "user", "content": text}})
    .to_string()
}

fn reply(session: &str, text: &str) -> String {
    json!({"type": "assistant", "sessionId": session, "timestamp": T0, "version": "2.1.316",
           "message": {"role": "assistant", "stop_reason": "end_turn",
                       "content": [{"type": "text", "text": text}]}})
    .to_string()
}

fn transcript(home: &Path, session: &str, lines: &[String]) {
    let dir = home.join(".claude/projects/-repo");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join(format!("{session}.jsonl")),
        lines.join("\n") + "\n",
    )
    .expect("write");
}

fn log_lines(home: &Path) -> Vec<Value> {
    let mut segments: Vec<PathBuf> = std::fs::read_dir(home.join("log"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default();
    segments.sort();
    segments
        .iter()
        .flat_map(|segment| {
            String::from_utf8_lossy(&std::fs::read(segment).unwrap_or_default())
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn show(records: &[Value]) -> String {
    records
        .iter()
        .map(|record| {
            format!(
                "seq={} {} sess={} turn={} bf={} text={:?}",
                record["seq"],
                record["event"],
                record["vendor_session_id"]
                    .as_str()
                    .map_or("-", |id| &id[..8.min(id.len())]),
                record["vendor_turn_id"].as_str().unwrap_or("-"),
                record["backfilled_from"].as_str().unwrap_or("-"),
                record["prompt"]["text"]
                    .as_str()
                    .or(record["assistant_message"]["text"].as_str())
                    .unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A parent and the fork it became, both recorded live, with the fork's
/// transcript opening on the parent's lines under the parent's session id: the
/// shape Claude Desktop continuations are written in.
fn a_recorded_fork(home: &Path) {
    let daemon = start(home);
    hook(home, &prompt(PARENT, "pp", "the parent's prompt"));
    hook(home, &ended(PARENT, "pp", "the parent's reply"));
    hook(home, &prompt(CHILD, "cc", "the fork's prompt"));
    hook(home, &ended(CHILD, "cc", "the fork's reply"));
    stop(daemon);
}

#[test]
fn a_start_does_not_copy_a_parent_conversation_into_its_fork() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_recorded_fork(home);
    transcript(
        home,
        CHILD,
        &[
            user(PARENT, "pp", "the parent's prompt"),
            reply(PARENT, "the parent's reply"),
            user(CHILD, "cc", "the fork's prompt"),
            reply(CHILD, "the fork's reply"),
        ],
    );

    stop(start(home));

    let all = log_lines(home);
    let fork = all
        .iter()
        .find(|record| record["vendor_turn_id"] == "cc")
        .expect("the fork's own prompt")["bus_id"]
        .clone();
    let copied: Vec<Value> = all
        .into_iter()
        .filter(|record| record["bus_id"] == fork && record.get("backfilled_from").is_some())
        .collect();
    assert!(copied.is_empty(), "under the fork:\n{}", show(&copied));
}

#[test]
fn a_start_still_fills_what_a_fork_itself_missed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_recorded_fork(home);
    transcript(
        home,
        CHILD,
        &[
            user(PARENT, "pp", "the parent's prompt"),
            reply(PARENT, "the parent's reply"),
            user(CHILD, "cc", "the fork's prompt"),
            reply(CHILD, "the fork's reply"),
            user(CHILD, "c2", "said while the recorder was down"),
            reply(CHILD, "answered while the recorder was down"),
        ],
    );

    stop(start(home));

    let all = log_lines(home);
    let fork = all
        .iter()
        .find(|record| record["vendor_turn_id"] == "cc")
        .expect("the fork's own prompt")["bus_id"]
        .clone();
    let filled: Vec<&Value> = all
        .iter()
        .filter(|record| record.get("backfilled_from").is_some())
        .collect();
    assert_eq!(filled.len(), 2, "the missed prompt and its reply");
    assert!(
        filled
            .iter()
            .all(|record| record["bus_id"] == fork && record["vendor_session_id"] == CHILD)
    );
}

/// A `user` line carrying vendor flags.
fn flagged(session: &str, id: &str, text: &str, flags: Value) -> String {
    let mut line: Value = serde_json::from_str(&user(session, id, text)).expect("json");
    for (key, value) in flags.as_object().expect("flags") {
        line[key] = value.clone();
    }
    line.to_string()
}

#[test]
fn a_start_does_not_record_vendor_bookkeeping_as_turns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(CHILD, "seen", "the prompt the hook saw"));
    stop(daemon);
    transcript(
        home,
        CHILD,
        &[
            user(CHILD, "seen", "the prompt the hook saw"),
            flagged(
                CHILD,
                "m1",
                "Base directory for this skill",
                json!({"isMeta": true}),
            ),
            flagged(
                CHILD,
                "m2",
                "This session is being continued",
                json!({"isCompactSummary": true, "isVisibleInTranscriptOnly": true}),
            ),
            user(CHILD, "m3", "[Request interrupted by user]"),
            flagged(
                CHILD,
                "m4",
                "<local-command-caveat>x</local-command-caveat>",
                json!({"isMeta": true}),
            ),
            user(CHILD, "m4", "<command-name>/model</command-name>"),
            user(
                CHILD,
                "m4",
                "<local-command-stdout>y</local-command-stdout>",
            ),
        ],
    );

    stop(start(home));

    let filled: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| record.get("backfilled_from").is_some())
        .collect();
    assert!(filled.is_empty(), "{}", show(&filled));
}

#[test]
fn a_missed_turn_with_a_skill_body_is_filled_with_its_reply() {
    // Before, the body was a second prompt under the same id: `missing_from`
    // saw the id again and dropped the reply behind it.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(CHILD, "seen", "the prompt the hook saw"));
    stop(daemon);
    transcript(
        home,
        CHILD,
        &[
            user(CHILD, "seen", "the prompt the hook saw"),
            user(CHILD, "missed", "use the skill"),
            flagged(
                CHILD,
                "missed",
                "Base directory for this skill",
                json!({"isMeta": true}),
            ),
            reply(CHILD, "the skill's answer"),
        ],
    );

    stop(start(home));

    let filled: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| record.get("backfilled_from").is_some())
        .collect();
    assert_eq!(filled.len(), 2, "{}", show(&filled));
    assert_eq!(filled[0]["prompt"]["text"], "use the skill");
    assert_eq!(filled[1]["assistant_message"]["text"], "the skill's answer");
}
