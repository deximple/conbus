//! What a wedged `codex app-server` leaves behind on each way out. The server runs in a
//! process group of its own, so a signal to conbus does not reach it unless conbus passes
//! it on. Stand-ins only, and signals go only to what these tests start.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

mod support;

const THREAD: &str = "01a19463-cbd4-7b50-b819-000000000abc";

/// Starts a helper, says who they both are, answers `initialize`, then hangs on anything
/// else unless told to answer.
const SERVER: &str = r#"#!/usr/bin/env python3
import json, os, subprocess, sys, time
here = os.path.dirname(os.path.abspath(sys.argv[0]))
if len(sys.argv) < 2:
    sys.exit(0)
helper = subprocess.Popen(["/bin/sleep", "120"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
with open(os.path.join(here, "pids.tmp"), "w") as pids:
    pids.write(f"{os.getpid()} {helper.pid}\n")
os.rename(os.path.join(here, "pids.tmp"), os.path.join(here, "pids"))
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method"); i = msg.get("id")
    if m == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"userAgent":"fake"}}), flush=True)
    elif i is None:
        continue
    elif os.path.exists(os.path.join(here, "answers")):
        print(json.dumps({"jsonrpc":"2.0","id":i,"result":{"data":[],"nextCursor":None,"rateLimits":{}}}), flush=True)
    else:
        while True:
            time.sleep(60)
"#;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

/// A root, and a stand-in that has run once so its first exec is not charged to a limit.
struct Case {
    _dir: tempfile::TempDir,
    home: PathBuf,
    stub: PathBuf,
    codex: PathBuf,
}

