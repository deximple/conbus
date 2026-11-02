//! A client for `codex app-server`: JSON-RPC over the child's stdio.
//!
//! One fresh server per use, stopped afterwards. Quota and backfill only
//! read. The warm leg writes, and only into a thread we opened with
//! `thread/start` — never into one the user minted.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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

/// Whether the server said it has no such method: the one refusal that asking again
/// cannot change. JSON-RPC's own word for it is -32601. The real server parses a
/// request into an enum, and its binary holds "Invalid request: " and "unknown
/// variant" but no "method not found", so it would answer in those words. A real
/// server has not been seen doing so.
pub fn is_unknown_method(err: &io::Error, method: &str) -> bool {
    let said = err.to_string();
    is_refusal(err)
        && (said.contains("\"code\":-32601")
            || said.contains(&format!("unknown variant `{method}`")))
}

/// Whether the server said to ask again later, in the words the real binary holds.
pub fn says_retry_later(err: &io::Error) -> bool {
    let said = err.to_string();
    is_refusal(err) && (said.contains("\"code\":-32001") || said.contains("retry later"))
}

pub struct AppServer {
    child: Child,
    /// What the server wrote, read by a thread of its own: a read blocks for as long
    /// as anything holds the other end open, which need not be the server.
    lines: Receiver<io::Result<String>>,
    /// What to write to it, written by a thread of its own for the same reason: a long
    /// write blocks for as long as something holds the other end open and reads nothing.
    to_server: Sender<String>,
    deadline: Instant,
    next_id: u64,
}

/// What a signal has to do before this process goes.
///
/// The server is in a process group of its own, so Ctrl-C, a closed terminal or a TERM
/// does not reach it, and the watchdog dies with this process. A wedged server would stay
/// until it ended by itself. So while one lives, these three signals kill its group first
/// and then go on to whatever they did before: the default, which ends this process the
/// way it always did, or the handler `serve` installs, which is left to do its work.
/// SIGKILL cannot be caught, so a service manager's kill of this process still leaves a
/// wedged server behind.
mod on_signal {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// The groups of the servers alive now, 0 for a free slot. A signal handler cannot
    /// take a lock, and more than a few servers at once does not happen.
    static GROUPS: [AtomicI32; 8] = [const { AtomicI32::new(0) }; 8];

    /// What each of [`SIGNALS`] did before. Each one published here is never changed or
    /// freed, so a handler can read it whenever it runs.
    static BEFORE: [AtomicPtr<libc::sigaction>; 3] =
        [const { AtomicPtr::new(std::ptr::null_mut()) }; 3];

    /// Keeps two servers starting at once from taking each other's handler for the one
    /// to go on to.
    static INSTALLING: Mutex<()> = Mutex::new(());

    /// Only what is async-signal-safe: atomics, `killpg`, `sigaction` and `raise`.
    extern "C" fn kill_servers_and_go_on(signal: libc::c_int) {
        // SAFETY: each of these calls is async-signal-safe and takes either plain values
        // or a pointer to a `sigaction` that is never changed or freed. errno is saved
        // and put back, as the code this interrupted may be about to read it.
        unsafe {
            let errno = errno_location();
            let saved = *errno;
            for slot in &GROUPS {
                let group = slot.load(Ordering::SeqCst);
                if group > 0 {
                    libc::killpg(group, libc::SIGKILL);
                }
            }
            let before = SIGNALS
                .iter()
                .position(|known| *known == signal)
                .map_or(std::ptr::null_mut(), |at| BEFORE[at].load(Ordering::SeqCst));
            if before.is_null() {
                libc::signal(signal, libc::SIG_DFL);
            } else {
                libc::sigaction(signal, before, std::ptr::null_mut());
            }
            // Blocked until this handler returns, and then what it did before happens.
            libc::raise(signal);
            *errno = saved;
        }
    }

