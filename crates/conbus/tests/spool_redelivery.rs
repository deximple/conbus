//! A daemon can write a record and die before it replies, or before it deletes
//! the spool file it drained. The hook id on the record is what stops the next
//! daemon writing it a second time, and that has to outlive the daemon.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SESSION: &str = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";

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

fn prompt(home: &Path, id: &str, text: &str) {
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
    child.wait().expect("wait");
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

fn spooled(home: &Path) -> usize {
    std::fs::read_dir(home.join("spool"))
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .count()
        })
        .unwrap_or(0)
}

fn drain(home: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && spooled(home) > 0 {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_hook_id_already_on_disk_is_not_written_again_by_a_new_daemon() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    prompt(home.path(), "p0", "first");
    prompt(
        home.path(),
        "p1",
        "written, then the daemon died before replying",
    );
    stop(daemon);
    // What the hook leaves when the daemon wrote the record and died before the
    // reply reached it: the same request, under the same id, in the spool.
    let id = records(home.path())
        .into_iter()
        .find(|record| record["vendor_turn_id"] == "p1")
        .expect("the record")["hook_id"]
        .as_str()
        .expect("a hook id")
        .to_string();
    std::fs::create_dir_all(home.path().join("spool")).expect("spool");
    let request = serde_json::json!({
        "pv": 6, "harness": "claude_code", "vendor_session_id": SESSION,
        "vendor_turn_id": "p1", "workspace": "/repo", "hid": id,
        "spooled": "2026-11-01T00:00:00.000Z",
        "event": {"event": "turn_started", "prompt": {
            "kind": "kept", "text": "written, then the daemon died before replying"
        }}
    });
    std::fs::write(
        home.path().join("spool").join(format!("{id}.json")),
        request.to_string(),
    )
    .expect("spool entry");

    let daemon = start(home.path());
    drain(home.path());
    stop(daemon);

    let copies = records(home.path())
        .into_iter()
        .filter(|record| record["vendor_turn_id"] == "p1")
        .count();
    assert_eq!(copies, 1, "the new daemon wrote it a second time");
    assert_eq!(spooled(home.path()), 0, "the spool entry was answered");
}

#[test]
fn a_spooled_request_the_log_does_not_hold_is_still_written() {
    // The other half: remembering ids must not swallow a real delivery.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    prompt(home.path(), "p0", "first");
    stop(daemon);
    prompt(home.path(), "p1", "spooled while nothing was listening");
    assert_eq!(spooled(home.path()), 1);

    let daemon = start(home.path());
    drain(home.path());
    stop(daemon);

    let copies = records(home.path())
        .into_iter()
        .filter(|record| record["vendor_turn_id"] == "p1")
        .count();
    assert_eq!(copies, 1);
}