/// Whatever a failing test left of the stand-ins goes when the test does. These ids are
/// children of this test's own conbus.
impl Drop for Case {
    fn drop(&mut self) {
        for pid in pids_in(&self.stub) {
            if alive(pid) {
                // SAFETY: `kill` takes no pointers.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

fn a_case() -> Case {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let stub = dir.path().join("stub");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&stub).expect("stub");
    let codex = stub.join("codex");
    support::write_executable(&codex, SERVER);
    let warmed = Command::new(&codex)
        .stdin(Stdio::null())
        .status()
        .expect("warm");
    assert!(warmed.success());
    Case {
        _dir: dir,
        home,
        stub,
        codex,
    }
}

fn env_for(command: &mut Command, case: &Case) {
    command
        .env("CONBUS_HOME", &case.home)
        .env("HOME", &case.home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", &case.codex)
        .env("CONBUS_AGY_BIN", "/nonexistent/agy")
        .env("CONBUS_CLAUDE_BIN", "/nonexistent/claude")
        .env_remove("CODEX_HOME")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
}

/// conbus in a group of its own, as a shell runs a foreground command.
fn conbus(case: &Case, args: &[&str]) -> Child {
    let mut command = Command::new(bin());
    command.args(args).process_group(0);
    env_for(&mut command, case);
    command.spawn().expect("conbus")
}

fn alive(pid: i32) -> bool {
    let out = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

fn pids_in(stub: &Path) -> Vec<i32> {
    std::fs::read_to_string(stub.join("pids"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

fn wait_for_the_server(case: &Case) -> Vec<i32> {
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        let pids = pids_in(&case.stub);
        if pids.len() == 2 {
            // Past its `initialize`, so it is wedged on the real question.
            std::thread::sleep(Duration::from_millis(300));
            return pids;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the stand-in never ran");
}

fn wait_for_exit(child: &mut Child) -> ExitStatus {
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    panic!("conbus was still running 30 s after the signal");
}

fn assert_nothing_left(pids: &[i32]) {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end && pids.iter().any(|pid| alive(*pid)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let left: Vec<&i32> = pids.iter().filter(|pid| alive(**pid)).collect();
    assert!(
        left.is_empty(),
        "left behind: {left:?} of server and helper {pids:?}"
    );
}

fn send(pid: u32, signal: libc::c_int) {
    // SAFETY: `kill` takes no pointers, and the pid is this test's own child.
    unsafe { libc::kill(pid as i32, signal) };
}

/// One way out: `args`, wedged, this signal to conbus alone, and what it left.
fn signalled(args: &[&str], signal: libc::c_int) {
    let case = a_case();
    let mut child = conbus(&case, args);
    let pids = wait_for_the_server(&case);

    send(child.id(), signal);

    let status = wait_for_exit(&mut child);
    assert_nothing_left(&pids);
    assert_eq!(
        status.signal(),
        Some(signal),
        "{args:?}: it should die of the signal, as it did before: {status:?}"
    );
}

#[test]
fn ctrl_c_on_quota_kills_a_wedged_server() {
    signalled(&["quota"], libc::SIGINT);
}

#[test]
fn a_closed_terminal_on_quota_kills_a_wedged_server() {
    signalled(&["quota"], libc::SIGHUP);
}

#[test]
fn sigterm_on_quota_kills_a_wedged_server() {
    signalled(&["quota"], libc::SIGTERM);
}

#[test]
fn ctrl_c_on_status_kills_a_wedged_server() {
    signalled(&["status"], libc::SIGINT);
}

#[test]
fn ctrl_c_on_doctor_kills_a_wedged_server() {
    // Its probes have two seconds, and it may be past them by the time the signal lands,
    // so what it dies of is not the point here.
    let case = a_case();
    let mut child = conbus(&case, &["doctor"]);
    let pids = wait_for_the_server(&case);

    send(child.id(), libc::SIGINT);

    let _ = wait_for_exit(&mut child);
    assert_nothing_left(&pids);
}

fn a_record(case: &Case, harness: &str, payload: &serde_json::Value) {
    let mut command = Command::new(bin());
    command.args(["hook", "--harness", harness]);
    env_for(&mut command, case);
    let mut child = command.stdin(Stdio::piped()).spawn().expect("hook");
    std::io::Write::write_all(
        child.stdin.as_mut().expect("stdin"),
        payload.to_string().as_bytes(),
    )
    .expect("write");
    child.wait().expect("wait");
}

fn serving(case: &Case) -> Child {
    let mut command = Command::new(bin());
    command.arg("daemon").process_group(0);
    env_for(&mut command, case);
    let mut daemon = command
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .spawn()
        .expect("daemon");
    let stamp = case.home.join("daemon.build");
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        let text = std::fs::read_to_string(&stamp).unwrap_or_default();
        if text.contains("serving") && text.contains(&format!("\"pid\":{}", daemon.id())) {
            return daemon;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    panic!("the daemon never served");
}

/// A live Codex thread with a rollout, so a start's Codex pass brings the server up.
fn a_live_thread(case: &Case) {
    let mut daemon = serving(case);
    a_record(
        case,
        "codex",
        &serde_json::json!({"session_id": THREAD, "hook_event_name": "UserPromptSubmit",
            "prompt": "before", "turn_id": "t0", "cwd": "/repo", "model": "m",
            "permission_mode": "default", "transcript_path": null}),
    );
    let _ = daemon.kill();
    let _ = daemon.wait();
    let rollouts = case.home.join(".codex/sessions/2026/11/01");
    std::fs::create_dir_all(&rollouts).expect("rollouts");
    std::fs::write(
        rollouts.join(format!("rollout-2026-11-01T00-00-00-{THREAD}.jsonl")),
        "",
    )
    .expect("rollout");
}

#[test]
fn a_signal_to_hop_kills_a_wedged_server() {
    let case = a_case();
    let mut daemon = serving(&case);
    a_record(
        &case,
        "claude_code",
        &serde_json::json!({"session_id": "ea1d23e9-bb83-4df5-9a73-a6a12e138d85",
            "hook_event_name": "UserPromptSubmit", "prompt": "before",
            "prompt_id": "p0", "cwd": case.home.to_str().expect("utf-8")}),
    );
    let _ = daemon.kill();
    let _ = daemon.wait();
    let mut command = Command::new(bin());
    command.arg("hop").current_dir(&case.home).process_group(0);
    env_for(&mut command, &case);
    let mut child = command.spawn().expect("hop");
    let pids = wait_for_the_server(&case);

    send(child.id(), libc::SIGINT);

    let status = wait_for_exit(&mut child);
    assert_nothing_left(&pids);
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
}

#[test]
fn sigterm_on_a_starts_prepare_kills_a_wedged_server() {
    let case = a_case();
    a_live_thread(&case);
    let mut child = conbus(&case, &["daemon", "--prepare"]);
    let pids = wait_for_the_server(&case);

    send(child.id(), libc::SIGTERM);

    let status = wait_for_exit(&mut child);
    assert_nothing_left(&pids);
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
}

#[test]
fn sigterm_to_a_starting_daemons_group_kills_a_wedged_server() {
    let case = a_case();
    a_live_thread(&case);
    let mut child = conbus(&case, &["daemon"]);
    let pids = wait_for_the_server(&case);

    // SAFETY: `killpg` takes no pointers, and this group is this test's own child's.
    unsafe { libc::killpg(child.id() as i32, libc::SIGTERM) };

    let _ = wait_for_exit(&mut child);
    assert_nothing_left(&pids);
}

#[test]
fn a_normal_finish_leaves_nothing_either() {
    let case = a_case();
    std::fs::write(case.stub.join("answers"), "").expect("flag");
    let mut child = conbus(&case, &["quota"]);
    let pids = wait_for_the_server(&case);

    let _ = wait_for_exit(&mut child);

    assert_nothing_left(&pids);
}

#[test]
fn a_hangup_someone_asked_to_ignore_stays_ignored() {
    // `nohup conbus quota`: it should run on, as it always did.
    let case = a_case();
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "trap '' HUP; exec \"$0\" quota", bin()])
        .process_group(0);
    env_for(&mut command, &case);
    let mut child = command.spawn().expect("sh");
    let pids = wait_for_the_server(&case);

    send(child.id(), libc::SIGHUP);
    std::thread::sleep(Duration::from_millis(500));

    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "it died of the hangup"
    );
    assert!(
        pids.iter().all(|pid| alive(*pid)),
        "its server died of the hangup"
    );
    send(child.id(), libc::SIGINT);
    let _ = wait_for_exit(&mut child);
    assert_nothing_left(&pids);
}
