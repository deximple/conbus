//! The start-up gap fill, through the real daemon: what a start may add to an
//! append-only log, and what it must leave out. Root and `HOME` are tempdirs,
//! and Codex is a path that does not exist.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

mod support;

const PARENT: &str = "9b24e37f-0000-4000-8000-000000000001";
const CHILD: &str = "437e9522-0000-4000-8000-000000000002";
const S: &str = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";
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
    hook_as(home, "claude_code", payload);
}

fn hook_as(home: &Path, harness: &str, payload: &str) {
    let mut command = Command::new(bin());
    command.args(["hook", "--harness", harness]);
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

/// What a running turn's transcript ends on: text, and the model not stopped.
fn partial(session: &str, text: &str) -> String {
    json!({"type": "assistant", "sessionId": session, "timestamp": T0, "version": "2.1.316",
           "message": {"role": "assistant", "stop_reason": "tool_use",
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

#[test]
fn a_missed_turn_is_filled_even_when_its_words_were_said_before() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "continue"));
    hook(home, &ended(S, "p0", "first reply"));
    stop(daemon);
    transcript(
        home,
        S,
        &[
            user(S, "p0", "continue"),
            reply(S, "first reply"),
            user(S, "p1", "continue"),
            reply(S, "the reply only the transcript holds"),
        ],
    );

    stop(start(home));

    let all = log_lines(home);
    assert!(
        all.iter().any(|record| record["vendor_turn_id"] == "p1"),
        "p1 missing:\n{}",
        show(&all)
    );
    let replies = all
        .iter()
        .filter(|record| record["event"] == "turn_ended")
        .count();
    assert_eq!(replies, 2, "{}", show(&all));
}

/// One vendor session recorded under two conversations, the way a split leaves
/// it.
fn split_session(home: &Path, session: &str, turns: [(&str, &str); 2]) {
    use conbus_core::event::{Body, Event, Harness, Source, VendorTurnId};
    use conbus_core::ids::BusId;
    use conbus_core::record::Entry;
    use conbus_core::retention::Retention;
    use conbus_core::segment::SegmentLog;

    let mut log = SegmentLog::open(home.join("log"), Retention::default()).expect("open");
    for (bus, (id, text)) in [BusId::mint(), BusId::mint()].into_iter().zip(turns) {
        log.append(Entry {
            bus_id: bus,
            observed_at: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some(session.into()),
                workspace: Some("/repo".into()),
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            },
            vendor_turn_id: Some(VendorTurnId::new(id)),
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event: Event::TurnStarted {
                author: None,
                prompt: Body::kept(text),
            },
        })
        .expect("append");
    }
}

#[test]
fn a_split_session_does_not_get_its_older_turns_filled_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    split_session(
        home,
        S,
        [("p0", "under the first id"), ("p1", "under the second id")],
    );
    transcript(
        home,
        S,
        &[
            user(S, "p0", "under the first id"),
            user(S, "p1", "under the second id"),
        ],
    );

    stop(start(home));

    let again: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| record["vendor_turn_id"] == "p0")
        .collect();
    assert_eq!(again.len(), 1, "\n{}", show(&again));
}

#[test]
fn a_prompt_whose_hook_was_lost_is_filled_without_a_second_ending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "seen"));
    // The prompt hook never arrived; the turn's ending did.
    hook(home, &ended(S, "p1", "the reply the hook saw"));
    stop(daemon);
    transcript(
        home,
        S,
        &[
            user(S, "p0", "seen"),
            user(S, "p1", "the prompt only the transcript holds"),
            reply(S, "the reply the hook saw"),
        ],
    );

    stop(start(home));

    let all = log_lines(home);
    let started = all
        .iter()
        .filter(|record| record["event"] == "turn_started" && record["vendor_turn_id"] == "p1")
        .count();
    let endings = all
        .iter()
        .filter(|record| record["event"] == "turn_ended")
        .count();
    assert_eq!((started, endings), (1, 1), "\n{}", show(&all));
}

fn spooled(home: &Path) -> usize {
    std::fs::read_dir(home.join("spool"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|x| x == "json"))
                .count()
        })
        .unwrap_or(0)
}