    #[cfg(target_os = "macos")]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: returns this thread's errno, and is safe to call at any time.
        unsafe { libc::__error() }
    }

    #[cfg(not(target_os = "macos"))]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: returns this thread's errno, and is safe to call at any time.
        unsafe { libc::__errno_location() }
    }

    /// Have the signals kill this group too, from now until [`forget`].
    pub fn watch(group: libc::pid_t) {
        let Ok(_installing) = INSTALLING.lock() else {
            return;
        };
        for slot in &GROUPS {
            if slot
                .compare_exchange(0, group, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
        let ours = kill_servers_and_go_on as *const () as usize;
        for (at, signal) in SIGNALS.iter().enumerate() {
            // SAFETY: both `sigaction` values are fully initialised before a call reads
            // them, and the handler is async-signal-safe.
            unsafe {
                let mut before: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(*signal, std::ptr::null(), &mut before) != 0 {
                    continue;
                }
                // Already ours, or ignored on purpose, as `nohup` does.
                if before.sa_sigaction == ours || before.sa_sigaction == libc::SIG_IGN {
                    continue;
                }
                BEFORE[at].store(Box::into_raw(Box::new(before)), Ordering::SeqCst);
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = ours;
                libc::sigemptyset(&mut action.sa_mask);
                // What a call in progress does when interrupted stays as it was.
                action.sa_flags = before.sa_flags & libc::SA_RESTART;
                libc::sigaction(*signal, &action, std::ptr::null_mut());
            }
        }
    }

    #[cfg(test)]
    pub fn watching(group: libc::pid_t) -> bool {
        GROUPS
            .iter()
            .any(|slot| slot.load(Ordering::SeqCst) == group)
    }

    /// Stop killing this group. Before it is reaped, when its id can be someone else's.
    pub fn forget(group: libc::pid_t) {
        for slot in &GROUPS {
            if slot
                .compare_exchange(group, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
    }
}

/// Kill the server and whatever it started. An npm `codex` is a node script spawning
/// the native binary with stdout inherited, so the process we started is not the one
/// that wedges. Started in a group of its own, all of them go together. The server itself
/// is killed by id as well, in case it moved out of that group: reaping it waits for it.
fn kill_group(child: &mut Child) {
    let group = child.id() as libc::pid_t;
    // SAFETY: `killpg` takes no pointers. The caller holds the unreaped child, whose
    // id leads this group and so cannot have been reused.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    let _ = child.kill();
}

impl AppServer {
    /// Start a server and complete the `initialize` handshake.
    ///
    /// `deadline` bounds the whole conversation, not one call: what stalls an
    /// app-server is the MCP servers it tries to start, not any question of
    /// ours. At the deadline a call gives up, whatever is still holding its pipes open,
    /// and dropping the server kills its process group. Something that started a session
    /// of its own leaves the group, so it can outlive this, but not hold it up.
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
        on_signal::watch(child.id() as libc::pid_t);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let (to_server, to_write) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut stdin = stdin;
            for line in to_write {
                if writeln!(stdin, "{line}")
                    .and_then(|()| stdin.flush())
                    .is_err()
                {
                    break;
                }
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
            lines,
            to_server,
            deadline: until,
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
        self.write(line.to_string())
    }

    fn write(&self, line: String) -> io::Result<()> {
        self.to_server
            .send(line)
            .map_err(|_| io::Error::other("codex app-server stopped reading"))
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
        self.write(line.to_string())?;
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
        kill_group(&mut self.child);
        on_signal::forget(self.child.id() as libc::pid_t);
        let _ = self.child.wait();
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
    fn only_a_server_saying_it_has_no_such_method_is_read_as_that() {
        let refused =
            |error: &str| io::Error::other(format!("{REFUSED} thread/turns/list: {error}"));
        let method = "thread/turns/list";
        for no_such_method in [
            r#"{"code":-32601,"message":"method not found"}"#,
            r#"{"code":-32600,"message":"Invalid request: unknown variant `thread/turns/list`, expected one of `initialize`"}"#,
        ] {
            assert!(is_unknown_method(&refused(no_such_method), method));
        }
        for for_now_or_this_thread in [
            r#"{"code":-32001,"message":"Server overloaded; retry later."}"#,
            r#"{"code":-32600,"message":"no rollout found for thread"}"#,
            r#"{"code":-32600,"message":"Invalid request: unknown variant `asc`, expected one of `x`"}"#,
        ] {
            assert!(!is_unknown_method(&refused(for_now_or_this_thread), method));
        }
        let gone = io::Error::other(
            "codex app-server closed without answering unknown variant `thread/turns/list`",
        );
        assert!(!is_unknown_method(&gone, method));
    }

    const WEDGED_AFTER_INITIALIZE: &str = r#"#!/usr/bin/env python3
import json, sys, time
if len(sys.argv) < 2: sys.exit(0)
msg = json.loads(sys.stdin.readline())
print(json.dumps({"jsonrpc":"2.0","id":msg.get("id"),"result":{}}), flush=True)
time.sleep(60)
"#;

    /// A recorder's handler lands what it holds on its way out, and a server started after
    /// it must not take that away. A signal belongs to a whole process, so this runs in one
    /// of its own.
    #[test]
    fn a_handler_already_there_still_runs_when_a_signal_kills_a_server() {
        use std::os::unix::process::ExitStatusExt;
        use std::sync::atomic::{AtomicI32, Ordering};

        const ALONE: &str = "CONBUS_TEST_SIGNAL_ALONE";
        if std::env::var_os(ALONE).is_none() {
            let out = Command::new(std::env::current_exe().expect("this test binary"))
                .args([
                    "--exact",
                    "appserver::tests::a_handler_already_there_still_runs_when_a_signal_kills_a_server",
                    "--nocapture",
                ])
                .env(ALONE, "1")
                .stdin(Stdio::null())
                .output()
                .expect("ran alone");
            let said = String::from_utf8_lossy(&out.stdout);
            // Asked for a name that no longer exists, it would run nothing and exit 0.
            assert!(
                out.status.success() && said.contains("1 passed"),
                "{:?}\n{said}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }

        static NOTED: AtomicI32 = AtomicI32::new(0);
        extern "C" fn note(signal: libc::c_int) {
            NOTED.store(signal, Ordering::SeqCst);
        }
        // SAFETY: the handler only stores to an atomic, and `action` is fully initialised
        // before the call reads it.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = note as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            assert_eq!(
                libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()),
                0
            );
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("codex");
        crate::test_support::write_executable(&fake, WEDGED_AFTER_INITIALIZE);
        let _ = Command::new(&fake)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // Twice, as the first signal hands the signal back to the handler that was there.
        for _ in 0..2 {
            NOTED.store(0, Ordering::SeqCst);
            let mut server =
                AppServer::start_with(fake.as_os_str(), Duration::from_secs(30)).expect("started");

            // SAFETY: raising a signal touches no memory.
            unsafe { libc::raise(libc::SIGTERM) };

            assert_eq!(
                NOTED.load(Ordering::SeqCst),
                libc::SIGTERM,
                "the handler that was there ran"
            );
            let until = Instant::now() + Duration::from_secs(10);
            let status = loop {
                let status = server.child.try_wait().expect("try_wait");
                if status.is_some() || Instant::now() > until {
                    break status;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            assert_eq!(
                status.and_then(|status| status.signal()),
                Some(libc::SIGKILL),
                "the server went too"
            );
        }
    }

    #[test]
    fn a_server_gone_is_not_a_group_a_signal_would_kill() {
        // Its id can be someone else's once it has been reaped.
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("codex");
        crate::test_support::write_executable(
            &fake,
            "#!/usr/bin/env python3\nimport json, sys\nfor line in sys.stdin:\n    msg = json.loads(line)\n    print(json.dumps({\"jsonrpc\":\"2.0\",\"id\":msg.get(\"id\"),\"result\":{}}), flush=True)\n",
        );
        let server =
            AppServer::start_with(fake.as_os_str(), Duration::from_secs(30)).expect("started");
        let group = server.child.id() as libc::pid_t;
        assert!(on_signal::watching(group));

        drop(server);

        assert!(!on_signal::watching(group));
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
    fn a_process_outside_the_group_holding_stdin_cannot_hold_a_long_write_past_the_deadline() {
        // It holds the server's stdin open and reads nothing, and killing the group does
        // not reach it, so the pipe stays open and full.
        let dir = tempfile::tempdir().expect("tempdir");
        let script = r#"#!/usr/bin/env python3
import json, os, sys, time
if len(sys.argv) < 2: sys.exit(0)
msg = json.loads(sys.stdin.readline())
print(json.dumps({"jsonrpc":"2.0","id":msg.get("id"),"result":{}}), flush=True)
if os.fork() == 0:
    os.setsid()
    time.sleep(25)
    os._exit(0)
time.sleep(60)
"#;
        let fake = dir.path().join("codex");
        crate::test_support::write_executable(&fake, script);
        let _ = Command::new(&fake)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let mut server =
            AppServer::start_with(fake.as_os_str(), Duration::from_secs(2)).expect("started");
        let (answer, answered) = mpsc::channel();
        std::thread::spawn(move || {
            let big = "x".repeat(4 * 1024 * 1024);
            let _ = answer.send(server.call("thread/inject_items", serde_json::json!(big)));
        });

        let said = answered.recv_timeout(Duration::from_secs(10));

        assert!(said.is_ok(), "still blocked 8 s past its deadline");
    }

    #[test]
    fn a_child_that_left_its_group_does_not_hang_the_drop() {
        // The group kill does not reach it, and reaping it waits for it.
        let dir = tempfile::tempdir().expect("tempdir");
        let script = r#"#!/usr/bin/env python3
import json, os, sys, time
if len(sys.argv) < 2: sys.exit(0)
os.setpgid(0, os.getpgid(os.getppid()))
msg = json.loads(sys.stdin.readline())
print(json.dumps({"jsonrpc":"2.0","id":msg.get("id"),"result":{}}), flush=True)
time.sleep(25)
"#;
        let fake = dir.path().join("codex");
        crate::test_support::write_executable(&fake, script);
        let _ = Command::new(&fake)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let server =
            AppServer::start_with(fake.as_os_str(), Duration::from_secs(30)).expect("started");
        let (done, dropped) = mpsc::channel();
        std::thread::spawn(move || {
            drop(server);
            let _ = done.send(());
        });

        assert!(
            dropped.recv_timeout(Duration::from_secs(8)).is_ok(),
            "drop still waiting after 8 s"
        );
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
