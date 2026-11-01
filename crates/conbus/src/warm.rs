//! The warm legs.
//!
//! Codex: the bus opens a thread through `codex app-server` and writes history
//! with `thread/inject_items`. That call is not a model turn. The thread is
//! ours from the first RPC — we never pass a thread id the user minted.
//!
//! Claude: the bus opens a headless session with `--session-id` we minted and
//! `--max-turns 0`, which is the CLI form of `shouldQuery: false`. Hop-back
//! is `--resume` of that session, not a new mint. Permissions are
//! `--permission-prompts none` (auto-deny), not skip-all.
//!
//! This is not on the hook path. `conbus hop --apply` is the writer.

use std::io;
use std::process::{Command, Stdio};

use serde_json::json;

use crate::appserver::{self, AppServer};
use crate::hook::client::INTERNAL_MARKER;
use crate::quota::DEADLINE;

/// How a `leased.holder` names a thread we opened.
pub const HOLDER_PREFIX: &str = "warm:";

pub fn holder(thread_id: &str) -> String {
    format!("{HOLDER_PREFIX}{thread_id}")
}

pub fn thread_id(holder: &str) -> Option<&str> {
    if claude_id(holder).is_some() {
        return None;
    }
    holder
        .strip_prefix(HOLDER_PREFIX)
        .filter(|id| !id.is_empty())
}

/// How a `leased.holder` names a Claude session we opened.
pub const CLAUDE_PREFIX: &str = "warm-claude:";

pub fn claude_holder(session_id: &str) -> String {
    format!("{CLAUDE_PREFIX}{session_id}")
}

pub fn claude_id(holder: &str) -> Option<&str> {
    holder
        .strip_prefix(CLAUDE_PREFIX)
        .filter(|id| !id.is_empty())
}

/// A Codex thread the bus started.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Thread {
    pub id: String,
}

/// Open a new thread and inject `text` as an assistant message.
///
/// `cwd` is the checkout being continued, when we know it. Codex assigns the
/// id; we do not send one.
pub fn open(cwd: Option<&str>, text: &str) -> io::Result<Thread> {
    open_with(&appserver::codex_binary(), cwd, text)
}

/// As [`open`], with the app-server binary named rather than resolved from
/// the environment — see [`AppServer::start_with`] for why tests must.
fn open_with(binary: &std::ffi::OsStr, cwd: Option<&str>, text: &str) -> io::Result<Thread> {
    let mut server = AppServer::start_with(binary, DEADLINE)?;
    let mut params = json!({});
    if let Some(cwd) = cwd {
        params["cwd"] = json!(cwd);
    }
    let result = server.call("thread/start", params)?;
    let id = result
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(|id| id.as_str())
        .ok_or_else(|| io::Error::other(format!("thread/start returned no id: {result}")))?
        .to_string();
    inject(&mut server, &id, text)?;
    Ok(Thread { id })
}

/// Load a thread we opened and inject a newer digest. Not a user thread:
/// the caller must have taken the id from [`holder`].
pub fn sync(thread_id: &str, text: &str) -> io::Result<()> {
    if thread_id.is_empty() || thread_id.contains("user:") {
        return Err(io::Error::other(
            "refusing to inject into a thread the bus did not open",
        ));
    }
    sync_with(&appserver::codex_binary(), thread_id, text)
}

fn sync_with(binary: &std::ffi::OsStr, thread_id: &str, text: &str) -> io::Result<()> {
    let mut server = AppServer::start_with(binary, DEADLINE)?;
    server.call("thread/resume", json!({ "threadId": thread_id }))?;
    inject(&mut server, thread_id, text)
}

fn inject(server: &mut AppServer, thread_id: &str, text: &str) -> io::Result<()> {
    server.call(
        "thread/inject_items",
        json!({
            "threadId": thread_id,
            "items": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text }]
            }]
        }),
    )?;
    Ok(())
}

