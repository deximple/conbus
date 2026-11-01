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

/// A stamp from now. A fill takes a line stamped well before its file was created
/// for a copy, so what a test wants filled cannot carry a fixed date.
fn now() -> String {
    conbus_core::clock::Timestamp::now().to_string()
}

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

/// A daemon this test owns, stopped when the test ends however it ends. A failing
/// test that only stopped it on the way out left one serving a deleted root.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait until the stamp says this daemon serves: its start-up fill is done.
fn wait_serving(home: &Path, daemon: Child) -> Daemon {
    let daemon = Daemon(daemon);
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let stamped = stamp(home);
        if stamped["pid"].as_u64() == Some(u64::from(daemon.0.id()))
            && stamped.get("serving").is_some()
        {
            return daemon;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the daemon never served");
}

fn start(home: &Path) -> Daemon {
    wait_serving(home, daemon_command(home).spawn().expect("daemon"))
}

fn stop(daemon: Daemon) {
    drop(daemon);
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
    json!({"type": "user", "sessionId": session, "timestamp": now(), "version": "2.1.316",
           "cwd": "/repo", "entrypoint": "cli", "promptId": id,
           "message": {"role": "user", "content": text}})
    .to_string()
}

fn reply(session: &str, text: &str) -> String {
    json!({"type": "assistant", "sessionId": session, "timestamp": now(), "version": "2.1.316",
           "message": {"role": "assistant", "stop_reason": "end_turn",
                       "content": [{"type": "text", "text": text}]}})
    .to_string()
}

/// What a running turn's transcript ends on: text, and the model not stopped.
fn partial(session: &str, text: &str) -> String {
    json!({"type": "assistant", "sessionId": session, "timestamp": now(), "version": "2.1.316",
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

/// A `user` line the way a front end writes it.
fn sdk_user(session: &str, id: &str, text: &str) -> String {
    let flags = json!({"promptSource": "sdk"});
    flagged(session, id, text, flags)
}

/// A line stamped well before the file it is written to can have existed.
fn from_before_the_file(line: String) -> String {
    let mut line: Value = serde_json::from_str(&line).expect("json");
    let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
    let hours_ago = conbus_core::clock::Timestamp::now().seconds_since(epoch) - 2 * 3600;
    line["timestamp"] = json!(
        conbus_core::clock::Timestamp::from_unix_seconds(hours_ago)
            .expect("a time")
            .to_string()
    );
    line.to_string()
}

/// A turn neither conversation holds, in the parent's transcript and in the one of
/// a fork taken moments later: the parent's lines with the fork's own session id
/// and the parent's prompt id, the shape real forks have before 2026-08-31.
fn a_fork_taken_right_after_a_turn_nobody_holds(home: &Path) {
    a_recorded_fork(home);
    transcript(
        home,
        PARENT,
        &[
            sdk_user(PARENT, "pp", "the parent's prompt"),
            reply(PARENT, "the parent's reply"),
            sdk_user(
                PARENT,
                "pq",
                "a parent turn only its own conversation should hold",
            ),
            reply(PARENT, "the parent's other reply"),
        ],
    );
    // CHILD sorts first by name, so only their ages put the parent first.
    std::thread::sleep(Duration::from_millis(20));
    transcript(
        home,
        CHILD,
        &[
            sdk_user(CHILD, "pp", "the parent's prompt"),
            reply(CHILD, "the parent's reply"),
            sdk_user(
                CHILD,
                "pq",
                "a parent turn only its own conversation should hold",
            ),
            reply(CHILD, "the parent's other reply"),
            sdk_user(CHILD, "cc", "the fork's prompt"),
            reply(CHILD, "the fork's reply"),
        ],
    );
}

fn filled_under(all: &[Value], turn: &str) -> Vec<Value> {
    let bus = all
        .iter()
        .find(|record| record["vendor_turn_id"] == turn)
        .expect("the live prompt")["bus_id"]
        .clone();
    all.iter()
        .filter(|record| record["bus_id"] == bus && record.get("backfilled_from").is_some())
        .cloned()
        .collect()
}

#[test]
fn a_turn_two_transcripts_list_goes_to_the_older_file_and_not_the_fork() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_fork_taken_right_after_a_turn_nobody_holds(home);

    stop(start(home));

    let all = log_lines(home);
    let under_fork = filled_under(&all, "cc");
    assert!(
        under_fork.is_empty(),
        "under the fork:\n{}",
        show(&under_fork)
    );
    let under_parent = filled_under(&all, "pp");
    assert_eq!(
        under_parent.len(),
        2,
        "the prompt and its ending:\n{}",
        show(&all)
    );
    assert_eq!(under_parent[0]["vendor_turn_id"], "pq");
    assert_eq!(under_parent[0]["vendor_session_id"], PARENT);

    stop(start(home));
    assert_eq!(
        log_lines(home).len(),
        all.len(),
        "a second start adds nothing"
    );
}

#[test]
fn a_fork_does_not_write_a_turn_of_a_parent_the_recorder_never_saw() {
    // Most of what the real case is: the parent's transcript is not one a fill reads,
    // and nothing holds these turns, so only their age says they are not the fork's.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(CHILD, "cc", "the fork's prompt"));
    hook(home, &ended(CHILD, "cc", "the fork's reply"));
    stop(daemon);
    transcript(
        home,
        CHILD,
        &[
            from_before_the_file(sdk_user(CHILD, "pq", "the parent's prompt")),
            from_before_the_file(reply(CHILD, "the parent's reply")),
            sdk_user(CHILD, "cc", "the fork's prompt"),
            reply(CHILD, "the fork's reply"),
            sdk_user(CHILD, "c2", "said while the recorder was down"),
            reply(CHILD, "answered while the recorder was down"),
        ],
    );

    let (ok, said) = backfill_gaps(home, Path::new("/nonexistent/codex"), true);

    assert!(ok, "{said}");
    let filled: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| record.get("backfilled_from").is_some())
        .collect();
    assert_eq!(filled.len(), 2, "\n{}", show(&filled));
    assert_eq!(
        filled[0]["prompt"]["text"],
        "said while the recorder was down"
    );
    assert!(said.contains("2 line older than its file"), "{said}");
}

#[test]
fn a_start_leaves_a_turn_another_conversation_recorded_out_of_its_fork() {
    // Taken moments after the turn, so its age does not give it away, and the parent
    // has no transcript in range: the record is all there is to go on.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_recorded_fork(home);
    transcript(
        home,
        CHILD,
        &[
            sdk_user(CHILD, "pp", "the parent's prompt"),
            reply(CHILD, "the parent's reply"),
            sdk_user(CHILD, "cc", "the fork's prompt"),
            reply(CHILD, "the fork's reply"),
        ],
    );

    stop(start(home));

    let under_fork = filled_under(&log_lines(home), "cc");
    assert!(
        under_fork.is_empty(),
        "under the fork:\n{}",
        show(&under_fork)
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

/// What every stand-in `codex app-server` starts with: `thread/turns/list` as the
/// real one answers it. The schema of codex-cli 0.166.0 says of `sortDirection`
/// "defaults to descending", and the cursor goes on in the direction asked for.
/// `turns` is given oldest first. A stand-in that listed oldest first whatever it
/// was asked hid that the fill never asked.
const TURNS_ANSWER: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
def turns_page(msg, turns, page_size=100):
    params = msg["params"]
    listed = list(turns) if params.get("sortDirection") == "asc" else list(reversed(turns))
    at = int(params.get("cursor") or 0)
    size = min(params.get("limit") or page_size, page_size)
    more = at + size < len(listed)
    return {"jsonrpc":"2.0","id":msg.get("id"),"result":{"data":listed[at:at + size],"nextCursor":str(at + size) if more else None}}
"#;

/// A stand-in `codex app-server` that lists one completed turn, `t1`.
const FAKE_APP_SERVER: &str = r#"
for line in sys.stdin:
    try: msg = json.loads(line)
    except Exception: continue
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize": print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"userAgent":"fake"}}), flush=True)
    elif m == "thread/turns/list":
        data = [{"id":"t1","items":[{"type":"userMessage","content":[{"type":"text","text":"during"}]},{"type":"agentMessage","text":"a1"}],
                 "status":"completed","startedAt":1788368800,"completedAt":1788368801}]
        print(json.dumps(turns_page(msg, data)), flush=True)
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
    // It also lists t2, which nothing spooled: filling it shows the pass ran, and
    // a Codex that could not start would pass this for the wrong reason.
    const LISTS_TWO_TURNS: &str = r#"
def turn(i, said, reply):
    return {"id":i,"items":[{"type":"userMessage","content":[{"type":"text","text":said}]},{"type":"agentMessage","text":reply}],
            "status":"completed","startedAt":1788368800,"completedAt":1788368801}
for line in sys.stdin:
    try: msg = json.loads(line)
    except Exception: continue
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize": print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"userAgent":"fake"}}), flush=True)
    elif m == "thread/turns/list":
        data = [turn("t1", "during", "a1"), turn("t2", "only the thread holds this", "a2")]
        print(json.dumps(turns_page(msg, data)), flush=True)
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(LISTS_TWO_TURNS);
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

    let all = log_lines(home);
    let started = |id: &str| {
        all.iter()
            .filter(|record| record["vendor_turn_id"] == id && record["event"] == "turn_started")
            .count()
    };
    assert_eq!(started("t2"), 1, "the pass ran:\n{}", show(&all));
    assert_eq!(started("t1"), 1, "\n{}", show(&all));
    let t1: Vec<&Value> = all
        .iter()
        .filter(|record| record["vendor_turn_id"] == "t1")
        .collect();
    assert!(
        t1.iter().all(|record| record["hook_id"].is_string()),
        "\n{}",
        show(&all)
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

/// A prompt recorded at `when`, in unix seconds.
fn recorded_at(home: &Path, session: &str, turn: &str, when: i64) {
    use conbus_core::clock::Timestamp;
    use conbus_core::event::{Body, Event, Harness, Source, VendorTurnId};
    use conbus_core::ids::BusId;
    use conbus_core::record::Entry;
    use conbus_core::retention::Retention;
    use conbus_core::segment::SegmentLog;

    let mut log = SegmentLog::open(home.join("log"), Retention::default()).expect("open");
    log.append(Entry {
        bus_id: BusId::mint(),
        observed_at: Timestamp::from_unix_seconds(when),
        source: Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: None,
            vendor_session_id: Some(session.into()),
            workspace: Some("/repo".into()),
            entrypoint: None,
            backfilled_from: None,
            profile: None,
        },
        vendor_turn_id: Some(VendorTurnId::new(turn)),
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
        event: Event::TurnStarted {
            author: None,
            prompt: Body::kept("a recorded prompt"),
        },
    })
    .expect("append");
}

#[test]
fn a_fill_the_log_refused_keeps_the_range_from_before_it_wrote() {
    // A full disk after a long downtime. The fill stops part way and says so, and
    // the range it did not finish starts at the downtime, not at the last write.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
    let now = conbus_core::clock::Timestamp::now().seconds_since(epoch);
    recorded_at(home, S, "p0", now - 3 * 3600);
    transcript(
        home,
        S,
        &[
            user(S, "p0", "a recorded prompt"),
            user(S, "p1", "missed"),
            reply(S, "a1"),
        ],
    );
    std::fs::File::options()
        .write(true)
        .open(home.join(format!(".claude/projects/-repo/{S}.jsonl")))
        .expect("open")
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 3600))
        .expect("backdate");
    let segment = home.join("log/segment-00000000.jsonl");
    // A read-only segment stands in for ENOSPC.
    std::fs::set_permissions(&segment, std::fs::Permissions::from_mode(0o400)).expect("chmod");
    stop(start(home));
    std::fs::set_permissions(&segment, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let marker = home.join("cache/gap-since");
    assert!(marker.exists(), "nothing remembers the range");
    // The daemon goes on serving, so the next start's own range is past the file.
    recorded_at(home, CHILD, "later", now + 2 * 3600);

    stop(start(home));

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["vendor_turn_id"] == "p1"),
        "the range was lost to the records served after it"
    );
    assert!(!marker.exists(), "a finished fill clears the marker");
}

/// A live Codex thread with a rollout touched just now, and the recorder down.
fn a_live_codex_thread(home: &Path) {
    let daemon = start(home);
    hook_as(home, "codex", &codex_prompt("t0", "before"));
    stop(daemon);
    codex_rollout(home);
}

fn stand_in(script: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("stub");
    let path = dir.path().join("codex");
    support::write_executable(&path, script);
    (dir, path)
}

/// A stand-in `codex app-server`: `body` is its loop, after [`TURNS_ANSWER`]. It runs
/// once here, with nothing to read: the first exec of a new script can take seconds
/// on a loaded machine, and charged to a start that reads as a server that stalled.
fn codex_stand_in(body: &str) -> (tempfile::TempDir, PathBuf) {
    let (stub, fake) = stand_in(&format!("{TURNS_ANSWER}{body}"));
    let warmed = Command::new(&fake)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .expect("warm the stand-in");
    assert!(warmed.success());
    (stub, fake)
}

/// One start with this binary answering for Codex.
fn start_with_codex(home: &Path, codex: &Path) {
    let daemon = wait_serving(
        home,
        daemon_command(home)
            .env("CONBUS_CODEX_BIN", codex)
            .spawn()
            .expect("daemon"),
    );
    stop(daemon);
}

#[test]
fn a_codex_pass_that_could_not_run_keeps_its_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_live_codex_thread(home);

    stop(start(home)); // the app-server cannot start

    assert!(
        home.join("cache/gap-since-codex").exists(),
        "the thread was not read and nothing remembers the range"
    );
    assert!(
        !home.join("cache/gap-since").exists(),
        "a broken Codex must not hold the transcripts' range back, or every start \
         reads further into them"
    );
}

