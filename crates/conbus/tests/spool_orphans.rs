//! A tool event that arrives before its session has a conversation is spooled.
//! It used to be deleted at the next idle tick, so whether it survived a prompt
//! arriving a few seconds later came down to timing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

const SESSION: &str = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

fn start_with(home: &Path, retention: &str) -> Child {
    let mut child = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .env("CONBUS_RETENTION", retention)
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

fn start(home: &Path) -> Child {
    start_with(home, "conversation_only")
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

fn tool_call(id: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": SESSION, "hook_event_name": "PreToolUse", "tool_name": "Bash",
        "tool_input": {"command": "true"}, "tool_use_id": id, "cwd": "/repo"
    })
}

fn prompt(id: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": SESSION, "hook_event_name": "UserPromptSubmit",
        "prompt": "the prompt that binds it", "prompt_id": id, "cwd": "/repo"
    })
}

fn records(home: &Path) -> Vec<serde_json::Value> {
    let mut paths: Vec<_> = std::fs::read_dir(home.join("log"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    paths
        .iter()
        .flat_map(|path| {
            String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default())
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<serde_json::Value>>()
        })
        .collect()
}

fn spooled(home: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(home.join("spool"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn drops(home: &Path) -> String {
    std::fs::read_to_string(home.join("drops.jsonl")).unwrap_or_default()
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("never: {what}");
}

/// Past the 2 s idle tick, so a drain has really had its chance.
fn past_a_drain() {
    std::thread::sleep(Duration::from_millis(2600));
}

fn age(file: &Path, by: Duration) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .expect("open")
        .set_modified(SystemTime::now() - by)
        .expect("age it");
}

#[test]
fn a_tool_event_before_the_first_prompt_survives_when_the_session_binds_later() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &tool_call("tu"));
    past_a_drain();
    let held_on = spooled(home.path()).len();

    hook(home.path(), &prompt("p0"));
    until("it lands", || {
        records(home.path())
            .iter()
            .any(|record| record["tool_use_id"] == "tu")
    });
    until("the spool drains", || spooled(home.path()).is_empty());
    stop(daemon);

    assert_eq!(held_on, 1, "gone at the tick: {}", drops(home.path()));
    assert_eq!(drops(home.path()), "", "nothing was lost");
    let all = records(home.path());
    let tool = all
        .iter()
        .find(|record| record["tool_use_id"] == "tu")
        .expect("the tool call");
    let turn = all
        .iter()
        .find(|record| record["vendor_turn_id"] == "p0")
        .expect("the prompt");
    assert_eq!(tool["bus_id"], turn["bus_id"], "the same conversation");
    assert!(
        tool["spooled_at"].as_str() <= turn["ts"].as_str(),
        "it says it came first, whatever order it landed in: {tool}"
    );
}

#[test]
fn inside_the_idle_tick_it_survives_as_well() {
    // The control: this is the timing that already worked.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &tool_call("tu"));
    hook(home.path(), &prompt("p0"));
    until("the spool drains", || spooled(home.path()).is_empty());
    stop(daemon);
    assert!(
        records(home.path())
            .iter()
            .any(|record| record["tool_use_id"] == "tu")
    );
}

#[test]
fn an_event_waiting_for_its_prompt_waits_across_a_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &tool_call("tu"));
    past_a_drain();
    stop(daemon);

    let daemon = start(home.path());
    past_a_drain();
    let held_on = spooled(home.path()).len();
    hook(home.path(), &prompt("p0"));
    until("the spool drains", || spooled(home.path()).is_empty());
    stop(daemon);

    assert_eq!(held_on, 1);
    assert!(
        records(home.path())
            .iter()
            .any(|record| record["tool_use_id"] == "tu")
    );
}

#[test]
fn an_event_no_prompt_ever_claims_goes_with_a_drop_line_after_a_day() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &tool_call("tu"));
    stop(daemon);
    let entries = spooled(home.path());
    assert_eq!(entries.len(), 1);
    age(&entries[0], DAY + Duration::from_secs(60));

    let daemon = start(home.path());
    until("it goes", || spooled(home.path()).is_empty());
    stop(daemon);

    let said = drops(home.path());
    assert!(
        said.contains("spooled hook still has no conversation (orphan)"),
        "{said}"
    );
    assert!(
        said.contains("tool_requested for session ea1d23e9"),
        "{said}"
    );
}

#[test]
fn a_session_that_ended_before_any_prompt_goes_at_once() {
    // Nothing is coming to bind it, and its end carries nothing to keep: 703 of
    // the 5,726 real orphans were this, sessions with no transcript and no record.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(
        home.path(),
        &serde_json::json!({
            "session_id": SESSION, "hook_event_name": "SessionEnd", "reason": "other"
        }),
    );
    until("it goes", || {
        drops(home.path()).contains("session_ended for session ea1d23e9")
    });
    let left = spooled(home.path());
    stop(daemon);
    assert_eq!(left, Vec::<PathBuf>::new());
}

