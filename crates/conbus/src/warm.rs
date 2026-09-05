//! The Stage 0 Codex warm leg.
//!
//! The bus opens a thread through `codex app-server` and writes history into
//! it with `thread/inject_items`. That call is not a model turn. The thread
//! is ours from the first RPC — we never pass a thread id the user minted.
//! Syncing a later digest requires `thread/resume` first: inject targets a
//! *loaded* thread, and a fresh app-server has not loaded yesterday's.
//!
//! This is not on the hook path. Starting an app-server is ~15s-bounded and
//! would stall a prompt. `conbus hop --apply` is the writer.

use std::io;

use serde_json::json;

use crate::appserver::AppServer;
use crate::quota::DEADLINE;

/// How a `leased.holder` names a thread we opened.
pub const HOLDER_PREFIX: &str = "warm:";

pub fn holder(thread_id: &str) -> String {
    format!("{HOLDER_PREFIX}{thread_id}")
}

pub fn thread_id(holder: &str) -> Option<&str> {
    holder
        .strip_prefix(HOLDER_PREFIX)
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
    let mut server = AppServer::start(DEADLINE)?;
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
    let mut server = AppServer::start(DEADLINE)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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

    static ENV: Mutex<()> = Mutex::new(());

    fn fake() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("codex");
        std::fs::write(&path, FAKE).expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        (dir, path)
    }

    struct Env {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl Env {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Env { key, previous }
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn a_holder_round_trips_and_a_bare_id_is_not_ours() {
        assert_eq!(thread_id(&holder("thr_1")), Some("thr_1"));
        assert_eq!(thread_id("thr_1"), None);
        assert_eq!(thread_id("warm:"), None);
    }

    #[test]
    fn opening_a_warm_thread_injects_without_starting_a_turn() {
        let _lock = lock_env();
        let (_dir, codex) = fake();
        let _env = Env::set("CONBUS_CODEX_BIN", codex.to_str().unwrap());
        let thread = open(Some("/repo"), "conbus digest").expect("open");
        assert_eq!(thread.id, "thr_bus_owned");
    }

    #[test]
    fn syncing_resumes_the_thread_we_opened() {
        let _lock = lock_env();
        let (_dir, codex) = fake();
        let _env = Env::set("CONBUS_CODEX_BIN", codex.to_str().unwrap());
        sync("thr_bus_owned", "newer digest").expect("sync");
    }

    #[test]
    fn a_user_named_thread_is_refused() {
        let err = sync("user:abc", "no").expect_err("must refuse");
        assert!(err.to_string().contains("did not open"), "{err}");
    }
}
