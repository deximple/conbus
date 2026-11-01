//! The socket is bound only after restore: a hook that arrives while the
//! daemon is still preparing meets a refused or absent socket at once,
//! instead of waiting 500 ms on a listener that cannot answer.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

/// The stamp's `serving` field, when the stamp exists at all.
fn serving(home: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("daemon.build")).ok()?;
    let stamp: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    stamp
        .get("serving")
        .and_then(|s| s.as_str())
        .map(String::from)
}

#[test]
fn the_socket_answers_only_once_the_stamp_says_serving() {
    let home = tempfile::tempdir().expect("tempdir");
    let socket = home.path().join("conbus.sock");
    // The test knob widens the prepare->bind window from microseconds to
    // something a poll can observe.
    let mut child = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_TEST_PAUSE_BEFORE_BIND_MS", "800")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    // While the stamp says "starting" the socket must refuse — this is the
    // property: no listener accepts a hook it cannot serve.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut refused = false;
    let mut served = false;
    while Instant::now() < deadline {
        match serving(home.path()) {
            None => {}
            Some(_) => {
                served = true;
                break;
            }
        }
        if std::os::unix::net::UnixStream::connect(&socket).is_err() {
            refused = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !served {
        let _ = child.kill();
        panic!("the daemon never stamped `serving`");
    }
    assert!(
        refused || serving(home.path()).is_none(),
        "the socket accepted before `serving` was stamped"
    );

    // Once the stamp says serving, the socket really answers.
    let answered = std::os::unix::net::UnixStream::connect(&socket).is_ok();
    let _ = child.kill();
    let _ = child.wait();
    assert!(answered, "a stamped-serving daemon must answer its socket");
}

const SESSION: &str = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";

/// A daemon that goes with its test, however the test ends. One a failed assert
/// left behind would run on for good, over a root that has been deleted.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn daemon_in(home: &std::path::Path) -> Command {
    let mut command = Command::new(bin());
    command
        .arg("daemon")
        .env("CONBUS_HOME", home)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn stamped_pid(home: &std::path::Path) -> Option<u64> {
    let text = std::fs::read_to_string(home.join("daemon.build")).ok()?;
    let stamp: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    stamp.get("pid")?.as_u64()
}

/// Read a daemon's stderr until it says it is waiting for another recorder, or
/// until it has nothing more to say. A sleep here would pass on a build that
/// writes early whenever macOS is slow to start a freshly built binary.
fn until_it_says_it_waits(child: &mut Child) {
    use std::io::{BufRead, BufReader};
    let stderr = child.stderr.take().expect("stderr is piped");
    let (said, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("waiting") {
                let _ = said.send(());
            }
        }
        let _ = said.send(());
    });
    let _ = heard.recv_timeout(Duration::from_secs(30));
}

/// Wait until this process is the one the stamp says is serving.
fn wait_serving(home: &std::path::Path, pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if stamped_pid(home) == Some(u64::from(pid)) && serving(home).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("pid {pid} never stamped itself serving");
}

fn hook(home: &std::path::Path, payload: &str) {
    use std::io::Write;
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
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    child.wait().expect("wait");
}

fn prompt(id: &str, text: &str) -> String {
    serde_json::json!({
        "session_id": SESSION,
        "hook_event_name": "UserPromptSubmit",
        "prompt": text,
        "prompt_id": id,
        "cwd": "/repo"
    })
    .to_string()
}

