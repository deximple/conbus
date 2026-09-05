//! A client for `codex app-server`: JSON-RPC over the child's stdio.
//!
//! One fresh server per use, stopped afterwards. Quota and backfill only
//! read. The warm leg writes, and only into a thread we opened with
//! `thread/start` — never into one the user minted.

use std::io::{self, BufRead, BufReader, Lines, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Which binary answers. `CONBUS_CODEX_BIN` overrides `codex` on `PATH`, for
/// tests and for a Codex installed somewhere the daemon's `PATH` does not see.
pub fn codex_binary() -> std::ffi::OsString {
    std::env::var_os("CONBUS_CODEX_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "codex".into())
}

pub struct AppServer {
    child: Arc<Mutex<Child>>,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl AppServer {
    /// Start a server and complete the `initialize` handshake.
    ///
    /// `deadline` bounds the whole conversation, not one call: what stalls an
    /// app-server is the MCP servers it tries to start, not any question of
    /// ours, and a watchdog that stops the process unblocks whatever read is
    /// waiting on it.
    pub fn start(deadline: Duration) -> io::Result<Self> {
        let binary = codex_binary();
        let mut child = Command::new(&binary)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| {
                io::Error::other(format!(
                    "could not run `{} app-server`: {err}",
                    binary.to_string_lossy()
                ))
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let child = Arc::new(Mutex::new(child));
        let watched = Arc::clone(&child);
        std::thread::spawn(move || {
            std::thread::sleep(deadline);
            if let Ok(mut child) = watched.lock() {
                let _ = child.kill();
            }
        });
        let mut server = AppServer {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 0,
        };
        server.call(
            "initialize",
            serde_json::json!({
                "clientInfo": {"name": "conbus", "title": "conbus", "version": crate::build_info::BUILD},
                "capabilities": {"experimentalApi": true}
            }),
        )?;
        server.notify("initialized", serde_json::json!({}))?;
        Ok(server)
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) -> io::Result<()> {
        let line = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()
    }

    /// Ask one question and wait for its answer. Notifications and other
    /// traffic the server sends in between are skipped.
    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> io::Result<serde_json::Value> {
        self.next_id += 1;
        let id = self.next_id;
        let line =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        for line in self.lines.by_ref() {
            let line = line?;
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if message["id"] != id {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(io::Error::other(format!(
                    "codex app-server refused {method}: {error}"
                )));
            }
            return Ok(message["result"].clone());
        }
        Err(io::Error::other(format!(
            "codex app-server closed without answering {method}"
        )))
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