fn drained(home: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && spooled(home) > 0 {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(spooled(home), 0, "the spool never drained");
}

#[test]
fn a_turn_spooled_during_a_restart_is_written_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    hook(home, &ended(S, "p0", "a0"));
    stop(daemon);
    hook(home, &prompt(S, "p1", "during"));
    hook(home, &ended(S, "p1", "a1"));
    assert_eq!(spooled(home), 2);
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            reply(S, "a0"),
            user(S, "p1", "during"),
            reply(S, "a1"),
        ],
    );

    let daemon = start(home);
    drained(home);
    stop(daemon);

    let turn: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| {
            record["vendor_turn_id"] == "p1" || record["assistant_message"]["text"] == "a1"
        })
        .collect();
    assert_eq!(
        turn.len(),
        2,
        "one start and one end, got:\n{}",
        show(&turn)
    );
    assert!(
        turn.iter().all(|record| record["hook_id"].is_string()),
        "the hook's records are the stronger fact:\n{}",
        show(&turn)
    );
}

#[test]
fn a_turn_still_running_at_a_restart_gets_no_ending_from_its_partial_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    hook(home, &prompt(S, "p1", "still running"));
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            user(S, "p1", "still running"),
            partial(S, "Let me look at that first."),
        ],
    );

    let daemon = start(home);
    drained(home);
    stop(daemon);

    let endings: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| record["event"] == "turn_ended")
        .collect();
    assert!(
        endings.is_empty(),
        "the vendor has not stopped this turn:\n{}",
        show(&endings)
    );
}

#[test]
fn a_stop_spooled_for_a_lost_prompt_leaves_the_ending_to_the_drain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    // Its prompt hook was lost, and only the `Stop` reached the spool.
    hook(home, &ended(S, "p1", "a1"));
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            user(S, "p1", "only the transcript holds this prompt"),
            reply(S, "a1"),
        ],
    );

    let daemon = start(home);
    drained(home);
    stop(daemon);

    let all = log_lines(home);
    let filled: Vec<&Value> = all
        .iter()
        .filter(|record| record["vendor_turn_id"] == "p1" && record["event"] == "turn_started")
        .collect();
    let endings: Vec<&Value> = all
        .iter()
        .filter(|record| record["event"] == "turn_ended")
        .collect();
    assert_eq!(filled.len(), 1, "{}", show(&all));
    assert_eq!(endings.len(), 1, "{}", show(&all));
    assert!(endings[0]["hook_id"].is_string(), "{}", show(&all));
}

#[test]
fn a_spool_file_nobody_can_read_does_not_stop_the_fill() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    std::fs::create_dir_all(home.join("spool")).expect("spool");
    std::fs::write(home.join("spool/garbage.json"), "{not a request").expect("garbage");
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            user(S, "p1", "missed"),
            reply(S, "a1"),
        ],
    );

    let daemon = start(home);
    drained(home);
    stop(daemon);

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["vendor_turn_id"] == "p1"),
        "the missed turn was not filled"
    );
}

const THREAD: &str = "01a19463-cbd4-7b50-b819-000000000abc";

/// A stand-in `codex app-server` that lists one completed turn, `t1`.
const FAKE_APP_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    try: msg = json.loads(line)
    except Exception: continue
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize": print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"userAgent":"fake"}}), flush=True)
    elif m == "thread/turns/list":
        data = [{"id":"t1","items":[{"type":"userMessage","content":[{"type":"text","text":"during"}]},{"type":"agentMessage","text":"a1"}],
                 "status":"completed","startedAt":1788368800,"completedAt":1788368801}]
        print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"data":data,"nextCursor":None}}), flush=True)
"#;

fn codex_prompt(turn: &str, text: &str) -> String {
    json!({"session_id": THREAD, "hook_event_name": "UserPromptSubmit", "prompt": text,
           "turn_id": turn, "cwd": "/repo", "model": "m", "permission_mode": "default",
           "transcript_path": null})
    .to_string()
}

/// A rollout on disk, so the Codex pass asks about the thread.
fn codex_rollout(home: &Path) {
    let dir = home.join(".codex/sessions/2026/11/01");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join(format!("rollout-2026-11-01T00-00-00-{THREAD}.jsonl")),
        "{}\n",
    )
    .expect("rollout");
}

#[test]
fn the_codex_pass_does_not_write_a_spooled_turn_twice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let stub = tempfile::tempdir().expect("stub");
    let fake = stub.path().join("codex");
    support::write_executable(&fake, FAKE_APP_SERVER);
    let daemon = start(home);
    hook_as(home, "codex", &codex_prompt("t0", "before"));
    stop(daemon);
    hook_as(home, "codex", &codex_prompt("t1", "during"));
    codex_rollout(home);

    let daemon = wait_serving(
        home,
        daemon_command(home)
            .env("CONBUS_CODEX_BIN", &fake)
            .spawn()
            .expect("daemon"),
    );
    drained(home);
    stop(daemon);

    let turn: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| record["vendor_turn_id"] == "t1")
        .collect();
    let started = turn
        .iter()
        .filter(|record| record["event"] == "turn_started")
        .count();
    assert_eq!(started, 1, "\n{}", show(&turn));
    assert!(
        turn.iter().all(|record| record["hook_id"].is_string()),
        "\n{}",
        show(&turn)
    );
}

