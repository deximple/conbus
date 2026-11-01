//! `docs/stability.md` allows a new event kind in 1.x, so a 1.1 hook can meet a
//! 1.0 daemon, which keeps running after an upgrade until it is restarted. The
//! record has to wait for a daemon that can read it, not be refused or deleted.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const HOOK_ID: &str = "01a19463-cbd4-7b50-b819-1c9ecf85ef18";

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

fn start(home: &Path) -> Daemon {
    let child = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
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

/// A request from a build that knows an event kind this one does not.
fn from_the_future(pv: u32) -> serde_json::Value {
    serde_json::json!({
        "pv": pv, "harness": "claude_code", "vendor_session_id": "s-new", "hid": HOOK_ID,
        "event": {"event": "message_displayed", "delta": "from a newer build"}
    })
}

fn spool_entry(home: &Path, name: &str, body: &str) -> PathBuf {
    let dir = home.join("spool");
    std::fs::create_dir_all(&dir).expect("spool");
    let file = dir.join(format!("{name}.json"));
    std::fs::write(&file, body).expect("entry");
    file
}

fn age(file: &Path, by: Duration) {
    std::fs::File::options()
        .write(true)
        .open(file)
        .expect("open")
        .set_modified(SystemTime::now() - by)
        .expect("age it");
}

fn drops(home: &Path) -> String {
    std::fs::read_to_string(home.join("drops.jsonl")).unwrap_or_default()
}

/// Past the first drains and one idle tick.
fn past_a_drain() {
    std::thread::sleep(Duration::from_millis(2600));
}

fn ask(home: &Path, line: &str) -> String {
    let mut socket =
        std::os::unix::net::UnixStream::connect(home.join("conbus.sock")).expect("connect");
    writeln!(socket, "{line}").expect("write");
    let mut reply = String::new();
    BufReader::new(&socket).read_line(&mut reply).expect("read");
    reply
}

#[test]
fn an_entry_with_an_event_kind_this_build_does_not_know_stays_spooled() {
    let home = tempfile::tempdir().expect("tempdir");
    let file = spool_entry(home.path(), HOOK_ID, &from_the_future(99).to_string());

    let daemon = start(home.path());
    past_a_drain();
    let kept = file.exists();
    stop(daemon);

    assert!(kept, "destroyed: {}", drops(home.path()));
    assert_eq!(drops(home.path()), "", "nothing is gone yet");
}

#[test]
fn the_same_goes_for_a_newer_build_that_forgot_to_move_the_protocol() {
    let home = tempfile::tempdir().expect("tempdir");
    let file = spool_entry(home.path(), HOOK_ID, &from_the_future(6).to_string());

    let daemon = start(home.path());
    past_a_drain();
    let kept = file.exists();
    stop(daemon);

    assert!(kept, "destroyed: {}", drops(home.path()));
}

#[test]
fn a_newer_entry_goes_with_a_drop_line_once_its_life_is_over() {
    // Nothing may wait for a build that never comes. It names what it was and
    // whose it was, and nothing it said.
    let home = tempfile::tempdir().expect("tempdir");
    let file = spool_entry(home.path(), HOOK_ID, &from_the_future(99).to_string());
    age(&file, WEEK + Duration::from_secs(60));

    let daemon = start(home.path());
    past_a_drain();
    let kept = file.exists();
    stop(daemon);

    assert!(!kept, "it outlived its life");
    let said = drops(home.path());
    assert!(said.contains("spooled hook refused"), "{said}");
    assert!(said.contains("protocol v"), "{said}");
    assert!(
        said.contains("message_displayed for session s-new"),
        "{said}"
    );
    assert!(!said.contains("from a newer build"), "content: {said}");
}

#[test]
fn a_newer_protocol_with_a_kind_this_build_knows_has_a_life_too() {
    let home = tempfile::tempdir().expect("tempdir");
    let readable = serde_json::json!({
        "pv": 99, "harness": "claude_code", "vendor_session_id": "s-new", "hid": HOOK_ID,
        "event": {"event": "session_candidate"}
    });
    let file = spool_entry(home.path(), HOOK_ID, &readable.to_string());
    age(&file, WEEK + Duration::from_secs(60));

    let daemon = start(home.path());
    past_a_drain();
    let kept = file.exists();
    stop(daemon);

    assert!(!kept, "it outlived its life");
    assert!(drops(home.path()).contains("protocol v"));
}

#[test]
fn a_file_that_is_nobodys_request_goes_at_once() {
    let home = tempfile::tempdir().expect("tempdir");
    for (name, body) in [
        ("not-json", "not json at all"),
        ("no-protocol", r#"{"harness":"claude_code"}"#),
    ] {
        spool_entry(home.path(), name, body);
    }

    let daemon = start(home.path());
    past_a_drain();
    let left = std::fs::read_dir(home.path().join("spool"))
        .expect("spool")
        .count();
    stop(daemon);

    assert_eq!(left, 0);
    assert_eq!(drops(home.path()).lines().count(), 2);
}

#[test]
fn an_entry_that_cannot_be_read_just_now_is_not_deleted() {
    // A read error is no decision about the entry. A hook writes these `0600`, so
    // a mode stands in for the disk that says no.
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().expect("tempdir");
    let body = serde_json::json!({
        "pv": 6, "harness": "claude_code", "vendor_session_id": "s-read", "hid": HOOK_ID,
        "event": {"event": "turn_started", "prompt": {"kind": "kept", "text": "read me later"}}
    });
    let file = spool_entry(home.path(), HOOK_ID, &body.to_string());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).expect("close");

    let daemon = start(home.path());
    past_a_drain();
    let kept = file.exists();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("open");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && file.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    stop(daemon);

    assert!(kept, "deleted on a read error: {}", drops(home.path()));
    assert!(!file.exists(), "and read once it could be");
    assert_eq!(drops(home.path()), "");
}

#[test]
fn a_newer_hook_with_a_new_event_kind_is_told_to_restart_the_daemon() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());

    let newer = ask(home.path(), &from_the_future(99).to_string());
    let same = ask(home.path(), &from_the_future(6).to_string());
    stop(daemon);

    for reply in [newer, same] {
        assert!(reply.contains("protocol v"), "{reply}");
        assert!(reply.contains("Restart the daemon"), "{reply}");
        assert!(!reply.contains("from a newer build"), "content: {reply}");
    }
}

#[test]
fn a_line_that_is_no_hook_at_all_still_gets_the_plain_answer() {
    // `bench-corpus.sh` probes with `{}` and only wants a reply, and an older
    // hook this build cannot read is a bug, not something a restart mends.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());

    let empty = ask(home.path(), "{}");
    let older = ask(home.path(), &from_the_future(1).to_string());
    stop(daemon);

    for reply in [empty, older] {
        assert!(reply.contains("unreadable request"), "{reply}");
    }
}
