//! By the time a spooled request is drained, the hook that sent it has already
//! answered and exited. Whatever the daemon says back reaches nobody, so
//! nothing may be recorded or acted on as though it had.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const EARLIER: &str = "9b24e37f-0000-4000-8000-000000000001";
const NEW: &str = "437e9522-0000-4000-8000-000000000002";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

fn start(home: &Path) -> Child {
    let mut child = Command::new(bin())
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

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn hook(home: &Path, payload: &serde_json::Value) -> String {
    let mut child = Command::new(bin())
        .args(["hook", "--harness", "claude_code"])
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn prompt(session: &str, id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": session, "hook_event_name": "UserPromptSubmit",
        "prompt": text, "prompt_id": id, "cwd": "/repo"
    })
}

fn ended(session: &str, id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": session, "hook_event_name": "Stop",
        "last_assistant_message": text, "prompt_id": id, "cwd": "/repo"
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
fn a_drained_first_prompt_does_not_record_a_briefing_nobody_received() {
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &prompt(EARLIER, "e0", "earlier work"));
    hook(home.path(), &ended(EARLIER, "e0", "done"));
    hook(home.path(), &prompt(EARLIER, "e1", "more work"));
    hook(home.path(), &ended(EARLIER, "e1", "done again"));
    stop(daemon);

    let reply = hook(
        home.path(),
        &prompt(NEW, "n0", "a new session while the recorder is down"),
    );
    assert!(
        !reply.contains("additionalContext"),
        "the hook answered with no daemon to ask: {reply}"
    );

    let daemon = start(home.path());
    drain(home.path());
    stop(daemon);

    let all = records(home.path());
    assert!(
        all.iter().any(|record| record["vendor_turn_id"] == "n0"),
        "the prompt itself is recorded"
    );
    let briefed = all
        .iter()
        .filter(|record| record["event"] == "briefed")
        .count();
    assert_eq!(briefed, 0, "a briefing nobody received");
}

#[test]
fn the_control_a_live_first_prompt_is_still_briefed() {
    // Without this the test above would pass on a build that stopped briefing
    // altogether.
    let home = tempfile::tempdir().expect("tempdir");
    let daemon = start(home.path());
    hook(home.path(), &prompt(EARLIER, "e0", "earlier work"));
    hook(home.path(), &ended(EARLIER, "e0", "done"));
    hook(home.path(), &prompt(EARLIER, "e1", "more work"));
    hook(home.path(), &ended(EARLIER, "e1", "done again"));

    let reply = hook(home.path(), &prompt(NEW, "n0", "a new session"));
    stop(daemon);

    assert!(reply.contains("additionalContext"), "{reply}");
    let briefed = records(home.path())
        .iter()
        .filter(|record| record["event"] == "briefed")
        .count();
    assert_eq!(briefed, 1);
}
