//! `conbus backfill` recovering Codex history through a stand-in app-server
//! that serves the thread APIs the real one answered on 2026-09-03.

use std::process::Command;

const FAKE_APP_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys
T1 = "01a06316-03dc-7c31-abae-aa2c2606cdba"   # nobody recorded this one
T2 = "01a06307-cc6a-7702-8533-d02e159b951d"   # the recorder saw this one live
def thread(i, cwd):
    return {"id": i, "sessionId": i, "cwd": cwd, "name": "n", "preview": "p", "createdAt": "1788368782",
            "updatedAt": "1788368799", "historyMode": "paginated", "source": "cli", "cliVersion": "0.152.1",
            "status": "completed", "turns": []}
def turns(i):
    return [{"id": i[:8] + "-turn-1", "items": [
        {"type": "userMessage", "id": "u1", "content": [{"type": "text", "text": "prompt for " + i[:8]}]},
        {"type": "agentMessage", "id": "a1", "text": "answer for " + i[:8], "phase": "final_answer"}],
        "status": "completed", "error": None, "startedAt": 1788368796, "completedAt": 1788368799, "durationMs": 3000}]
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"userAgent": "fake"}}), flush=True)
    elif m == "thread/list":
        page = msg["params"].get("cursor")
        if page is None:
            print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"data": [thread(T1, "/repo")], "nextCursor": "page2"}}), flush=True)
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"data": [thread(T2, "/repo")], "nextCursor": None}}), flush=True)
    elif m == "thread/turns/list":
        print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"data": turns(msg["params"]["threadId"]), "nextCursor": None}}), flush=True)
"#;

fn fake_app_server() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("codex");
    std::fs::write(&path, FAKE_APP_SERVER).expect("write");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (dir, path)
}

fn log_lines(home: &std::path::Path) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(home.join("log")) else {
        return out;
    };
    let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
            if !line.is_empty() {
                out.push(serde_json::from_str(line).expect("json"));
            }
        }
    }
    out
}

#[test]
fn codex_history_is_recovered_through_the_app_server_and_a_live_session_is_left_alone() {
    let (_fake_dir, codex) = fake_app_server();
    let home = tempfile::tempdir().expect("tempdir");
    // The recorder saw T2 as it happened: one live record under the thread's
    // own session id, which is also its thread id.
    let mut log = conbus_core::segment::SegmentLog::open(
        home.path().join("log"),
        conbus_core::retention::Retention::default(),
    )
    .expect("open");
    log.append(conbus_core::record::Entry {
        bus_id: conbus_core::ids::BusId::mint(),
        observed_at: None,
        source: conbus_core::event::Source {
            harness: conbus_core::event::Harness::new(conbus_core::event::Harness::CODEX),
            harness_version: None,
            vendor_session_id: Some("01a06307-cc6a-7702-8533-d02e159b951d".into()),
            workspace: Some("/repo".into()),
            entrypoint: None,
            backfilled_from: None,
            profile: None,
        },
        vendor_turn_id: None,
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
        event: conbus_core::event::Event::TurnStarted {
            author: None,
            prompt: conbus_core::event::Body::kept("seen live"),
        },
    })
    .expect("append");
    drop(log);

    let output = Command::new(env!("CARGO_BIN_EXE_conbus"))
        .arg("backfill")
        .arg("--apply")
        .env("CONBUS_HOME", home.path())
        // No Claude transcripts here, so only the Codex source has anything.
        .env("HOME", home.path())
        .env("CONBUS_CODEX_BIN", &codex)
        .output()
        .expect("run backfill");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}{stdout}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = log_lines(home.path());
    let codex_lines: Vec<_> = lines
        .iter()
        .filter(|l| l["harness"] == "codex" && l["backfilled_from"] == "codex_thread")
        .collect();
    let kinds: Vec<&str> = codex_lines
        .iter()
        .map(|l| l["event"].as_str().expect("event"))
        .collect();
    assert_eq!(
        kinds,
        vec!["session_candidate", "turn_started", "turn_ended"],
        "one thread recovered whole, the live one left alone: {stdout}\n{lines:#?}"
    );
    assert!(
        codex_lines
            .iter()
            .all(|l| l["bus_id"] == "01a06316-03dc-7c31-abae-aa2c2606cdba"),
        "the thread's own id is the conversation's id: {codex_lines:#?}"
    );
    assert_eq!(codex_lines[1]["prompt"]["text"], "prompt for 01a06316");
    assert_eq!(
        codex_lines[2]["assistant_message"]["text"],
        "answer for 01a06316"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l["bus_id"] == "01a06307-cc6a-7702-8533-d02e159b951d"),
        "a copy of the live session would be a twin: {lines:#?}"
    );
    assert!(
        stdout.contains("codex"),
        "the report names the source: {stdout}"
    );
}

#[test]
fn a_missing_codex_does_not_stop_the_claude_history_from_being_recovered() {
    let home = tempfile::tempdir().expect("tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_conbus"))
        .arg("backfill")
        .env("CONBUS_HOME", home.path())
        .env("HOME", home.path())
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .output()
        .expect("run backfill");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("codex") && stdout.contains("unavailable"),
        "a source that cannot be read is reported, not fatal: {stdout}"
    );
}
