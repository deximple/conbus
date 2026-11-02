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

/// A daemon that goes with its test, however the test ends. One a failed assert
/// left behind would run on for good, over a root that has been deleted.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(home: &Path, retention: &str) -> Daemon {
    start_with_key(home, retention, KEK)
}

fn start_with_key(home: &Path, retention: &str, key: &str) -> Daemon {
    let child = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .env("CONBUS_RETENTION", retention)
        .env("CONBUS_KEK_HEX", key)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("daemon");
    let daemon = Daemon(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let stamp: serde_json::Value = std::fs::read_to_string(home.join("daemon.build"))
            .ok()
            .and_then(|text| serde_json::from_str(text.trim()).ok())
            .unwrap_or_default();
        if stamp["pid"].as_u64() == Some(u64::from(daemon.0.id())) && stamp.get("serving").is_some()
        {
            return daemon;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the daemon never served");
}

fn stop(daemon: Daemon) {
    drop(daemon);
}

/// Ask it to go, as launchd and systemd do, and say what it died of.
fn ask_to_go(mut daemon: Daemon) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    // SAFETY: a plain kill(2) on a child this test still holds, so the pid is its.
    unsafe { libc::kill(daemon.0.id() as libc::pid_t, libc::SIGTERM) };
    daemon.0.wait().expect("wait").signal()
}

/// A value that is not a key: what a Keychain that will not answer looks like.
const NOT_A_KEY: &str = "not a key";

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Past the 2 s idle tick, so a drain has really had its chance.
fn past_a_drain() {
    std::thread::sleep(Duration::from_millis(2600));
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("never: {what}");
}

/// A log with one prompt in it and no daemon: the next one has to open the
/// segment again, which a mode can refuse.
fn a_log_with_one_turn(home: &Path, key: &str) -> std::path::PathBuf {
    let daemon = start_with_key(home, "encrypted_tools", key);
    hook(home, &prompt());
    stop(daemon);
    home.join("log/segment-00000000.jsonl")
}

fn tool_records(home: &Path, kind: &str) -> usize {
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
        .filter(|record| record["event"] == "tool_requested" && record["input"]["kind"] == kind)
        .count()
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
    assert_eq!(said.lines().count(), 1, "{said}");
    assert!(
        said.contains("\"reason\":\"tool bodies elided, not sealed\""),
        "{said}"
    );
    assert!(said.contains("\"detail\":\"1 tool event(s)\""), "{said}");
    assert!(!said.contains("the secret command"), "{said}");
    let doctor = doctor(home.path());
    assert!(
        doctor.contains("1× tool bodies elided, not sealed"),
        "{doctor}"
    );
}

#[test]
fn a_spool_of_thousands_costs_one_line_and_doctor_counts_them_all() {
    // One line per record let a single start fill the drop log, and past its cap
    // the next real drop has no line at all.
    const SPOOLED: usize = 3_000;
    let home = tempfile::tempdir().expect("tempdir");
    a_log_with_one_turn(home.path(), KEK);
    hook(home.path(), &tool_call("template"));
    let spool = home.path().join("spool");
    let template = std::fs::read_dir(&spool)
        .expect("spool")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .next()
        .expect("the hook spooled");
    let mut entry: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&template).expect("read")).expect("json");
    std::fs::remove_file(&template).expect("remove");
    for n in 0..SPOOLED {
        let id = format!("01a19463-cbd4-7b50-b819-{n:012}");
        entry["hid"] = serde_json::json!(id);
        entry["event"]["tool_use_id"] = serde_json::json!(format!("t{n}"));
        std::fs::write(spool.join(format!("{id}.json")), entry.to_string()).expect("entry");
    }

    let daemon = start(home.path(), "encrypted_tools");
    drain(home.path());
    past_a_drain();
    stop(daemon);

    assert_eq!(tool_records(home.path(), "elided"), SPOOLED);
    let said = drops(home.path());
    assert_eq!(said.lines().count(), 1, "{said}");
    assert!(
        said.contains(&format!("\"detail\":\"{SPOOLED} tool event(s)\"")),
        "{said}"
    );
    let doctor = doctor(home.path());
    assert!(
        doctor.contains(&format!("{SPOOLED}× tool bodies elided, not sealed")),
        "{doctor}"
    );
    assert!(doctor.contains(&format!("{SPOOLED} recorded")), "{doctor}");
}

#[test]
fn a_key_that_could_not_be_read_is_counted_and_said_at_most_so_often() {
    // This is the live path, and it can go on for as long as the machine runs. So
    // it is said once, then every ten minutes, and whatever is left on the way out.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start_with_key(home.path(), "encrypted_tools", NOT_A_KEY);
    hook(home.path(), &prompt());
    hook(home.path(), &tool_call("first"));
    until("the first count is said", || !drops(home.path()).is_empty());
    assert_eq!(tool_record(home.path(), "first")["input"]["kind"], "elided");
    hook(home.path(), &tool_call("second"));
    hook(home.path(), &tool_call("third"));
    past_a_drain();
    let inside_the_ten_minutes = drops(home.path()).lines().count();

    let died_of = ask_to_go(daemon);

    assert_eq!(inside_the_ten_minutes, 1, "{}", drops(home.path()));
    let said = drops(home.path());
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines.len(), 2, "{said}");
    assert!(
        lines[0].contains("\"detail\":\"1 tool event(s)\""),
        "{said}"
    );
    assert!(
        lines[1].contains("\"detail\":\"2 tool event(s)\""),
        "{said}"
    );
    assert_eq!(
        died_of,
        Some(libc::SIGTERM),
        "launchd and systemd read how it died, and that has not changed"
    );
    let doctor = doctor(home.path());
    assert!(
        doctor.contains("3× tool bodies elided, not sealed"),
        "{doctor}"
    );
}

#[test]
fn a_held_tool_body_that_could_not_be_sealed_is_counted_too() {
    // A record the log refused comes back through a different path from every
    // other write, and it can be elided there as well.
    let home = tempfile::tempdir().expect("tempdir");
    let segment = a_log_with_one_turn(home.path(), NOT_A_KEY);
    chmod(&segment, 0o400);
    let daemon = start_with_key(home.path(), "encrypted_tools", NOT_A_KEY);

    hook(home.path(), &tool_call("held"));
    chmod(&segment, 0o600);
    until("it lands", || tool_records(home.path(), "elided") == 1);
    until("it says so", || {
        drops(home.path()).contains("elided, not sealed")
    });
    stop(daemon);

    assert!(
        drops(home.path()).contains("\"detail\":\"1 tool event(s)\""),
        "{}",
        drops(home.path())
    );
}

#[test]
fn a_held_tool_body_lands_sealed_and_its_elided_copy_costs_nothing() {
    // The daemon holds what the hook sent, and the hook's own copy has no body.
    // The copy goes when the record lands, so nothing is written twice or lost.
    let home = tempfile::tempdir().expect("tempdir");
    let segment = a_log_with_one_turn(home.path(), KEK);
    chmod(&segment, 0o400);
    let daemon = start(home.path(), "encrypted_tools");

    hook(home.path(), &tool_call("held"));
    chmod(&segment, 0o600);
    until("it lands", || tool_records(home.path(), "sealed") == 1);
    drain(home.path());
    past_a_drain();
    stop(daemon);

    assert_eq!(tool_records(home.path(), "elided"), 0);
    assert!(
        !drops(home.path()).contains("elided, not sealed"),
        "{}",
        drops(home.path())
    );
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