fn claude_binary() -> std::ffi::OsString {
    std::env::var_os("CONBUS_CLAUDE_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "claude".into())
}

/// Open a headless Claude session the bus owns. Not a model turn.
pub fn open_claude(cwd: Option<&str>, text: &str) -> io::Result<Thread> {
    open_claude_with(&claude_binary(), cwd, text)
}

/// As [`open_claude`], with the binary named rather than read from the
/// environment. The environment is process-global and `cargo test` runs tests
/// in threads, so a test that pointed `CONBUS_CLAUDE_BIN` at its own stub
/// could be answered by another module's — which is what happened here and in
/// `runner`, under two separate locks.
fn open_claude_with(binary: &std::ffi::OsStr, cwd: Option<&str>, text: &str) -> io::Result<Thread> {
    let minted = conbus_core::ids::BusId::mint().to_string();
    let got = run_claude(binary, cwd, Some(&minted), None, text)?;
    Ok(Thread {
        id: got.unwrap_or(minted),
    })
}

/// Write a newer digest into a Claude session we opened. Still not a model turn.
pub fn sync_claude(session_id: &str, text: &str) -> io::Result<()> {
    sync_claude_with(&claude_binary(), session_id, text)
}

fn sync_claude_with(binary: &std::ffi::OsStr, session_id: &str, text: &str) -> io::Result<()> {
    if session_id.is_empty() || session_id.contains("user:") {
        return Err(io::Error::other(
            "refusing to inject into a Claude session the bus did not open",
        ));
    }
    run_claude(binary, None, None, Some(session_id), text).map(|_| ())
}

fn run_claude(
    binary: &std::ffi::OsStr,
    cwd: Option<&str>,
    session_id: Option<&str>,
    resume: Option<&str>,
    text: &str,
) -> io::Result<Option<String>> {
    let mut cmd = Command::new(binary);
    cmd.args([
        "-p",
        "--max-turns",
        "0",
        "--output-format",
        "json",
        "--permission-prompts",
        "none",
        "--append-system-prompt",
        text,
    ]);
    if let Some(id) = session_id {
        cmd.args(["--session-id", id]);
    }
    if let Some(id) = resume {
        cmd.args(["--resume", id]);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.env(INTERNAL_MARKER, "1")
        .env("PATH", appserver::path_for_vendor())
        .stdin(Stdio::null())
        .arg("conbus warm");
    let output = cmd.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let got = serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|value| {
            value
                .get("session_id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    if got.is_some() || stdout.contains("session_id") || stdout.contains("\"type\":\"system\"") {
        return Ok(got);
    }
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "claude warm failed: {} {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = r#"#!/usr/bin/env python3
import json, sys
loaded = None
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    m = msg.get("method"); i = msg.get("id")
    if i is None:
        continue
    if m == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"userAgent": "fake"}}), flush=True)
    elif m == "thread/start":
        loaded = "thr_bus_owned"
        print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"thread": {
            "id": loaded, "sessionId": loaded
        }}}), flush=True)
    elif m == "thread/resume":
        loaded = msg["params"]["threadId"]
        print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"thread": {"id": loaded}}}), flush=True)
    elif m == "thread/inject_items":
        if loaded is None:
            print(json.dumps({"jsonrpc": "2.0", "id": i, "error": {"code": -32000, "message": "thread not loaded"}}), flush=True)
        elif msg["params"]["threadId"] != loaded:
            print(json.dumps({"jsonrpc": "2.0", "id": i, "error": {"code": -32000, "message": "not the loaded thread"}}), flush=True)
        else:
            items = msg["params"]["items"]
            assert items[0]["type"] == "message"
            assert items[0]["role"] == "assistant"
            assert items[0]["content"][0]["type"] == "output_text"
            print(json.dumps({"jsonrpc": "2.0", "id": i, "result": {}}), flush=True)