/// Every record on disk, parsed from the segments rather than through a build
/// of the reader.
fn records(home: &std::path::Path) -> Vec<serde_json::Value> {
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

/// A transcript holding a turn the log does not: what a start fills a gap from.
fn transcript_with_a_gap(home: &std::path::Path) {
    let dir = home.join(".claude/projects/-repo");
    std::fs::create_dir_all(&dir).expect("dir");
    let line = |id: &str, text: &str| {
        serde_json::json!({
            "type": "user", "sessionId": SESSION, "timestamp": "2026-11-01T00:00:00.000Z",
            "version": "2.1.316", "cwd": "/repo", "entrypoint": "cli", "promptId": id,
            "message": {"role": "user", "content": text}
        })
        .to_string()
    };
    std::fs::write(
        dir.join(format!("{SESSION}.jsonl")),
        [
            line("p0", "first"),
            line("gap", "only the transcript holds this"),
        ]
        .join("\n")
            + "\n",
    )
    .expect("transcript");
}

#[test]
fn a_second_daemon_writes_nothing_before_it_gives_up() {
    // Hand-started beside a live recorder it used to stamp over the live pid,
    // fill gaps and drain the spool, and only then did `bind` refuse it: one
    // record, one duplicate sequence number, and a doctor reading a dead pid.
    let home = tempfile::tempdir().expect("tempdir");
    let live = Daemon(daemon_in(home.path()).spawn().expect("daemon"));
    wait_serving(home.path(), live.0.id());
    hook(home.path(), &prompt("p0", "first"));
    transcript_with_a_gap(home.path());
    let before = records(home.path()).len();

    let status = daemon_in(home.path()).status().expect("second daemon");

    let wrote = records(home.path()).len() - before;
    let stamp = stamped_pid(home.path());
    hook(
        home.path(),
        &prompt("p1", "after the second daemon gave up"),
    );
    let live_pid = live.0.id();
    drop(live);

    assert!(!status.success(), "it did not become the recorder");
    assert_eq!(wrote, 0, "it wrote to a log it does not own");
    assert_eq!(stamp, Some(u64::from(live_pid)), "it took the live stamp");
    let mut seqs: Vec<u64> = records(home.path())
        .iter()
        .filter_map(|record| record["seq"].as_u64())
        .collect();
    let total = seqs.len();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), total, "a sequence number was used twice");
}

#[test]
fn a_prepare_run_by_hand_beside_a_live_recorder_writes_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let live = Daemon(daemon_in(home.path()).spawn().expect("daemon"));
    wait_serving(home.path(), live.0.id());
    hook(home.path(), &prompt("p0", "first"));
    transcript_with_a_gap(home.path());
    let before = records(home.path()).len();

    let status = daemon_in(home.path())
        .arg("--prepare")
        .status()
        .expect("prepare");

    let wrote = records(home.path()).len() - before;
    drop(live);
    assert!(!status.success(), "there is already a recorder here");
    assert_eq!(wrote, 0, "it wrote beside the live recorder");
}

#[test]
fn a_start_waits_out_a_daemon_that_is_on_its_way_out() {
    // `install --apply` boots out and then bootstraps, so the old daemon can
    // still be there when the new one starts. Giving up at once would cost a
    // restart under the service manager; taking over would be two writers.
    let home = tempfile::tempdir().expect("tempdir");
    let old = Daemon(daemon_in(home.path()).spawn().expect("old"));
    wait_serving(home.path(), old.0.id());
    let old_pid = old.0.id();

    let mut new = Daemon(
        daemon_in(home.path())
            .stderr(Stdio::piped())
            .spawn()
            .expect("new"),
    );
    until_it_says_it_waits(&mut new.0);
    assert_eq!(
        stamped_pid(home.path()),
        Some(u64::from(old_pid)),
        "the new daemon stamped before the old one had gone"
    );
    drop(old);

    wait_serving(home.path(), new.0.id());
}

#[test]
fn an_older_daemon_that_takes_no_lock_is_still_waited_for() {
    // The first start of this build meets a daemon of the build before it,
    // which knows nothing of the lock and answers only on the socket.
    let home = tempfile::tempdir().expect("tempdir");
    let older = std::os::unix::net::UnixListener::bind(home.path().join("conbus.sock"))
        .expect("an older daemon's socket");

    let mut new = Daemon(
        daemon_in(home.path())
            .stderr(Stdio::piped())
            .spawn()
            .expect("new"),
    );
    until_it_says_it_waits(&mut new.0);
    let stamped_early = stamped_pid(home.path());
    let wrote_early = home.path().join("log").exists();
    drop(older);

    wait_serving(home.path(), new.0.id());
    drop(new);
    assert_eq!(stamped_early, None, "it stamped beside a serving daemon");
    assert!(!wrote_early, "it opened the log beside a serving daemon");
}

#[test]
fn the_claim_goes_with_the_daemon_however_it_dies() {
    // Killed, not asked: nothing gets to clean up a lock file, so the file
    // staying behind must not be what says somebody is here.
    let home = tempfile::tempdir().expect("tempdir");
    let first = Daemon(daemon_in(home.path()).spawn().expect("first"));
    wait_serving(home.path(), first.0.id());
    drop(first);

    let started = Instant::now();
    let second = Daemon(daemon_in(home.path()).spawn().expect("second"));
    wait_serving(home.path(), second.0.id());
    let waited = started.elapsed();
    drop(second);
    assert!(
        waited < Duration::from_secs(4),
        "a dead daemon's claim held the next one up for {waited:?}"
    );
}