#[test]
fn a_codex_pass_that_could_not_run_does_not_lose_a_deferred_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_live_codex_thread(home);
    std::fs::create_dir_all(home.join("cache")).expect("cache");
    std::fs::write(home.join("cache/gap-since"), "2026-10-01T00:00:00.000Z").expect("marker");

    stop(start(home));

    assert_eq!(
        std::fs::read_to_string(home.join("cache/gap-since-codex")).expect("marker"),
        "2026-10-01T00:00:00.000Z",
        "the earlier cut's range was not carried to the pass that has not read it"
    );
}

/// A record from later than the rollout by more than the margin, so that only a
/// remembered range still reaches the rollout.
fn newer_records_arrive(home: &Path) {
    use conbus_core::event::{Body, Event, Harness, Source};
    use conbus_core::ids::BusId;
    use conbus_core::record::Entry;
    use conbus_core::retention::Retention;
    use conbus_core::segment::SegmentLog;

    let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
    let later = conbus_core::clock::Timestamp::now().seconds_since(epoch) + 2 * 3600;
    let mut log = SegmentLog::open(home.join("log"), Retention::default()).expect("open");
    log.append(Entry {
        bus_id: BusId::mint(),
        observed_at: conbus_core::clock::Timestamp::from_unix_seconds(later),
        source: Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: None,
            vendor_session_id: Some(S.into()),
            workspace: None,
            entrypoint: None,
            backfilled_from: None,
            profile: None,
        },
        vendor_turn_id: None,
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
        event: Event::TurnStarted {
            author: None,
            prompt: Body::kept("a later prompt"),
        },
    })
    .expect("append");
}

#[test]
fn a_range_the_codex_pass_could_not_read_is_read_by_the_next_start_that_can() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(FAKE_APP_SERVER);
    a_live_codex_thread(home);
    stop(start(home)); // the app-server cannot start
    newer_records_arrive(home);

    start_with_codex(home, &fake);

    let all = log_lines(home);
    assert!(
        all.iter().any(|record| record["vendor_turn_id"] == "t1"),
        "the thread's turn was lost to newer records:\n{}",
        show(&all)
    );
    assert!(
        !home.join("cache/gap-since-codex").exists(),
        "a pass that finished clears its marker"
    );
}