"#;

    /// A stand-in `codex app-server` in its own directory.
    fn fake() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("codex");
        std::fs::write(&path, FAKE).expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        runs_when_nobody_holds_it(&path);
        (dir, path)
    }

    /// A test thread that forks while this file is being written holds it open
    /// for writing until its child execs, and until then Linux refuses to run it
    /// with "Text file busy". Once it has run, nothing is still holding it.
    fn runs_when_nobody_holds_it(path: &std::path::Path) {
        while let Err(err) = Command::new(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            assert_eq!(err.raw_os_error(), Some(libc::ETXTBSY), "{err}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_holder_round_trips_and_a_bare_id_is_not_ours() {
        assert_eq!(thread_id(&holder("thr_1")), Some("thr_1"));
        assert_eq!(thread_id("thr_1"), None);
        assert_eq!(thread_id("warm:"), None);
        assert_eq!(claude_id(&claude_holder("sess_1")), Some("sess_1"));
        assert_eq!(thread_id(&claude_holder("sess_1")), None);
    }

    #[test]
    fn opening_a_warm_thread_injects_without_starting_a_turn() {
        let (_dir, codex) = fake();
        let thread = open_with(codex.as_os_str(), Some("/repo"), "conbus digest").expect("open");
        assert_eq!(thread.id, "thr_bus_owned");
    }

    #[test]
    fn syncing_resumes_the_thread_we_opened() {
        let (_dir, codex) = fake();
        sync_with(codex.as_os_str(), "thr_bus_owned", "newer digest").expect("sync");
    }

    #[test]
    fn a_user_named_thread_is_refused() {
        let err = sync("user:abc", "no").expect_err("must refuse");
        assert!(err.to_string().contains("did not open"), "{err}");
    }

    /// A stand-in `claude` that records the argv it was given. The log path
    /// is written into the script rather than read from the environment, so
    /// two of these can run at once without meeting.
    const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, sys
args = sys.argv[1:]
sid = ""
if "--session-id" in args:
    sid = args[args.index("--session-id") + 1]
elif "--resume" in args:
    sid = args[args.index("--resume") + 1]
open("__LOG__", "w").write(" ".join(args))
print(json.dumps({"type": "result", "session_id": sid, "is_error": True}))
sys.exit(1)
"#;

    /// A fake `claude` in its own directory, and the file it logs argv to.
    fn fake_claude() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("claude");
        let log = dir.path().join("argv");
        crate::test_support::write_executable(
            &path,
            &FAKE_CLAUDE.replace("__LOG__", log.to_str().expect("utf-8")),
        );
        (dir, path, log)
    }

    #[test]
    fn opening_a_claude_warm_uses_max_turns_zero() {
        let (dir, claude, log) = fake_claude();
        let cwd = dir.path().to_str().unwrap();
        let thread =
            open_claude_with(claude.as_os_str(), Some(cwd), "conbus digest").expect("open");
        assert!(!thread.id.is_empty());
        let argv = std::fs::read_to_string(&log).expect("argv");
        assert!(argv.contains("--max-turns 0"), "{argv}");
        assert!(argv.contains("--permission-prompts none"), "{argv}");
        assert!(
            argv.contains(&format!("--session-id {}", thread.id)),
            "{argv}"
        );
        assert!(!argv.contains("--dangerously-skip-permissions"), "{argv}");
    }

    #[test]
    fn syncing_a_claude_warm_resumes_the_session_we_opened() {
        let (_dir, claude, log) = fake_claude();
        sync_claude_with(claude.as_os_str(), "sess_bus_owned", "newer digest").expect("sync");
        let argv = std::fs::read_to_string(&log).expect("argv");
        assert!(argv.contains("--resume sess_bus_owned"), "{argv}");
        assert!(argv.contains("--max-turns 0"), "{argv}");
    }

    #[test]
    fn two_claude_warms_opened_at_once_each_get_their_own_binary() {
        // The regression this seam exists for: both of these used to point
        // the process-global `CONBUS_CLAUDE_BIN` at their own stub, and this
        // module's lock did not stop `runner` from doing the same under its
        // own. Running the two modules together failed five times in eight.
        let opened: Vec<_> = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                std::thread::spawn(move || {
                    let (dir, claude, log) = fake_claude();
                    let cwd = dir.path().to_str().expect("utf-8").to_string();
                    let thread =
                        open_claude_with(claude.as_os_str(), Some(&cwd), name).expect("open");
                    (thread.id, std::fs::read_to_string(&log).expect("argv"))
                })
            })
            .collect();

        let seen: Vec<(String, String)> = opened
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();

        for (id, argv) in &seen {
            assert!(
                argv.contains(&format!("--session-id {id}")),
                "a warm was answered by the other thread's binary: {seen:?}"
            );
        }
        assert_ne!(seen[0].0, seen[1].0, "each warm mints its own session");
    }

    #[test]
    fn a_user_named_claude_session_is_refused() {
        let err = sync_claude("user:abc", "no").expect_err("must refuse");
        assert!(err.to_string().contains("did not open"), "{err}");
    }
}