/// A transcript that takes a start seconds to read, with turns only it holds at
/// the end of it: long enough to kill a daemon while its child is still reading.
fn a_long_transcript_with_three_gaps(home: &std::path::Path) {
    let dir = home.join(".claude/projects/-repo");
    std::fs::create_dir_all(&dir).expect("dir");
    let ts = String::from_utf8(
        Command::new("/bin/date")
            .args(["-u", "+%Y-%m-%dT%H:%M:%S.000Z"])
            .output()
            .expect("date")
            .stdout,
    )
    .expect("utf8");
    let ts = ts.trim();
    let user = |id: &str, text: &str| {
        serde_json::json!({"type":"user","sessionId":SESSION,"timestamp":ts,"version":"2.1.316",
            "cwd":"/repo","entrypoint":"cli","promptId":id,"message":{"role":"user","content":text}})
        .to_string()
    };
    let filler = serde_json::json!({"type":"assistant","sessionId":SESSION,"timestamp":ts,
        "version":"2.1.316","message":{"role":"assistant","content":[{"type":"text","text":"x".repeat(2048)}]}})
    .to_string();
    let mut body = vec![user("p0", "first")];
    body.extend(std::iter::repeat_n(filler, 40_000));
    body.extend((0..3).map(|n| {
        user(
            &format!("gap{n}"),
            &format!("only the transcript holds this {n}"),
        )
    }));
    std::fs::write(dir.join(format!("{SESSION}.jsonl")), body.join("\n") + "\n")
        .expect("transcript");
}

/// Until this daemon has started its `--prepare` child. The child has the claim
/// from the moment it exists: it is handed over before the child runs a line.
fn until_it_has_a_child(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let children = Command::new("/usr/bin/pgrep")
            .args(["-P", &pid.to_string()])
            .output()
            .expect("pgrep");
        if !children.stdout.is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("pid {pid} never started its repair");
}

/// What launchd or systemd does: start a daemon, and start it again a moment after
/// it gives up, until one serves.
fn a_daemon_under_a_service_manager(home: &std::path::Path) -> Daemon {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let mut daemon = Daemon(daemon_in(home).spawn().expect("daemon"));
        while Instant::now() < deadline {
            if stamped_pid(home) == Some(u64::from(daemon.0.id())) && serving(home).is_some() {
                return daemon;
            }
            if daemon.0.try_wait().expect("try_wait").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(Instant::now() < deadline, "no daemon ever served");
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[test]
fn a_prepare_child_that_outlives_its_daemon_does_not_write_beside_the_next_one() {
    // A daemon dies alone, as one started by hand can, and the kernel frees its
    // lock. Its child went on filling gaps with no lock of its own, and the next
    // daemon's child filled the same gaps: duplicate sequence numbers and every
    // turn twice, for good.
    let home = tempfile::tempdir().expect("tempdir");
    let first = Daemon(daemon_in(home.path()).spawn().expect("first"));
    wait_serving(home.path(), first.0.id());
    hook(home.path(), &prompt("p0", "first"));
    drop(first);
    a_long_transcript_with_three_gaps(home.path());

    let mut a = daemon_in(home.path()).spawn().expect("a");
    let a_pid = a.id();
    until_it_has_a_child(a_pid);
    Command::new("/bin/kill")
        .args(["-TERM", &a_pid.to_string()])
        .status()
        .expect("kill");
    a.wait().expect("reap");

    // The daemon has gone and the claim has not, as a lock taken from outside shows.
    let lock = std::fs::File::open(home.path().join("daemon.lock")).expect("lock");
    let claimed = matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock));
    drop(lock);

    let b = a_daemon_under_a_service_manager(home.path());
    // Anything still writing beside it has time to show here.
    std::thread::sleep(Duration::from_secs(8));
    drop(b);

    assert!(
        claimed,
        "the claim went with the daemon, not with the child"
    );
    let all = records(home.path());
    let mut seqs: Vec<u64> = all.iter().filter_map(|r| r["seq"].as_u64()).collect();
    let total = seqs.len();
    seqs.sort_unstable();
    seqs.dedup();
    let gaps = all
        .iter()
        .filter(|r| {
            r["vendor_turn_id"]
                .as_str()
                .is_some_and(|t| t.starts_with("gap"))
        })
        .count();
    assert_eq!(
        (total - seqs.len(), gaps),
        (0, 3),
        "(duplicate seq, gap turn records)"
    );
}