#[test]
fn an_app_server_that_dies_part_way_keeps_the_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let witness = home.join("the-stand-in-was-asked");
    let dies = format!(
        r#"
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        print(json.dumps({{"jsonrpc":"2.0","id":msg.get("id"),"result":{{"userAgent":"fake"}}}}), flush=True)
    elif msg.get("method") == "thread/turns/list":
        open({:?}, "w").close()
        sys.exit(1)
"#,
        witness.to_str().expect("utf-8")
    );
    let (_stub, fake) = codex_stand_in(&dies);
    a_live_codex_thread(home);

    start_with_codex(home, &fake);

    assert!(witness.exists(), "the pass reached the thread");
    assert!(home.join("cache/gap-since-codex").exists());
}

/// A stand-in that answers every `thread/turns/list` with this error.
fn refuses_with(error: &str) -> String {
    format!(
        r#"
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        print(json.dumps({{"jsonrpc":"2.0","id":msg.get("id"),"result":{{"userAgent":"fake"}}}}), flush=True)
    elif msg.get("method") == "thread/turns/list":
        print(json.dumps({{"jsonrpc":"2.0","id":msg.get("id"),"error":{error}}}), flush=True)
"#
    )
}

#[test]
fn a_server_without_the_method_does_not_hold_the_range_for_ever() {
    // Asking again gets the same answer, and a range that never clears has every
    // later start bring up the app-server again.
    for error in [
        r#"{"code":-32601,"message":"method not found"}"#,
        r#"{"code":-32600,"message":"Invalid request: unknown variant `thread/turns/list`, expected one of `initialize`"}"#,
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let (_stub, fake) = codex_stand_in(&refuses_with(error));
        a_live_codex_thread(home);

        start_with_codex(home, &fake);

        assert!(!home.join("cache/gap-since-codex").exists(), "{error}");
    }
}

#[test]
fn a_thread_a_server_refuses_keeps_the_range_whatever_it_says() {
    // Only "no such method" says asking again cannot help. The cost of this one is
    // an app-server start at each restart, for as long as the server says it.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(&refuses_with(
        r#"{"code":-32600,"message":"no rollout found"}"#,
    ));
    a_live_codex_thread(home);

    start_with_codex(home, &fake);

    assert!(home.join("cache/gap-since-codex").exists());
}

#[test]
fn a_refusal_that_says_retry_later_keeps_the_range() {
    // Not every refusal is permanent: -32001 is the real server's overloaded answer.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let flag = home.join("refused-once");
    let script = format!(
        r#"
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({{"jsonrpc":"2.0","id":i,"result":{{"userAgent":"fake"}}}}), flush=True)
    elif m == "thread/turns/list":
        if not os.path.exists({flag:?}):
            open({flag:?}, "w").close()
            print(json.dumps({{"jsonrpc":"2.0","id":i,"error":{{"code":-32001,"message":"Server overloaded; retry later."}}}}), flush=True)
            continue
        data = [{{"id":"t1","items":[{{"type":"userMessage","content":[{{"type":"text","text":"missed"}}]}},{{"type":"agentMessage","text":"a"}}],
                 "status":"completed","startedAt":1788368800,"completedAt":1788368801}}]
        print(json.dumps(turns_page(msg, data)), flush=True)
"#,
        flag = flag.to_str().expect("utf-8")
    );
    let (_stub, fake) = codex_stand_in(&script);
    a_live_codex_thread(home);

    start_with_codex(home, &fake);
    assert!(flag.exists(), "the first start was refused");
    newer_records_arrive(home);
    start_with_codex(home, &fake);

    let all = log_lines(home);
    assert!(
        all.iter().any(|record| record["vendor_turn_id"] == "t1"),
        "the missed turn was lost:\n{}",
        show(&all)
    );
    assert!(!home.join("cache/gap-since-codex").exists());
}

#[test]
fn a_refused_thread_by_hand_is_counted_and_keeps_the_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(&refuses_with(
        r#"{"code":-32001,"message":"Server overloaded; retry later."}"#,
    ));
    a_live_codex_thread(home);
    start_with_codex(home, &fake);
    assert!(home.join("cache/gap-since-codex").exists());

    let (ok, said) = backfill_gaps(home, &fake, true);

    assert!(ok, "{said}");
    assert!(said.contains("1 thread/turns/list refused"), "{said}");
    assert!(!said.contains("unavailable"), "{said}");
    assert!(home.join("cache/gap-since-codex").exists(), "not read yet");
}

fn backfill_gaps(home: &Path, codex: &Path, apply: bool) -> (bool, String) {
    let mut command = Command::new(bin());
    command.args(["backfill", "--gaps"]);
    if apply {
        command.arg("--apply");
    }
    env_for(&mut command, home);
    let output = command
        .env("CONBUS_CODEX_BIN", codex)
        .output()
        .expect("backfill");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn backfill_gaps_fills_a_live_codex_thread() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(FAKE_APP_SERVER);
    a_live_codex_thread(home);
    let bus = log_lines(home)
        .iter()
        .find(|record| record["vendor_turn_id"] == "t0")
        .expect("the live prompt")["bus_id"]
        .clone();

    let (ok, dry) = backfill_gaps(home, &fake, false);
    assert!(ok, "{dry}");
    assert!(dry.contains("1 thread(s), 1 turn(s), 2 record(s)"), "{dry}");
    assert!(
        !log_lines(home)
            .iter()
            .any(|record| record["vendor_turn_id"] == "t1"),
        "a dry run writes nothing"
    );

    let (ok, applied) = backfill_gaps(home, &fake, true);
    assert!(ok, "{applied}");
    let filled: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| record["vendor_turn_id"] == "t1")
        .collect();
    assert_eq!(filled.len(), 2, "{}", show(&filled));
    assert!(filled.iter().all(|record| record["bus_id"] == bus));
}

#[test]
fn backfill_gaps_says_when_codex_could_not_be_read_and_still_fills_the_rest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_live_codex_thread(home);

    let (ok, said) = backfill_gaps(home, Path::new("/nonexistent/codex"), true);

    assert!(ok, "{said}");
    assert!(said.contains("codex     history unavailable"), "{said}");
}

const THREADS: [&str; 6] = [
    "01a19463-0000-7000-8000-000000000001",
    "01a19463-0000-7000-8000-000000000002",
    "01a19463-0000-7000-8000-000000000003",
    "01a19463-0000-7000-8000-000000000004",
    "01a19463-0000-7000-8000-000000000005",
    "01a19463-0000-7000-8000-000000000006",
];

/// A stand-in that answers one thread per call, slowly, so a budget covers only
/// some of them.
const SLOW_APP_SERVER: &str = r#"
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"userAgent":"fake"}}), flush=True)
    elif m == "thread/turns/list":
        time.sleep(0.8)
        t = msg["params"]["threadId"]
        data = [{"id":"turn-of-"+t,"items":[{"type":"userMessage","content":[{"type":"text","text":"said in "+t}]},{"type":"agentMessage","text":"a"}],
                 "status":"completed","startedAt":1788368800,"completedAt":1788368801}]
        print(json.dumps(turns_page(msg, data)), flush=True)
"#;

/// Every thread live, the first of them the most recently touched: the order the
/// pass asks them in.
fn live_threads_touched_in_order(home: &Path) {
    let daemon = start(home);
    for thread in THREADS {
        let payload = json!({"session_id": thread, "hook_event_name": "UserPromptSubmit",
            "prompt": "seen", "turn_id": "seen", "cwd": "/repo", "model": "m",
            "permission_mode": "default", "transcript_path": null});
        hook_as(home, "codex", &payload.to_string());
    }
    stop(daemon);
    let rollouts = home.join(".codex/sessions/2026/11/01");
    std::fs::create_dir_all(&rollouts).expect("dir");
    for (age, thread) in THREADS.iter().enumerate() {
        let path = rollouts.join(format!("rollout-2026-11-01T00-00-00-{thread}.jsonl"));
        let file = std::fs::File::create(&path).expect("rollout");
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(10 + age as u64 * 10))
            .expect("mtime");
    }
}

