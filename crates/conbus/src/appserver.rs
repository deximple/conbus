//! A client for `codex app-server`: JSON-RPC over the child's stdio.
//!
//! One fresh server per use, stopped afterwards. Quota and backfill only
//! read. The warm leg writes, and only into a thread we opened with
//! `thread/start` — never into one the user minted.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Which binary answers. `CONBUS_CODEX_BIN` overrides `PATH`, for tests and
/// for a Codex the daemon's launchd `PATH` (`/usr/bin:/bin:…`) cannot see.
/// When that PATH is empty of `codex`, the sibling next to this binary is
/// the install layout — both live under `~/.local/bin`.
pub fn codex_binary() -> OsString {
    std::env::var_os("CONBUS_CODEX_BIN")
        .filter(|value| !value.is_empty())
        .or_else(|| on_path("codex"))
        .or_else(|| sibling_of_bus("codex"))
        .unwrap_or_else(|| "codex".into())
}

fn on_path(name: &str) -> Option<OsString> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| executable(&dir.join(name)))
}

fn sibling_of_bus(name: &str) -> Option<OsString> {
    let exe = std::env::current_exe().ok()?;
    executable(&exe.parent()?.join(name))
}

fn executable(path: &Path) -> Option<OsString> {
    path.is_file().then(|| path.as_os_str().to_os_string())
}

/// Codex is `#!/usr/bin/env node`. The launchd agent PATH is
/// `/usr/bin:/bin:/usr/sbin:/sbin`, so `env node` dies before initialize.
/// The node that can run the sibling `codex` lives next to this binary,
/// then in the usual user-install directories.
/// PATH a vendor child can see. Launchd's is too thin for `#!/usr/bin/env node`.
pub fn path_for_vendor() -> OsString {
    let mut dirs = Vec::<PathBuf>::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_path_buf());
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".npm-global/bin"));
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    if let Some(existing) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(dirs)
        .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()))
}

/// How an answer that is an error starts. The server is up and said no, which
/// is a different thing from a server that stopped answering.
const REFUSED: &str = "codex app-server refused";

/// Whether this error is the server saying no, rather than the server gone.
pub fn is_refusal(err: &io::Error) -> bool {
    err.to_string().starts_with(REFUSED)
}

pub struct AppServer {
    /// `None` once reaped, when its id can belong to someone else.
    child: Arc<Mutex<Option<Child>>>,
    stdin: ChildStdin,
    /// What the server wrote, read by a thread of its own: a read blocks for as long
    /// as anything holds the other end open, which need not be the server.
    lines: Receiver<io::Result<String>>,
    deadline: Instant,
    /// Dropped with the server, which is what stops the watchdog.
    _stop_watchdog: Sender<()>,
    next_id: u64,
}

/// Kill the server and whatever it started. An npm `codex` is a node script spawning
/// the native binary with stdout inherited, so the process we started is not the one
/// that wedges. Started in a group of its own, all of them go together.
fn kill_group(child: &Child) {
    let group = child.id() as libc::pid_t;
    // SAFETY: `killpg` takes no pointers. The caller holds the unreaped child, whose
    // id leads this group and so cannot have been reused.
    unsafe { libc::killpg(group, libc::SIGKILL) };
}

impl AppServer {
    /// Start a server and complete the `initialize` handshake.
    ///
    /// `deadline` bounds the whole conversation, not one call: what stalls an
    /// app-server is the MCP servers it tries to start, not any question of
    /// ours. At the deadline a watchdog kills the server's process group, and a read
    /// gives up whatever is still holding its pipe open. Something that started a
    /// session of its own leaves the group, so it can outlive this, but not hold it up.
    pub fn start(deadline: Duration) -> io::Result<Self> {
        Self::start_with(&codex_binary(), deadline)
    }

    /// As [`AppServer::start`], with the binary named rather than resolved
    /// from the environment. Tests pass a stand-in this way: `set_var` is
    /// unsound while another thread reads the environment, and a test whose
    /// `CONBUS_CODEX_BIN` was not seen spawns the *real* app-server, which
    /// does not return — three of those held the suite for forty minutes on
    /// 2026-09-08.
    pub fn start_with(binary: &std::ffi::OsStr, deadline: Duration) -> io::Result<Self> {
        let binary = binary.to_os_string();
        let until = Instant::now() + deadline;
        let mut child = Command::new(&binary)
            .arg("app-server")
            .env("PATH", path_for_vendor())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
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
        let child = Arc::new(Mutex::new(Some(child)));
        let watched = Arc::clone(&child);
        let (stop_watchdog, stopped) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            if stopped.recv_timeout(deadline) == Err(RecvTimeoutError::Timeout)
                && let Ok(child) = watched.lock()
                && let Some(child) = child.as_ref()
            {
                kill_group(child);
            }
        });
        // Bounded, so that a server talking to nobody waits as it would on the pipe.
        let (line_out, lines) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line_out.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = AppServer {
            child,
            stdin,
            lines,
            deadline: until,
            _stop_watchdog: stop_watchdog,
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
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(line) => line?,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("codex app-server did not answer {method} inside its limit"),
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other(format!(
                        "codex app-server closed without answering {method}"
                    )));
                }
            };
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if message["id"] != id {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(io::Error::other(format!("{REFUSED} {method}: {error}")));
            }
            return Ok(message["result"].clone());
        }
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            kill_group(&child);
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_codex_next_to_the_bus_is_found_when_path_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("codex");
        fs::write(&fake, "#!/bin/sh\n").expect("write");
        assert_eq!(
            executable(&fake).as_deref(),
            Some(fake.as_os_str()),
            "the install layout is both binaries in one directory"
        );
        assert_eq!(executable(&dir.path().join("missing")), None);
    }

    #[test]
    fn a_server_that_stops_reading_cannot_hold_a_long_write_past_the_deadline() {
        // A digest for a warm thread can be bigger than the pipe. A wedged server
        // reads none of it, and the write blocks until the server is gone.
        let dir = tempfile::tempdir().expect("tempdir");
        let script = r#"#!/usr/bin/env python3
import json, sys, time
msg = json.loads(sys.stdin.readline())
print(json.dumps({"jsonrpc":"2.0","id":msg.get("id"),"result":{}}), flush=True)
time.sleep(60)
"#;
        let fake = dir.path().join("codex");
        crate::test_support::write_executable(&fake, script);
        // Its first exec can take seconds on a loaded machine, and the deadline is
        // what this test waits out.
        let _ = Command::new(&fake)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let mut server =
            AppServer::start_with(fake.as_os_str(), Duration::from_secs(3)).expect("started");
        let (answer, answered) = mpsc::channel();
        std::thread::spawn(move || {
            let big = "x".repeat(4 * 1024 * 1024);
            let _ = answer.send(server.call("thread/inject_items", serde_json::json!(big)));
        });

        let said = answered
            .recv_timeout(Duration::from_secs(30))
            .expect("the write was still blocked long past its deadline");

        assert!(said.is_err(), "{said:?}");
    }

    #[test]
    fn the_vendor_path_can_see_node_when_launchd_cannot() {
        let path = path_for_vendor();
        let path = path.to_string_lossy();
        assert!(
            path.contains("/usr/local/bin") && path.contains("/opt/homebrew/bin"),
            "node is not on the launchd PATH: {path}"
        );
    }
}
