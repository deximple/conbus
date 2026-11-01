//! A log that will not take a record — a full disk, a read-only segment — is
//! temporary, and must not destroy the record. Neither may an entry that can
//! never land sit in the spool for ever.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

const SESSION: &str = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";
const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

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

/// What the hook printed, and what it said on stderr.
fn prompt(home: &Path, id: &str, text: &str) -> (String, String) {
    let mut child = Command::new(bin())
        .args(["hook", "--harness", "claude_code"])
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hook");
    let payload = serde_json::json!({
        "session_id": SESSION, "hook_event_name": "UserPromptSubmit",
        "prompt": text, "prompt_id": id, "cwd": "/repo"
    });
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(), "a hook always exits zero");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
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

fn turns(home: &Path, id: &str) -> usize {
    records(home)
        .iter()
        .filter(|record| record["vendor_turn_id"] == id)
        .count()
}

fn spooled(home: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(home.join("spool"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect()
        })
        .unwrap_or_default()
}

fn drops(home: &Path) -> String {
    std::fs::read_to_string(home.join("drops.jsonl")).unwrap_or_default()
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// A log with one record in it, and no daemon: the next one will have to open
/// the segment again, which a mode can refuse. A daemon already holding it open
/// would go on writing.
fn a_log_with_one_turn(home: &Path) -> PathBuf {
    let daemon = start(home);
    prompt(home, "p0", "before");
    stop(daemon);
    home.join("log/segment-00000000.jsonl")
}

/// Until `done`, or panic saying what we were waiting for.
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

#[test]
fn a_spooled_request_survives_a_log_that_refuses_it_and_lands_when_it_stops() {
    let home = tempfile::tempdir().expect("tempdir");
    let segment = a_log_with_one_turn(home.path());
    prompt(home.path(), "p1", "spooled, then the log refuses it");
    assert_eq!(spooled(home.path()).len(), 1);
    chmod(&segment, 0o400);

    let daemon = start(home.path());
    past_a_drain();
    let held_on = spooled(home.path()).len();
    let lost_early = drops(home.path());
    chmod(&segment, 0o600);
    until("the spool drains", || spooled(home.path()).is_empty());
    stop(daemon);

    assert_eq!(held_on, 1, "the only copy was deleted: {lost_early}");
    assert_eq!(lost_early, "", "nothing is gone yet, and it is not over");
    assert_eq!(turns(home.path(), "p1"), 1);
    let after = drops(home.path());
    assert_eq!(after.lines().count(), 1, "one outage, one line: {after}");
    assert!(after.contains("the log refused writes"), "{after}");
}

#[test]
fn a_live_record_the_log_refuses_lands_once_when_it_stops() {
    let home = tempfile::tempdir().expect("tempdir");
    let segment = a_log_with_one_turn(home.path());
    chmod(&segment, 0o400);
    let daemon = start(home.path());

    let (reply, stderr) = prompt(home.path(), "p1", "the log refuses this one");

    let _: serde_json::Value = serde_json::from_str(reply.trim()).expect("one object");
    assert!(
        !stderr.contains("record dropped"),
        "it is not dropped: {stderr}"
    );
    chmod(&segment, 0o600);
    until("it lands", || turns(home.path(), "p1") == 1);
    until("the spool drains", || spooled(home.path()).is_empty());
    past_a_drain();
    stop(daemon);
    assert_eq!(
        turns(home.path(), "p1"),
        1,
        "one record, however many copies"
    );
}

#[test]
fn with_no_room_for_a_spool_file_the_daemon_keeps_the_record_in_memory() {
    // A really full disk refuses the spool file and the drop line too. The one
    // place left is the memory of the daemon that is holding the request.
    let home = tempfile::tempdir().expect("tempdir");
    let segment = a_log_with_one_turn(home.path());
    // A file where the directory should be: the hook cannot make a spool entry
    // here, and it would put a closed directory's mode back itself.
    let spool = home.path().join("spool");
    std::fs::write(&spool, "").expect("in the way");
    chmod(&segment, 0o400);
    let daemon = start(home.path());

    let (reply, stderr) = prompt(home.path(), "p1", "nowhere on disk to put this");
    prompt(home.path(), "p2", "nor this");
    let on_disk_too_early = spooled(home.path()).len();
    chmod(&segment, 0o600);
    std::fs::remove_file(&spool).expect("out of the way");
    until("both land", || {
        turns(home.path(), "p1") == 1 && turns(home.path(), "p2") == 1
    });
    past_a_drain();
    stop(daemon);

    let _: serde_json::Value = serde_json::from_str(reply.trim()).expect("one object");
    assert!(
        !stderr.contains("record dropped"),
        "it is not dropped: {stderr}"
    );
    assert_eq!(on_disk_too_early, 0);
    let order: Vec<String> = records(home.path())
        .iter()
        .filter(|record| record["event"] == "turn_started")
        .filter_map(|record| record["vendor_turn_id"].as_str().map(String::from))
        .collect();
    assert_eq!(order, ["p0", "p1", "p2"], "in the order they arrived");
    let landed = records(home.path())
        .into_iter()
        .find(|record| record["vendor_turn_id"] == "p1")
        .expect("p1");
    assert!(
        landed.get("spooled_at").is_some(),
        "it says when it first tried: {landed}"
    );
    assert!(
        drops(home.path()).contains("the log refused writes"),
        "and the outage leaves a trace once it can: {}",
        drops(home.path())
    );
}

#[test]
fn a_refused_entry_goes_with_a_drop_line_once_its_life_is_over() {
    let home = tempfile::tempdir().expect("tempdir");
    let segment = a_log_with_one_turn(home.path());
    prompt(home.path(), "p1", "refused for longer than anyone waits");
    prompt(home.path(), "p2", "written just now");
    chmod(&segment, 0o400);
    let mut entries = spooled(home.path());
    entries.sort();
    assert_eq!(entries.len(), 2);
    std::fs::File::options()
        .write(true)
        .open(&entries[0])
        .expect("open")
        .set_modified(SystemTime::now() - WEEK - Duration::from_secs(60))
        .expect("age it");

    let daemon = start(home.path());
    past_a_drain();
    let left = spooled(home.path());
    stop(daemon);
    chmod(&segment, 0o600);

    assert_eq!(left, vec![entries[1].clone()], "only the old one goes");
    let said = drops(home.path());
    assert!(said.contains("spooled hook refused"), "{said}");
    assert!(said.contains("turn_started for session"), "{said}");
}

#[test]
fn an_old_entry_the_log_will_take_is_written_not_expired() {
    // A laptop shut for a fortnight comes back with its spool. Age alone is not
    // a reason to lose anything.
    let home = tempfile::tempdir().expect("tempdir");
    a_log_with_one_turn(home.path());
    prompt(home.path(), "p1", "older than a week and fine");
    let entries = spooled(home.path());
    assert_eq!(entries.len(), 1);
    std::fs::File::options()
        .write(true)
        .open(&entries[0])
        .expect("open")
        .set_modified(SystemTime::now() - WEEK - WEEK)
        .expect("age it");

    let daemon = start(home.path());
    until("the spool drains", || spooled(home.path()).is_empty());
    stop(daemon);

    assert_eq!(turns(home.path(), "p1"), 1);
    assert_eq!(drops(home.path()), "");
}

#[test]
fn a_refused_entry_does_not_keep_the_others_from_a_log_that_comes_back() {
    let home = tempfile::tempdir().expect("tempdir");
    let segment = a_log_with_one_turn(home.path());
    for n in 0..5 {
        prompt(home.path(), &format!("s{n}"), "spooled while it was down");
    }
    chmod(&segment, 0o400);
    let daemon = start(home.path());
    past_a_drain();
    chmod(&segment, 0o600);
    until("the spool drains", || spooled(home.path()).is_empty());
    stop(daemon);

    let order: Vec<String> = records(home.path())
        .iter()
        .filter(|record| record["event"] == "turn_started")
        .filter_map(|record| record["vendor_turn_id"].as_str().map(String::from))
        .collect();
    assert_eq!(order, ["p0", "s0", "s1", "s2", "s3", "s4"]);
}

#[test]
fn a_short_write_leaves_no_torn_line_behind() {
    // A file-size limit makes a write stop partway, the way a nearly full disk
    // does, on every platform and with no disk image. The signal it would send
    // is ignored here, as `trap` passes on to what it runs.
    let home = tempfile::tempdir().expect("tempdir");
    a_log_with_one_turn(home.path());
    let segment = home.path().join("log/segment-00000000.jsonl");
    let at = std::fs::metadata(&segment).expect("meta").len();
    // `ulimit -f` counts 512-byte blocks: room for part of the next line.
    let blocks = at / 512 + 2;
    let limited = Command::new("/bin/sh")
        .args([
            "-c",
            "trap '' XFSZ; ulimit -f \"$1\"; exec \"$2\" daemon",
            "sh",
        ])
        .arg(blocks.to_string())
        .arg(bin())
        .env("CONBUS_HOME", home.path())
        .env("HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("daemon under a limit");
    let limited = Daemon(limited);
    let socket = home.path().join("conbus.sock");
    until("it serves", || {
        std::os::unix::net::UnixStream::connect(&socket).is_ok()
    });

    let long = "x".repeat(4 * 1024);
    prompt(home.path(), "p1", &long);
    stop(limited);

    assert!(
        std::fs::metadata(&segment).expect("meta").len() <= at + blocks * 512,
        "the limit did not hold"
    );
    let log = Command::new(bin())
        .args(["log", "--json"])
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .output()
        .expect("log");
    let stderr = String::from_utf8_lossy(&log.stderr);
    assert!(!stderr.contains("unreadable"), "a torn line: {stderr}");
}

#[test]
fn a_torn_final_line_costs_only_itself() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    prompt(home.path(), "p0", "before the crash");
    stop(daemon);
    let segment = home.path().join("log/segment-00000000.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&segment)
        .expect("open");
    file.write_all(br#"{"v":3,"ts":"2026-11-01T00:00:00.000Z","seq":1,"prompt":{"text":""#)
        .expect("tear");
    file.write_all(&[0xEC]).expect("tear");
    drop(file);

    let daemon = start(home.path());
    prompt(home.path(), "p1", "after the crash");
    stop(daemon);

    let all = records(home.path());
    let started: Vec<&serde_json::Value> = all
        .iter()
        .filter(|record| record["event"] == "turn_started")
        .collect();
    assert_eq!(started.len(), 2);
    assert_eq!(started[0]["bus_id"], started[1]["bus_id"]);
    let log = Command::new(bin())
        .args(["log", "--json"])
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .output()
        .expect("log");
    assert_eq!(
        String::from_utf8_lossy(&log.stderr)
            .matches("unreadable")
            .count(),
        1
    );
}