#[test]
fn threads_a_budget_cut_off_are_reached_by_later_starts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(SLOW_APP_SERVER);
    live_threads_touched_in_order(home);

    let reached = |home: &Path, thread: &str| {
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == format!("said in {thread}"))
    };
    for _ in 0..8 {
        let daemon = wait_serving(
            home,
            daemon_command(home)
                .env("CONBUS_CODEX_BIN", &fake)
                // Five answers at most, with room for a slow first exec: a stand-in
                // that answers nothing inside its limit counts as stalled.
                .env("CONBUS_GAP_BUDGET_MS", "4000")
                .spawn()
                .expect("daemon"),
        );
        stop(daemon);
        if THREADS.iter().all(|thread| reached(home, thread)) {
            break;
        }
    }

    for thread in THREADS {
        assert!(reached(home, thread), "{thread} was never reached");
    }
    let turns = log_lines(home)
        .into_iter()
        .filter(|record| {
            record["event"] == "turn_started"
                && record["prompt"]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("said in"))
        })
        .count();
    assert_eq!(turns, THREADS.len(), "each thread's turn once");
}

#[test]
fn a_thread_that_kills_the_app_server_does_not_starve_the_ones_behind_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(&format!(
        r#"
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({{"jsonrpc":"2.0","id":i,"result":{{"userAgent":"fake"}}}}), flush=True)
    elif m == "thread/turns/list":
        t = msg["params"]["threadId"]
        if t == {first:?}:
            sys.exit(1)
        data = [{{"id":"turn-of-"+t,"items":[{{"type":"userMessage","content":[{{"type":"text","text":"said in "+t}}]}},{{"type":"agentMessage","text":"a"}}],
                 "status":"completed","startedAt":1788368800,"completedAt":1788368801}}]
        print(json.dumps(turns_page(msg, data)), flush=True)
"#,
        first = THREADS[0]
    ));
    live_threads_touched_in_order(home);

    start_with_codex(home, &fake);
    start_with_codex(home, &fake);

    let all = log_lines(home);
    for thread in &THREADS[1..] {
        assert!(
            all.iter()
                .any(|record| record["prompt"]["text"] == format!("said in {thread}")),
            "{thread} never reached behind the thread that dies:\n{}",
            show(&all)
        );
    }
    assert!(
        home.join("cache/gap-since-codex").exists(),
        "the thread that dies is still unread"
    );
}

#[test]
fn the_codex_pass_does_not_copy_a_parent_thread_into_its_fork() {
    // Older than THREAD, so older than the thread that lists it.
    const LISTS_THE_PARENTS_TURN: &str = r#"
def turn(i, said):
    return {"id":i,"items":[{"type":"userMessage","content":[{"type":"text","text":said}]},{"type":"agentMessage","text":"a"}],
            "status":"completed","startedAt":1788368800,"completedAt":1788368801}
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"userAgent":"fake"}}), flush=True)
    elif m == "thread/turns/list":
        data = [turn("01a00000-0000-7000-8000-000000000001", "the parent's prompt"),
                turn("01a19463-ffff-7000-8000-000000000002", "the fork's own prompt")]
        print(json.dumps(turns_page(msg, data)), flush=True)
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(LISTS_THE_PARENTS_TURN);
    a_live_codex_thread(home);

    start_with_codex(home, &fake);

    let filled: Vec<Value> = log_lines(home)
        .into_iter()
        .filter(|record| {
            record["event"] == "turn_started" && record.get("backfilled_from").is_some()
        })
        .collect();
    assert_eq!(filled.len(), 1, "\n{}", show(&filled));
    assert_eq!(filled[0]["prompt"]["text"], "the fork's own prompt");
}
/// A stand-in that never answers `initialize`: the stall `AppServer` documents,
/// from an MCP server it is trying to start.
fn a_stalled_app_server() -> (tempfile::TempDir, PathBuf) {
    stand_in("#!/bin/sh\nexec sleep 60\n")
}

/// One start, keeping what the daemon said in `daemon-notes` under the root.
fn start_with_stalled_codex(home: &Path, codex: &Path) {
    let notes = std::fs::File::options()
        .create(true)
        .append(true)
        .open(home.join("daemon-notes"))
        .expect("notes");
    let daemon = wait_serving(
        home,
        daemon_command(home)
            .env("CONBUS_CODEX_BIN", codex)
            .env("CONBUS_GAP_BUDGET_MS", "1000")
            .stderr(Stdio::from(notes))
            .spawn()
            .expect("daemon"),
    );
    stop(daemon);
}

/// How many starts asked the app-server, and how many left it alone. Read from
/// what the daemon says rather than from the stand-in, whose first exec can
/// outlast its limit on a loaded machine.
fn asked_and_left_alone(home: &Path) -> (usize, usize) {
    let notes = std::fs::read_to_string(home.join("daemon-notes")).unwrap_or_default();
    let deferred: Vec<&str> = notes
        .lines()
        .filter(|line| line.contains("codex gap fill deferred ("))
        .collect();
    let left_alone = deferred
        .iter()
        .filter(|line| line.contains("it stalled at an earlier start"))
        .count();
    (deferred.len() - left_alone, left_alone)
}

#[test]
fn a_stalled_app_server_costs_one_start_not_every_start() {
    // Eight seconds in the real case, with the socket closed. Keeping the range
    // must not mean paying that on every restart for as long as Codex is wedged.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = a_stalled_app_server();
    a_live_codex_thread(home);

    start_with_stalled_codex(home, &fake);
    start_with_stalled_codex(home, &fake);
    start_with_stalled_codex(home, &fake);

    assert_eq!(
        asked_and_left_alone(home),
        (1, 2),
        "asked again within the hour"
    );
    assert!(
        home.join("cache/gap-since-codex").exists(),
        "and the range is still held"
    );
}

#[test]
fn a_stalled_app_server_is_asked_again_once_the_back_off_has_passed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = a_stalled_app_server();
    a_live_codex_thread(home);
    start_with_stalled_codex(home, &fake);

    // Two hours ago, as far as the record of it goes.
    let read = home.join("cache/gap-read");
    let held = std::fs::read_to_string(&read).expect("a record of the stall");
    let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
    let long_ago = conbus_core::clock::Timestamp::now().seconds_since(epoch) - 2 * 3600;
    let rewound: Vec<String> = held
        .lines()
        .map(|line| match line.split_once('\t') {
            Some((_, "codex app-server stalled")) => {
                format!("{long_ago}\tcodex app-server stalled")
            }
            _ => line.to_string(),
        })
        .collect();
    assert!(
        rewound.iter().any(|line| line.ends_with("stalled")),
        "the stall was noted: {held}"
    );
    std::fs::write(&read, rewound.join("\n") + "\n").expect("rewind");

    start_with_stalled_codex(home, &fake);

    assert_eq!(asked_and_left_alone(home), (2, 0));
}

#[test]
fn an_app_server_that_is_only_missing_is_tried_every_start() {
    // It costs nothing, and installing Codex should fill the range at the next one.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(FAKE_APP_SERVER);
    a_live_codex_thread(home);
    stop(start(home)); // the app-server cannot start

    start_with_codex(home, &fake);

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["vendor_turn_id"] == "t1"),
        "the next start did not try"
    );
}

#[test]
fn backfill_gaps_by_hand_asks_a_stalled_app_server_regardless() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = a_stalled_app_server();
    a_live_codex_thread(home);
    start_with_stalled_codex(home, &fake);
    let (_stub2, working) = codex_stand_in(FAKE_APP_SERVER);

    let (ok, said) = backfill_gaps(home, &working, true);

    assert!(ok, "{said}");
    assert!(
        said.contains("1 thread(s), 1 turn(s), 2 record(s)"),
        "{said}"
    );
}