#[test]
fn a_running_turn_no_hook_reported_is_filled_and_ended_by_its_own_stop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            user(S, "p1", "the hook for this one was lost"),
            partial(S, "Let me look at that first."),
        ],
    );

    let daemon = start(home);
    let while_running = log_lines(home);
    let endings = |records: &[Value]| -> Vec<Value> {
        records
            .iter()
            .filter(|record| record["event"] == "turn_ended")
            .cloned()
            .collect()
    };
    assert!(
        while_running
            .iter()
            .any(|record| record["vendor_turn_id"] == "p1"),
        "the prompt is filled:\n{}",
        show(&while_running)
    );
    assert!(
        endings(&while_running).is_empty(),
        "{}",
        show(&while_running)
    );

    // The turn finishes with the recorder up.
    hook(home, &ended(S, "p1", "the answer"));
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            user(S, "p1", "the hook for this one was lost"),
            partial(S, "Let me look at that first."),
            reply(S, "the answer"),
        ],
    );
    stop(daemon);
    stop(start(home));

    let all = log_lines(home);
    let done = endings(&all);
    assert_eq!(done.len(), 1, "{}", show(&all));
    assert_eq!(done[0]["assistant_message"]["text"], "the answer");
    assert!(done[0]["hook_id"].is_string(), "{}", show(&all));
}

/// A log with two segments, whose older one this process can no longer read and
/// no sidecar answers for.
fn an_older_segment_goes_unreadable(home: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let (older, newer) = (
        home.join("log/segment-00000000.jsonl"),
        home.join("log/segment-00000001.jsonl"),
    );
    let text = std::fs::read_to_string(&older).expect("the older segment");
    let mut later: Value =
        serde_json::from_str(text.lines().last().expect("a line")).expect("json");
    later["seq"] = json!(1000);
    std::fs::write(&newer, later.to_string() + "\n").expect("the newer segment");
    let _ = std::fs::remove_dir_all(home.join("cache/meta"));
    std::fs::set_permissions(&older, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    older
}

#[test]
fn a_read_error_at_start_does_not_ingest_a_chat_export_again() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let export = json!([{"uuid": "11111111-2222-4333-8444-555555555555", "name": "n",
        "created_at": T0, "chat_messages": [
            {"uuid": "m1", "sender": "human", "text": "a chat prompt", "created_at": T0},
            {"uuid": "m2", "sender": "assistant", "text": "a chat reply", "created_at": T0}]}]);
    std::fs::create_dir_all(home.join("chat-export")).expect("dir");
    std::fs::write(
        home.join("chat-export/conversations.json"),
        export.to_string(),
    )
    .expect("write");
    stop(start(home));
    let count = |home: &Path| {
        log_lines(home)
            .iter()
            .filter(|record| record["prompt"]["text"] == "a chat prompt")
            .count()
    };
    assert_eq!(count(home), 1);

    let older = an_older_segment_goes_unreadable(home);
    stop(start(home));
    std::fs::set_permissions(&older, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    assert_eq!(count(home), 1, "the whole export was appended again");

    // Readable again, the next start has the right answer too.
    stop(start(home));
    assert_eq!(
        count(home),
        1,
        "the export was appended once it could be read"
    );
}

#[test]
fn a_fill_the_log_refused_keeps_its_range() {
    // A full disk at start: the fill stops part way and says so. The range it
    // did not finish must not be lost to the records the daemon goes on to serve.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            user(S, "p1", "missed"),
            reply(S, "a1"),
        ],
    );
    let segment = home.join("log/segment-00000000.jsonl");
    // A read-only segment stands in for ENOSPC.
    std::fs::set_permissions(&segment, std::fs::Permissions::from_mode(0o400)).expect("chmod");
    stop(start(home));
    std::fs::set_permissions(&segment, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let marker = home.join("cache/gap-since");
    assert!(marker.exists(), "nothing remembers the range");

    stop(start(home));

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["vendor_turn_id"] == "p1"),
        "the range was not filled"
    );
    assert!(!marker.exists(), "a finished fill clears the marker");
}
