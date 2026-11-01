//! A hook that cannot reach the daemon spools its request beside the socket
//! and the next daemon drains it — zero loss across restarts.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

fn run_hook(home: &Path, payload: &str) -> (i32, String) {
    let mut child = Command::new(bin())
        .arg("hook")
        .arg("--harness")
        .arg("claude_code")
        .env("CONBUS_HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn prompt(text: &str) -> String {
    serde_json::json!({
        "session_id": "s-spool",
        "hook_event_name": "UserPromptSubmit",
        "prompt": text,
        "prompt_id": "p-1"
    })
    .to_string()
}

fn stamp(home: &Path, retention: &str) {
    std::fs::write(
        home.join("daemon.build"),
        serde_json::json!({
            "build": "test", "pid": 1, "started": "2026-11-01T00:00:00.000Z",
            "retention": retention
        })
        .to_string(),
    )
    .expect("stamp");
}

fn spool_files(home: &Path) -> Vec<std::path::PathBuf> {
    let dir = home.join("spool");
    std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_hook_with_no_daemon_spools_and_still_answers_silently() {
    let home = tempfile::tempdir().expect("tempdir");
    stamp(home.path(), "conversation_only");
    let (code, stdout) = run_hook(home.path(), &prompt("the secret turn"));
    assert_eq!(code, 0, "the turn still gets a clean reply");
    let reply: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one object");
    assert!(reply.is_object());
    let files = spool_files(home.path());
    assert_eq!(files.len(), 1, "one request spooled: {files:?}");
    let spooled: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&files[0]).expect("read")).expect("json");
    assert!(spooled.get("hid").is_some(), "the request names its id");
    assert!(spooled.get("spooled").is_some(), "the send time is set");
    // conversation_only: tool bodies would be elided; a prompt stays text.
}

#[test]
fn without_a_stamp_nothing_is_spooled() {
    let home = tempfile::tempdir().expect("tempdir");
    let (code, stdout) = run_hook(home.path(), &prompt("lost?"));
    assert_eq!(code, 0);
    let _: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one object");
    assert!(spool_files(home.path()).is_empty());
    assert!(
        !home.path().join("spool").exists(),
        "no stamp, no spool directory"
    );
}

#[test]
fn ephemeral_retention_never_spools() {
    let home = tempfile::tempdir().expect("tempdir");
    stamp(home.path(), "ephemeral");
    let (code, _) = run_hook(home.path(), &prompt("lost?"));
    assert_eq!(code, 0);
    assert!(spool_files(home.path()).is_empty());
}

#[test]
fn a_full_spool_takes_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    stamp(home.path(), "full");
    let dir = home.path().join("spool");
    std::fs::create_dir(&dir).expect("dir");
    let filler = vec![b'x'; 16 * 1024 * 1024];
    std::fs::write(dir.join("filler.json"), filler).expect("fill");
    let (code, _) = run_hook(home.path(), &prompt("dropped by the cap"));
    assert_eq!(code, 0);
    assert_eq!(
        spool_files(home.path()).len(),
        1,
        "only the filler remains — the cap refused the new entry"
    );
}

#[test]
fn the_daemon_drains_the_spool_it_finds() {
    let home = tempfile::tempdir().expect("tempdir");
    stamp(home.path(), "full");
    let (code, _) = run_hook(home.path(), &prompt("waiting for the restart"));
    assert_eq!(code, 0);
    assert_eq!(spool_files(home.path()).len(), 1);

    let mut daemon = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .env("HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut recorded = false;
    while Instant::now() < deadline {
        if spool_files(home.path()).is_empty() {
            let scan = Command::new(bin())
                .arg("log")
                .arg("--tail")
                .arg("1")
                .arg("--json")
                .env("CONBUS_HOME", home.path())
                .env("CONBUS_SKIP_LAUNCHCTL", "1")
                .output()
                .expect("log");
            let text = String::from_utf8_lossy(&scan.stdout);
            if text.contains("waiting for the restart") {
                recorded = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert!(recorded, "the spooled prompt reached the log after drain");
}

#[test]
fn a_newer_protocol_entry_stays_spooled() {
    let home = tempfile::tempdir().expect("tempdir");
    stamp(home.path(), "full");
    let dir = home.path().join("spool");
    std::fs::create_dir(&dir).expect("dir");
    let future = serde_json::json!({
        "pv": 99, "harness": "claude_code", "vendor_session_id": "s-new",
        "hid": "01a19463-cbd4-7b50-b819-1c9ecf85ef18",
        "event": {"event": "session_candidate"}
    });
    std::fs::write(
        dir.join("01a19463-cbd4-7b50-b819-1c9ecf85ef18.json"),
        future.to_string(),
    )
    .expect("write");

    let mut daemon = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .env("HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    std::thread::sleep(Duration::from_secs(3));
    let files = spool_files(home.path());
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert_eq!(
        files.len(),
        1,
        "a newer protocol stays for a daemon that speaks it"
    );
}