/// A thread's turns, given oldest first, listed the way the real server lists them.
/// `page_size` is what the server keeps a page to.
fn turns_of_a_thread(turns: &str, page_size: u32) -> String {
    format!(
        r#"
def prompt(i, said, reply=None, status="completed"):
    items=[{{"type":"userMessage","content":[{{"type":"text","text":said}}]}}]
    if reply: items.append({{"type":"agentMessage","text":reply}})
    return {{"id":i,"items":items,"status":status,"startedAt":1788368800,"completedAt":1788368801 if status=="completed" else None}}
def only_reply(i, reply):
    return {{"id":i,"items":[{{"type":"agentMessage","text":reply}}],"status":"completed","startedAt":1788368800,"completedAt":1788368801}}
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({{"jsonrpc":"2.0","id":i,"result":{{"userAgent":"fake"}}}}), flush=True)
    elif m == "thread/turns/list":
        print(json.dumps(turns_page(msg, {turns}, {page_size})), flush=True)
"#
    )
}

fn turns_filled(all: &[Value]) -> Vec<String> {
    all.iter()
        .filter(|record| {
            record["event"] == "turn_started" && record.get("backfilled_from").is_some()
        })
        .map(|record| record["vendor_turn_id"].as_str().unwrap_or("-").to_string())
        .collect()
}

#[test]
fn a_running_codex_turn_gets_no_ending_from_an_older_turns_reply() {
    // Listed newest first, the reply-only turn came straight after the running
    // prompt, and its reply and `completed` landed on it for good.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(&turns_of_a_thread(
        r#"[prompt("t1","older prompt","older first reply"), only_reply("t2","an older reply"), prompt("t3","still running",None,"inProgress")]"#,
        100,
    ));
    a_live_codex_thread(home);

    start_with_codex(home, &fake);

    let all = log_lines(home);
    assert_eq!(turns_filled(&all), ["t1", "t3"], "\n{}", show(&all));
    let endings: Vec<&Value> = all
        .iter()
        .filter(|record| record["event"] == "turn_ended")
        .collect();
    assert_eq!(endings.len(), 1, "\n{}", show(&all));
    assert_eq!(endings[0]["vendor_turn_id"], "t1");
    assert_eq!(endings[0]["assistant_message"]["text"], "an older reply");
}

#[test]
fn a_filled_codex_thread_is_written_oldest_first_across_pages() {
    // The log's newest record decides what a reader takes as the last turn.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(&turns_of_a_thread(
        r#"[prompt("t1","oldest","a1"), prompt("t2","middle","a2"), prompt("t3","next","a3"), prompt("t4","newest","a4"), prompt("t5","latest","a5")]"#,
        2,
    ));
    a_live_codex_thread(home);

    start_with_codex(home, &fake);

    assert_eq!(
        turns_filled(&log_lines(home)),
        ["t1", "t2", "t3", "t4", "t5"]
    );
}

#[test]
fn a_missed_turn_the_vendor_says_a_tool_ended_gets_its_ending() {
    // The next start finds the prompt held and skips the ending for good, so a turn
    // that gets none here reads as still running for ever.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    hook(home, &ended(S, "p0", "a0"));
    stop(daemon);
    let tool_use = json!({"type": "assistant", "sessionId": S, "timestamp": now(), "version": "2.1.316",
        "message": {"role": "assistant", "stop_reason": "tool_use",
                    "content": [{"type": "text", "text": "the closing words"},
                                {"type": "tool_use", "id": "tu", "name": "StructuredOutput", "input": {}}]}})
    .to_string();
    let result = json!({"type": "user", "sessionId": S, "timestamp": now(), "version": "2.1.316",
        "promptId": "p1", "toolEndsTurn": true, "toolUseResult": "ok",
        "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu", "content": "ok"}]}})
    .to_string();
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            reply(S, "a0"),
            sdk_user(S, "p1", "missed"),
            tool_use,
            result,
        ],
    );

    stop(start(home));

    let all = log_lines(home);
    assert!(
        all.iter().any(|record| record["vendor_turn_id"] == "p1"),
        "the prompt is filled:\n{}",
        show(&all)
    );
    assert!(
        all.iter()
            .any(|record| record["assistant_message"]["text"] == "the closing words"),
        "no ending:\n{}",
        show(&all)
    );
}

const THREAD_A: &str = "01a19463-0000-7000-8000-00000000000a";
const THREAD_B: &str = "01a19463-0000-7000-8000-00000000000b";

/// Answers thread A. Hangs on thread B until `answer-b` exists beside the script.
const WEDGED_ON_B: &str = r#"
here = os.path.dirname(os.path.abspath(sys.argv[0]))
flag = os.path.join(here, "answer-b")
open(os.path.join(here, "launches"), "a").write("x\n")
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"userAgent":"fake"}}), flush=True)
    elif m == "thread/list":
        data = [{"id":"01a19463-0000-7000-8000-00000000000a","updatedAt":4000000000},{"id":"01a19463-0000-7000-8000-00000000000b","updatedAt":4000000000}]
        print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"data":data,"nextCursor":None}}), flush=True)
    elif m == "thread/turns/list":
        t = msg["params"]["threadId"]
        if t.endswith("b") and not os.path.exists(flag):
            time.sleep(600)
        data = [{"id":"turn-of-"+t,"items":[{"type":"userMessage","content":[{"type":"text","text":"said in "+t}]},{"type":"agentMessage","text":"a"}],
                 "status":"completed","startedAt":1788368800,"completedAt":1788368801}]
        print(json.dumps(turns_page(msg, data)), flush=True)
"#;

fn codex_thread_live(home: &Path, thread: &str, turn: &str, text: &str) {
    let payload = json!({"session_id": thread, "hook_event_name": "UserPromptSubmit", "prompt": text,
        "turn_id": turn, "cwd": "/repo", "model": "m", "permission_mode": "default",
        "transcript_path": null});
    hook_as(home, "codex", &payload.to_string());
}

/// A thread's rollout, last written `age` seconds ago.
fn rollout_of(home: &Path, thread: &str, age: u64) {
    let dir = home.join(".codex/sessions/2026/11/01");
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join(format!("rollout-2026-11-01T00-00-00-{thread}.jsonl"));
    let file = std::fs::File::create(&path).expect("rollout");
    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(age))
        .expect("mtime");
}

/// One start on this budget, keeping what the daemon said in `notes`.
fn timed_start(home: &Path, codex: &Path, budget_ms: &str, notes: &Path) -> Duration {
    let file = std::fs::File::options()
        .create(true)
        .append(true)
        .open(notes)
        .expect("notes");
    let at = Instant::now();
    let daemon = wait_serving(
        home,
        daemon_command(home)
            .env("CONBUS_CODEX_BIN", codex)
            .env("CONBUS_GAP_BUDGET_MS", budget_ms)
            .stderr(Stdio::from(file))
            .spawn()
            .expect("daemon"),
    );
    let took = at.elapsed();
    stop(daemon);
    took
}

/// Two live threads, the one that wedges the server touched longer ago, so the one
/// that keeps growing is asked first.
fn a_thread_behind_one_that_keeps_growing(home: &Path) {
    let daemon = start(home);
    codex_thread_live(home, THREAD_A, "a0", "before");
    codex_thread_live(home, THREAD_B, "b0", "before");
    stop(daemon);
    rollout_of(home, THREAD_B, 60);
    rollout_of(home, THREAD_A, 0);
}

#[test]
fn a_thread_that_wedges_the_server_costs_one_start_not_every_start() {
    // The stall note needed a start where no thread answered, and the thread the
    // person keeps using always answers first: the whole limit at every start.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(WEDGED_ON_B);
    a_thread_behind_one_that_keeps_growing(home);
    let notes = home.join("notes");
    let mut asked = 0;
    let mut times = Vec::new();
    for _ in 0..4 {
        rollout_of(home, THREAD_A, 0); // the person keeps using A
        let before = std::fs::read_to_string(&notes).unwrap_or_default().len();
        times.push(timed_start(home, &fake, "1500", &notes));
        let said = std::fs::read_to_string(&notes).unwrap_or_default();
        if !said[before..].contains("it stalled at an earlier start") {
            asked += 1;
        }
    }
    assert_eq!(
        asked, 1,
        "the wedged server was asked at {asked} of 4 starts, each costing its limit: {times:?}"
    );
    assert!(
        home.join("cache/gap-since-codex").exists(),
        "and the wedged thread's range is still held"
    );
}

