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

/// The storm: 1,000 hooks across 16 threads, with the daemon TERMed and
/// restarted mid-burst. Every event must reach the log — spooled during the
/// gap or answered live — and a re-delivered hook_id counts once.
#[test]
fn a_thousand_hooks_survive_a_daemon_restart() {
    const HOOKS: u64 = 1000;
    const SESSIONS: u64 = 5;

    let home = tempfile::tempdir().expect("tempdir");
    let daemon = std::sync::Mutex::new(
        Command::new(bin())
            .arg("daemon")
            .env("CONBUS_HOME", home.path())
            .env("HOME", home.path())
            .env("CONBUS_SKIP_LAUNCHCTL", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("daemon"),
    );
    // Mint the sessions first so the storm's events land in live
    // conversations rather than all orphans.
    wait_for_socket(home.path());
    for s in 0..SESSIONS {
        let (code, _) = run_hook(
            home.path(),
            &serde_json::json!({
                "session_id": format!("s-storm-{s}"),
                "hook_event_name": "UserPromptSubmit",
                "prompt": "minting the session",
                "prompt_id": format!("mint-{s}")
            })
            .to_string(),
        );
        assert_eq!(code, 0);
    }

    // The storm: every hook invocation is its own process, the way a vendor
    // fires it.
    let sent = std::sync::atomic::AtomicU64::new(0);
    let failures = std::sync::Mutex::new(Vec::<String>::new());
    let killed = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                loop {
                    let n = sent.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if n >= HOOKS {
                        return;
                    }
                    // Mid-burst: kill the daemon once and restart it.
                    if n == 300 && !killed.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        let mut handle = daemon.lock().expect("daemon");
                        let _ = handle.kill();
                        let _ = handle.wait();
                        if let Ok(new_daemon) = Command::new(bin())
                            .arg("daemon")
                            .env("CONBUS_HOME", home.path())
                            .env("HOME", home.path())
                            .env("CONBUS_SKIP_LAUNCHCTL", "1")
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()
                        {
                            *handle = new_daemon;
                        }
                    }
                    let session = n % SESSIONS;
                    let payload = serde_json::json!({
                        "session_id": format!("s-storm-{session}"),
                        "hook_event_name": "PreToolUse",
                        "tool_name": "Bash",
                        "tool_input": {"command": format!("echo storm-{n}")},
                        "tool_use_id": format!("storm-{n}")
                    })
                    .to_string();
                    let (code, stdout) = run_hook(home.path(), &payload);
                    let object = serde_json::from_str::<serde_json::Value>(stdout.trim());
                    if code != 0 || object.is_err() || !object.expect("checked").is_object() {
                        failures
                            .lock()
                            .expect("failures")
                            .push(format!("hook {n}: exit {code} out {stdout:?}"));
                    }
                }
            });
        }
    });
    assert!(
        failures.lock().expect("failures").is_empty(),
        "hooks that did not answer cleanly: {:?}",
        failures.lock().expect("failures")
    );

    // The new daemon drains the spool before it serves and on its idle tick.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !spool_files(home.path()).is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        spool_files(home.path()).is_empty(),
        "the spool never drained"
    );

    // Every hook id is distinct, and every storm event is in the log.
    let output = Command::new(bin())
        .arg("log")
        .arg("--json")
        .arg("--grep")
        .arg("storm-")
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .output()
        .expect("log");
    let mut hook_ids = std::collections::HashSet::new();
    let mut markers = std::collections::HashSet::new();
    let mut dup_hook_ids = 0u64;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let record: serde_json::Value = serde_json::from_str(line).expect("record");
        if let Some(h) = record.get("hook_id").and_then(|h| h.as_str())
            && !hook_ids.insert(h.to_string())
        {
            dup_hook_ids += 1;
        }
        if let Some(id) = record
            .get("tool_use_id")
            .and_then(|t| t.as_str())
            .map(String::from)
            .or_else(|| {
                record
                    .get("input")
                    .and_then(|i| serde_json::to_string(i).ok())
                    .filter(|t| t.contains("storm-"))
            })
        {
            markers.insert(id);
        }
    }
    if let Ok(mut handle) = daemon.lock() {
        let _ = handle.kill();
        let _ = handle.wait();
    }
    assert_eq!(
        hook_ids.len() as u64,
        HOOKS,
        "every hook's id must reach the log exactly once ({dup_hook_ids} re-deliveries)"
    );
    assert_eq!(markers.len() as u64, HOOKS, "every storm event was written");
}

fn wait_for_socket(home: &Path) {
    let socket = home.join("conbus.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the daemon never answered its socket");
}

#[test]
fn a_link_in_the_spool_is_not_followed() {
    // Only a regular file a hook wrote is a spooled request. A link is dropped
    // unread, so the daemon never reads whatever it points at.
    let home = tempfile::tempdir().expect("tempdir");
    stamp(home.path(), "full");
    let (code, _) = run_hook(home.path(), &prompt("behind a link"));
    assert_eq!(code, 0);
    let spooled = spool_files(home.path()).pop().expect("one entry");
    let elsewhere = home.path().join("elsewhere.json");
    std::fs::rename(&spooled, &elsewhere).expect("move");
    std::os::unix::fs::symlink(&elsewhere, &spooled).expect("link");

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
    while Instant::now() < deadline && !spool_files(home.path()).is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    let drained = spool_files(home.path()).is_empty();
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert!(drained, "the link was not taken out of the spool");
    let log = Command::new(bin())
        .args(["log", "--json"])
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .output()
        .expect("log");
    assert!(
        !String::from_utf8_lossy(&log.stdout).contains("behind a link"),
        "the daemon followed a link out of the spool"
    );
    assert!(
        elsewhere.exists(),
        "the link's target is not the spool's to delete"
    );
}