#[test]
fn a_turn_end_with_no_conversation_goes_at_once() {
    // Landing it a day late would read as the end of whichever turn is running by
    // then, or start a hop on a limit long over. Its words can still come back
    // through gap fill, which tool records cannot.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(
        home.path(),
        &serde_json::json!({
            "session_id": SESSION, "hook_event_name": "Stop", "stop_hook_active": false,
            "prompt_id": "t1", "last_assistant_message": "stale"
        }),
    );
    until("it goes", || {
        drops(home.path()).contains("turn_ended for session ea1d23e9")
    });
    let left = spooled(home.path());
    stop(daemon);
    assert_eq!(left, Vec::<PathBuf>::new());
    assert!(!drops(home.path()).contains("stale"), "never the words");
}

#[test]
fn an_orphan_that_cannot_be_removed_has_not_gone_and_says_nothing_yet() {
    // On a full disk the file going is what makes room for the line, so the line
    // comes second, and only if the file really went.
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &tool_call("tu"));
    stop(daemon);
    let entries = spooled(home.path());
    assert_eq!(entries.len(), 1);
    age(&entries[0], DAY + Duration::from_secs(60));
    let dir = home.path().join("spool");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("close");

    let daemon = start(home.path());
    past_a_drain();
    let still_there = entries[0].exists();
    let said_early = drops(home.path());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("open");
    until("it goes", || spooled(home.path()).is_empty());
    stop(daemon);

    assert!(still_there);
    assert_eq!(said_early, "", "it had not gone");
    assert_eq!(drops(home.path()).lines().count(), 1);
}

#[test]
fn a_waiting_event_is_not_read_again_on_every_tick() {
    // A spool holding thousands of these reads them all every two seconds, in
    // front of every hook behind it. Rewriting the file is how to see whether it
    // was read: what it holds now would land if anyone looked.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &tool_call("tu"));
    past_a_drain();
    let entries = spooled(home.path());
    assert_eq!(entries.len(), 1);
    let mut changed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&entries[0]).expect("read")).expect("json");
    changed["event"] = serde_json::json!({
        "event": "turn_started", "prompt": {"kind": "kept", "text": "read again"}
    });
    std::fs::write(&entries[0], changed.to_string()).expect("rewrite");

    past_a_drain();
    let left = spooled(home.path()).len();
    stop(daemon);

    assert_eq!(left, 1, "it was read again");
}

#[test]
fn a_flood_of_orphans_does_not_crowd_out_a_restart() {
    // These share the spool's 16 MiB with the hooks of the next restart, and a
    // session with no prompt hook can write a lot of them in a day.
    let home = tempfile::tempdir().expect("tempdir");
    let dir = home.path().join("spool");
    std::fs::create_dir_all(&dir).expect("spool");
    let body = "x".repeat(1_000_000);
    for n in 0..10 {
        let id = format!("01a19463-cbd4-7b50-b819-00000000000{n}");
        let entry = serde_json::json!({
            "pv": 6, "harness": "claude_code", "vendor_session_id": SESSION, "hid": id,
            "spooled": "2026-11-01T00:00:00.000Z",
            "event": {"event": "tool_requested", "tool": "Bash", "tool_use_id": format!("t{n}"),
                      "input": {"kind": "kept", "text": body}}
        });
        std::fs::write(dir.join(format!("{id}.json")), entry.to_string()).expect("entry");
    }

    let daemon = start(home.path());
    past_a_drain();
    let left = spooled(home.path());
    stop(daemon);

    let held: u64 = left
        .iter()
        .map(|file| std::fs::metadata(file).expect("meta").len())
        .sum();
    assert!(held <= 8 * 1024 * 1024, "{held} bytes of orphans");
    assert!(!left.is_empty(), "the newest still wait");
    assert!(
        left.iter()
            .any(|file| file.to_string_lossy().ends_with("9.json")),
        "the newest are the ones kept: {left:?}"
    );
    assert_eq!(
        drops(home.path()).lines().count(),
        10 - left.len(),
        "and every one that went says so"
    );
}

#[test]
fn a_session_bound_before_a_restart_is_still_bound_after_it() {
    // Why the drop text stopped blaming a restart: bindings come back from the
    // log. A tool call straight after one lands at once, spooling nothing.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &prompt("p0"));
    stop(daemon);

    let daemon = start(home.path());
    hook(home.path(), &tool_call("tu"));
    let spooled_now = spooled(home.path()).len();
    let landed = records(home.path())
        .iter()
        .any(|record| record["tool_use_id"] == "tu");
    stop(daemon);

    assert_eq!(spooled_now, 0);
    assert!(landed);
}

#[test]
fn an_orphan_the_spool_will_not_take_says_why_without_blaming_a_restart() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_with(home.path(), "ephemeral");
    hook(home.path(), &tool_call("tu"));
    stop(daemon);

    let said = drops(home.path());
    assert!(said.contains("no conversation for this session"), "{said}");
    assert!(!said.contains("restart"), "{said}");
    assert!(
        !said.contains("   "),
        "the old text had a run of spaces: {said}"
    );
}