#[test]
fn a_by_hand_fill_that_reads_a_stalled_thread_ends_the_cost() {
    // "Until it is asked by hand": until now that run released nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (stub, fake) = codex_stand_in(WEDGED_ON_B);
    a_thread_behind_one_that_keeps_growing(home);
    let notes = home.join("notes");
    timed_start(home, &fake, "1500", &notes);
    assert!(
        home.join("cache/gap-since-codex").exists(),
        "the range is held"
    );

    // By hand, with a server that can answer B this time.
    std::fs::write(stub.path().join("answer-b"), "").expect("flag");
    let (ok, said) = backfill_gaps(home, &fake, true);
    assert!(ok, "{said}");
    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == format!("said in {THREAD_B}")),
        "the hand run read B"
    );
    assert!(
        !home.join("cache/gap-since-codex").exists(),
        "and with everything read no range is held"
    );
    std::fs::remove_file(stub.path().join("answer-b")).expect("flag");

    // B would hang now, so a start that asked it again would hold the range again.
    rollout_of(home, THREAD_A, 0);
    timed_start(home, &fake, "1500", &notes);
    assert!(
        !home.join("cache/gap-since-codex").exists(),
        "after a by-hand fill read the thread, the next start asked it again"
    );
    assert!(!home.join("cache/gap-read").exists());
}

#[test]
fn a_dry_run_by_hand_leaves_a_stalled_threads_marks_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (stub, fake) = codex_stand_in(WEDGED_ON_B);
    a_thread_behind_one_that_keeps_growing(home);
    timed_start(home, &fake, "1500", &home.join("notes"));
    let held = std::fs::read_to_string(home.join("cache/gap-read")).expect("noted");

    std::fs::write(stub.path().join("answer-b"), "").expect("flag");
    let (ok, said) = backfill_gaps(home, &fake, false);

    assert!(ok, "{said}");
    assert_eq!(
        std::fs::read_to_string(home.join("cache/gap-read")).expect("noted"),
        held
    );
    assert!(home.join("cache/gap-since-codex").exists());
}

#[test]
fn a_stalled_thread_is_asked_again_once_the_back_off_has_passed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (stub, fake) = codex_stand_in(WEDGED_ON_B);
    a_thread_behind_one_that_keeps_growing(home);
    let notes = home.join("notes");
    timed_start(home, &fake, "1500", &notes);

    // Two hours ago, as far as the record of it goes.
    let read = home.join("cache/gap-read");
    let held = std::fs::read_to_string(&read).expect("a record of the stall");
    let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
    let long_ago = conbus_core::clock::Timestamp::now().seconds_since(epoch) - 2 * 3600;
    let on_b = format!("codex app-server stalled on {THREAD_B}");
    assert!(held.contains(&on_b), "the stall was noted: {held}");
    let rewound: Vec<String> = held
        .lines()
        .map(|line| match line.split_once('\t') {
            Some((_, source)) if source == on_b => format!("{long_ago}\t{source}"),
            _ => line.to_string(),
        })
        .collect();
    std::fs::write(&read, rewound.join("\n") + "\n").expect("rewind");
    std::fs::write(stub.path().join("answer-b"), "").expect("flag");

    timed_start(home, &fake, "1500", &notes);

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == format!("said in {THREAD_B}")),
        "the thread was not asked again"
    );
    assert!(!home.join("cache/gap-since-codex").exists());
}

#[test]
fn a_server_that_stalls_on_its_first_thread_is_left_alone_after() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(WEDGED_ON_B);
    let daemon = start(home);
    codex_thread_live(home, THREAD_B, "b0", "before");
    stop(daemon);
    rollout_of(home, THREAD_B, 0);
    let notes = home.join("notes");

    let first = timed_start(home, &fake, "1500", &notes);
    let second = timed_start(home, &fake, "1500", &notes);

    let said = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(
        said.contains("it stalled at an earlier start"),
        "asked twice: {first:?} {second:?}"
    );
}

#[test]
fn a_start_with_only_a_stalled_thread_left_does_not_bring_the_server_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (stub, fake) = codex_stand_in(WEDGED_ON_B);
    a_thread_behind_one_that_keeps_growing(home);
    let notes = home.join("notes");
    timed_start(home, &fake, "1500", &notes);
    let launches = || {
        std::fs::read_to_string(stub.path().join("launches"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let before = launches();

    // A has not changed since it was read, and B is being left alone.
    timed_start(home, &fake, "1500", &notes);

    assert_eq!(launches(), before, "brought up with nothing to ask");
    let said = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(said.contains("it stalled at an earlier start"), "{said}");
}

/// A record of the stall as a start leaves it, with the range it holds.
fn stalled_on_b_just_now(home: &Path) {
    let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
    let now = conbus_core::clock::Timestamp::now().seconds_since(epoch);
    std::fs::create_dir_all(home.join("cache")).expect("cache");
    std::fs::write(
        home.join("cache/gap-read"),
        format!("{now}\tcodex app-server stalled on {THREAD_B}\n"),
    )
    .expect("gap-read");
}

#[test]
fn a_stalled_thread_is_left_alone_when_the_list_comes_from_thread_list() {
    // With names the walk cannot read, the threads come from the server itself.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = codex_stand_in(WEDGED_ON_B);
    let daemon = start(home);
    codex_thread_live(home, THREAD_A, "a0", "before");
    codex_thread_live(home, THREAD_B, "b0", "before");
    stop(daemon);
    let sessions = home.join(".codex/sessions");
    std::fs::create_dir_all(&sessions).expect("dir");
    std::fs::write(sessions.join("not-a-rollout.txt"), "x").expect("file");
    stalled_on_b_just_now(home);
    let notes = home.join("notes");

    timed_start(home, &fake, "3000", &notes);

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == format!("said in {THREAD_A}")),
        "the thread beside it was still read"
    );
    let said = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(said.contains("it stalled at an earlier start"), "{said}");
}

/// One live thread with a rollout, for a stand-in to stall on.
fn a_thread_to_ask(home: &Path) {
    let daemon = start(home);
    codex_thread_live(home, THREAD, "t0", "before");
    stop(daemon);
    rollout_of(home, THREAD, 0);
}

fn alive(pid: i32) -> bool {
    // SAFETY: `kill` takes no pointers, and signal 0 is only a question.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn pid_in(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .expect("the stand-in ran")
        .trim()
        .parse()
        .expect("a pid")
}

#[test]
fn a_wedged_app_server_behind_a_wrapper_does_not_hold_the_start_past_its_limit() {
    // The shape of an npm `codex`: a wrapper spawning the real binary with stdout
    // inherited. The wrapper dies and its child holds the read open for its whole life.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (stub, fake) = stand_in(
        "#!/bin/sh\n[ \"$1\" = warm ] && exit 0\nsleep 25 &\necho $! > \"$(dirname \"$0\")/child\"\nwait\n",
    );
    let warmed = Command::new(&fake).arg("warm").status().expect("warm");
    assert!(warmed.success());
    a_thread_to_ask(home);

    let took = timed_start(home, &fake, "1000", &home.join("notes"));

    assert!(
        took < Duration::from_secs(20),
        "a 1 s limit held the start for {took:?}"
    );
    // No process is left behind: the wrapper's child went with it.
    let child = pid_in(&stub.path().join("child"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(child) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(child), "the wrapper's child was left behind");
}

#[test]
fn something_outside_the_servers_group_does_not_hold_the_start_either() {
    // Nothing can reach what started a session of its own, and it holds stdout open
    // just the same, so the read cannot wait for the pipe to close.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (_stub, fake) = stand_in(
        r#"#!/usr/bin/env python3
import os, sys, time
if len(sys.argv) > 1 and sys.argv[1] == "warm":
    sys.exit(0)
if os.fork() == 0:
    os.setsid()
    time.sleep(15)
    os._exit(0)
time.sleep(60)
"#,
    );
    let warmed = Command::new(&fake).arg("warm").status().expect("warm");
    assert!(warmed.success());
    a_thread_to_ask(home);

    let took = timed_start(home, &fake, "1000", &home.join("notes"));

    assert!(
        took < Duration::from_secs(13),
        "a 1 s limit held the start for {took:?}"
    );
    assert!(
        home.join("cache/gap-since-codex").exists(),
        "the range is held"
    );
}

/// What the race `still_missing` documents does when it fires. The bind pause only
/// makes the moment easy to hit: any hook behind the spool read is the same.
#[test]
#[ignore = "a known race, left open: the hook must land after the fill read the spool, for a \
            line it already read, which is seconds at a restart, and it costs one extra \
            turn_started"]
fn a_hook_that_spools_after_the_fill_read_the_spool_is_written_once() {
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
            user(S, "p1", "its line is already written"),
            reply(S, "a1"),
        ],
    );
    let child = daemon_command(home)
        .env("CONBUS_TEST_PAUSE_BEFORE_BIND_MS", "4000")
        .spawn()
        .expect("daemon");
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end
        && !log_lines(home)
            .iter()
            .any(|record| record["vendor_turn_id"] == "p1")
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    hook(home, &prompt(S, "p1", "its line is already written"));
    let daemon = wait_serving(home, child);
    std::thread::sleep(Duration::from_millis(300));
    stop(daemon);
    let all = log_lines(home);
    let started: Vec<Value> = all
        .iter()
        .filter(|record| record["vendor_turn_id"] == "p1" && record["event"] == "turn_started")
        .cloned()
        .collect();
    assert_eq!(started.len(), 1, "\n{}", show(&all));
}

#[test]
fn a_thread_read_with_no_rollout_state_is_read_again() {
    // In the `thread/list` fallback there is no rollout to give a thread a state, so a
    // thread noted once stayed "already read" for as long as the range was held.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let second_turn = home.join("second-turn-exists");
    let b_answers = home.join("b-answers");
    let script = format!(
        r#"
def turn(i, said):
    return {{"id":i,"items":[{{"type":"userMessage","content":[{{"type":"text","text":said}}]}},{{"type":"agentMessage","text":"a"}}],
            "status":"completed","startedAt":1788368800,"completedAt":1788368801}}
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({{"jsonrpc":"2.0","id":i,"result":{{"userAgent":"fake"}}}}), flush=True)
    elif m == "thread/list":
        data = [{{"id":{a:?},"updatedAt":4000000000}},{{"id":{b:?},"updatedAt":4000000000}}]
        print(json.dumps({{"jsonrpc":"2.0","id":i,"result":{{"data":data,"nextCursor":None}}}}), flush=True)
    elif m == "thread/turns/list":
        t = msg["params"]["threadId"]
        if t == {b:?} and not os.path.exists({answers:?}):
            print(json.dumps({{"jsonrpc":"2.0","id":i,"error":{{"code":-32001,"message":"Server overloaded; retry later."}}}}), flush=True)
            continue
        data = [turn("first-"+t, "first in "+t)]
        if os.path.exists({second:?}):
            data.append(turn("second-"+t, "second in "+t))
        print(json.dumps(turns_page(msg, data)), flush=True)
"#,
        a = THREAD_A,
        b = THREAD_B,
        answers = b_answers.to_str().expect("utf-8"),
        second = second_turn.to_str().expect("utf-8"),
    );
    let (_stub, fake) = codex_stand_in(&script);
    let daemon = start(home);
    codex_thread_live(home, THREAD_A, "a0", "before");
    codex_thread_live(home, THREAD_B, "b0", "before");
    stop(daemon);
    // A name the walk cannot read, so the pass falls back to `thread/list`.
    let sessions = home.join(".codex/sessions");
    std::fs::create_dir_all(&sessions).expect("dir");
    std::fs::write(sessions.join("not-a-rollout.txt"), "x").expect("file");

    start_with_codex(home, &fake);
    assert!(
        home.join("cache/gap-since-codex").exists(),
        "B holds the range"
    );
    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == format!("first in {THREAD_A}"))
    );

    // A hookless second turn lands in A, and B can now be read.
    std::fs::write(&second_turn, "").expect("flag");
    std::fs::write(&b_answers, "").expect("flag");
    start_with_codex(home, &fake);

    let all = log_lines(home);
    assert!(
        all.iter()
            .any(|record| record["prompt"]["text"] == format!("second in {THREAD_A}")),
        "A's new turn was skipped as already read:\n{}",
        show(&all)
    );
}

#[test]
fn a_fork_whose_first_prompt_hook_was_lost_does_not_record_the_parents_words() {
    // The fork shape on real transcripts since 2026-08-31: the parent's lines under
    // the fork's own id and one prompt id, that of the fork's first prompt. One id
    // opens one turn, so the parent's first words would stand for it.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(CHILD, "q", "the fork's second prompt"));
    stop(daemon);
    transcript(
        home,
        CHILD,
        &[
            from_before_the_file(sdk_user(CHILD, "p", "the parent's first prompt")),
            from_before_the_file(reply(CHILD, "the parent's first reply")),
            from_before_the_file(sdk_user(CHILD, "p", "the parent's second prompt")),
            from_before_the_file(reply(CHILD, "the parent's second reply")),
            sdk_user(CHILD, "p", "the fork's first prompt"),
            reply(CHILD, "the fork's first reply"),
            sdk_user(CHILD, "q", "the fork's second prompt"),
            reply(CHILD, "the fork's second reply"),
        ],
    );

    stop(start(home));

    let all = log_lines(home);
    let turn: Vec<&Value> = all
        .iter()
        .filter(|record| record["vendor_turn_id"] == "p")
        .collect();
    assert_eq!(turn.len(), 1, "\n{}", show(&all));
    assert_eq!(
        turn[0]["prompt"]["text"],
        "the fork's first prompt",
        "\n{}",
        show(&all)
    );
    assert!(
        all.iter()
            .any(|record| record["assistant_message"]["text"] == "the fork's first reply"),
        "\n{}",
        show(&all)
    );
    assert!(
        !all.iter().any(|record| record["prompt"]["text"]
            .as_str()
            .is_some_and(|text| text.starts_with("the parent's"))),
        "\n{}",
        show(&all)
    );
}

#[test]
fn a_transcript_the_fill_could_not_read_keeps_its_range() {
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
            user(S, "p1", "only the transcript holds this"),
            reply(S, "a1"),
        ],
    );
    let file = home.join(format!(".claude/projects/-repo/{S}.jsonl"));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    stop(start(home));

    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(
        home.join("cache/gap-since").exists(),
        "nothing holds the range"
    );
    newer_records_arrive(home);
    stop(start(home));

    let all = log_lines(home);
    assert!(
        all.iter().any(|record| record["vendor_turn_id"] == "p1"),
        "the turn was lost to the newer record:\n{}",
        show(&all)
    );
    assert!(!home.join("cache/gap-since").exists(), "the marker clears");
    assert!(!home.join("cache/gap-read").exists());
}

#[test]
fn the_markers_keep_the_data_roots_modes() {
    // `docs/stability.md`: directories 0700, files 0600. On a fresh root these are
    // the first thing to make `cache/`, before any sidecar closes it.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_thread_to_ask(home);
    std::fs::remove_dir_all(home.join("cache")).ok();

    stop(start(home)); // the app-server cannot start, so the markers stay

    let mode = |path: &Path| {
        std::fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o777)
            .unwrap_or(0)
    };
    assert_eq!(
        (
            mode(&home.join("cache")),
            mode(&home.join("cache/gap-since-codex")),
            mode(&home.join("cache/gap-read"))
        ),
        (0o700, 0o600, 0o600),
        "cache, gap-since-codex, gap-read"
    );
}

#[test]
fn a_marker_garbled_or_left_over_is_read_as_nothing_and_replaced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    a_thread_to_ask(home);
    std::fs::create_dir_all(home.join("cache")).ok();
    std::fs::write(home.join("cache/gap-read"), "not\ta number\n\n\t\nx").expect("write");
    std::fs::write(home.join("cache/gap-since"), "garbled").expect("write");
    std::fs::write(home.join("cache/gap-since-codex"), "garbled").expect("write");

    stop(start(home));

    let codex = std::fs::read_to_string(home.join("cache/gap-since-codex")).expect("held");
    assert!(
        codex.parse::<conbus_core::clock::Timestamp>().is_ok(),
        "replaced with a time: {codex}"
    );
    assert!(!home.join("cache/gap-since").exists());
}

fn chat(uuid: &str, messages: &[(&str, &str, &str)]) -> Value {
    json!({"uuid": uuid, "name": "n", "created_at": T0, "chat_messages":
        messages.iter()
            .map(|(id, sender, text)| json!({"uuid": id, "sender": sender, "text": text, "created_at": T0}))
            .collect::<Vec<_>>()})
}

/// An export last written a month ago, as one moved into the folder keeps that time.
fn an_old_export(path: &Path, conversation: &str, text: &str) {
    let export = json!([chat(
        conversation,
        &[("m1", "human", text), ("m2", "assistant", "a1")]
    )]);
    std::fs::write(path, export.to_string()).expect("write");
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(30 * 86400))
        .expect("mtime");
}

#[test]
fn an_export_moved_in_during_a_downtime_is_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    newer_records_arrive(home);
    std::fs::create_dir_all(home.join("chat-export")).expect("dir");
    an_old_export(
        &home.join("chat-export/conversations.json"),
        "11111111-2222-4333-8444-555555555555",
        "from the export",
    );

    stop(start(home));

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == "from the export"),
        "never read"
    );
}

#[test]
fn an_export_read_once_is_not_read_again_at_every_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    std::fs::create_dir_all(home.join("chat-export")).expect("dir");
    let path = home.join("chat-export/conversations.json");
    an_old_export(&path, "11111111-2222-4333-8444-555555555555", "said once");
    stop(start(home));
    let said = |text: &str| {
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == text)
    };
    assert!(said("said once"));

    // The same size and the same old time: nothing says it changed, so a read of it
    // is all that could bring this conversation in.
    an_old_export(&path, "22222222-2222-4333-8444-555555555555", "said twce");
    stop(start(home));

    assert!(!said("said twce"), "read again");
}

/// A real FIFO, and it held open for as long as the file returned lives. Open
/// read-write and non-blocking so this cannot hang, and so a reader's open does not
/// either. A read waits for as long as this holds the write end, and when a test
/// fails, its unwinding closes it, and what was blocked sees the end and goes away.
fn a_fifo(path: &Path) -> std::fs::File {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("dir");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("a path");
    // SAFETY: `name` is a valid C string that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
    std::fs::File::options()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .expect("open the fifo")
}

/// Run `conbus` with these arguments, and kill it if it has not finished in time.
fn conbus_within(home: &Path, args: &[&str], limit: Duration) -> Option<(bool, String)> {
    let mut command = Command::new(bin());
    command.args(args);
    env_for(&mut command, home);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("conbus");
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if child.try_wait().expect("wait").is_some() {
            let output = child.wait_with_output().expect("output");
            return Some((
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned()
                    + &String::from_utf8_lossy(&output.stderr),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[test]
fn a_fifo_named_like_a_transcript_does_not_hold_a_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(CHILD, "cc", "the fifo's session"));
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    let _fifo = a_fifo(&home.join(format!(".claude/projects/-repo/{CHILD}.jsonl")));
    transcript(
        home,
        S,
        &[
            user(S, "p0", "before"),
            user(S, "p1", "beside the fifo"),
            reply(S, "a1"),
        ],
    );
    let notes = home.join("notes");

    timed_start(home, Path::new("/nonexistent/codex"), "4000", &notes);

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == "beside the fifo"),
        "the transcript beside it was not read"
    );
    assert!(
        !home.join("cache/gap-since").exists(),
        "a read that can never work does not hold the range"
    );
    let said = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(said.contains("1 not a regular file"), "{said}");
}

#[test]
fn a_fifo_newer_than_a_sessions_transcript_does_not_stand_in_for_it() {
    // One file per session, the most recently written, so it must not be this.
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
            user(S, "p1", "only the real file holds this"),
            reply(S, "a1"),
        ],
    );
    std::thread::sleep(Duration::from_millis(1100));
    let _fifo = a_fifo(&home.join(format!(".claude/projects/-moved/{S}.jsonl")));

    stop(start(home));

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["vendor_turn_id"] == "p1"),
        "the session's real transcript was not read"
    );
}

#[test]
fn a_fifo_does_not_hold_a_fill_by_hand_either() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(CHILD, "cc", "the fifo's session"));
    stop(daemon);
    let _live = a_fifo(&home.join(format!(".claude/projects/-repo/{CHILD}.jsonl")));
    // Not live, so what `backfill --apply` reads.
    let _unseen = a_fifo(&home.join(format!(".claude/projects/-repo/{PARENT}.jsonl")));

    let (ok, said) = conbus_within(home, &["backfill", "--gaps"], Duration::from_secs(30))
        .expect("`backfill --gaps` was still reading after 30 s");
    assert!(ok, "{said}");
    assert!(said.contains("1 not a regular file"), "{said}");

    let (ok, said) = conbus_within(home, &["backfill"], Duration::from_secs(30))
        .expect("`backfill` was still reading after 30 s");
    assert!(ok, "{said}");
    assert!(said.contains("1 not a regular file"), "{said}");
}

#[test]
fn a_fifo_in_the_drop_folder_does_not_hold_a_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let daemon = start(home);
    hook(home, &prompt(S, "p0", "before"));
    stop(daemon);
    let folder = home.join("chat-export");
    let _json = a_fifo(&folder.join("conversations.json"));
    let _zip = a_fifo(&folder.join("data-export-2026-11-01.zip"));
    let _inner = a_fifo(&folder.join("a/conversations.json"));
    std::fs::create_dir_all(folder.join("b")).expect("dir");
    let export = json!([chat(
        "11111111-2222-4333-8444-555555555555",
        &[
            ("m1", "human", "beside the fifos"),
            ("m2", "assistant", "a1")
        ]
    )]);
    std::fs::write(folder.join("b/conversations.json"), export.to_string()).expect("write");
    let notes = home.join("notes");

    timed_start(home, Path::new("/nonexistent/codex"), "4000", &notes);

    assert!(
        log_lines(home)
            .iter()
            .any(|record| record["prompt"]["text"] == "beside the fifos"),
        "the export beside them was not read"
    );
    let said = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(said.contains("3 not a regular file"), "{said}");
}

#[test]
fn a_fifo_named_as_an_export_by_hand_is_refused_and_does_not_hang() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let path = home.join("conversations.json");
    let _fifo = a_fifo(&path);

    let (ok, said) = conbus_within(
        home,
        &["backfill", "--chat-export", path.to_str().expect("utf-8")],
        Duration::from_secs(30),
    )
    .expect("`backfill --chat-export` was still reading after 30 s");

    assert!(!ok, "{said}");
    assert!(said.contains("not a regular file"), "{said}");
}

#[test]
fn a_fifo_named_like_a_rollout_is_no_sign_of_a_thread_to_ask() {
    // The server would be the one reading it, and it can wedge on it the same way. By
    // hand there are ten minutes for that, so this stand-in answers.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let (stub, fake) = codex_stand_in(WEDGED_ON_B);
    std::fs::write(stub.path().join("answer-b"), "").expect("flag");
    let daemon = start(home);
    codex_thread_live(home, THREAD_B, "b0", "before");
    stop(daemon);
    let _fifo = a_fifo(&home.join(format!(
        ".codex/sessions/2026/11/01/rollout-2026-11-01T00-00-00-{THREAD_B}.jsonl"
    )));
    let launches = || {
        std::fs::read_to_string(stub.path().join("launches"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let before = launches();

    let (ok, said) = backfill_gaps(home, &fake, false);

    assert!(ok, "{said}");
    assert!(said.contains("1 not a regular file"), "{said}");
    assert_eq!(launches(), before, "brought up to ask about it");
}
