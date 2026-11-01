//! `conbus backup`: what it copies, what it refuses, and the restore the manual
//! documents, through the real binary, on real records from the real writer.
//!
//! Every run here sets `CONBUS_HOME` and `HOME` under `/tmp`, and points the
//! vendor binaries nowhere.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use conbus_core::clock::Timestamp;
use conbus_core::event::{Body, Entrypoint, Event, Harness, Source, ToolOutcome, TurnState};
use conbus_core::record::Entry;
use conbus_core::retention::Retention;
use conbus_core::segment::SegmentLog;
use sha2::{Digest, Sha256};

const DAY: &str = "2026-10-01";
const SMALL_SEGMENT: u64 = 1024;
const SAME_DISK: &str = "same disk: protects against deletion, not a failed disk";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

/// A data root, a home and a place for destinations, all under one `/tmp`
/// folder, spelled through its symlink-free path.
struct World {
    _temp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    home: PathBuf,
}

impl World {
    fn empty() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("conbus-backup-test-")
            .tempdir_in("/tmp")
            .expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical");
        let (root, home) = (base.join("root"), base.join("home"));
        fs::create_dir(&root).expect("root");
        fs::create_dir(&home).expect("home");
        World {
            _temp: temp,
            base,
            root,
            home,
        }
    }

    /// Several closed segments and an open one holding records.
    fn with_log() -> Self {
        let world = Self::empty();
        world.write_turns(12, SMALL_SEGMENT);
        assert!(segment_names(&world.log()).len() >= 5, "the log must roll");
        world
    }

    fn log(&self) -> PathBuf {
        self.root.join("log")
    }

    /// Append through the recorder's own writer, so a segment here is what a
    /// recorder writes, only smaller.
    fn write_turns(&self, turns: u64, segment_bytes: u64) {
        let mut log =
            SegmentLog::open_with_segment_size(self.log(), Retention::Full, segment_bytes)
                .expect("log");
        for turn in 0..turns {
            for event in turn_events(turn) {
                let second = log.next_sequence();
                log.append(entry(second, event)).expect("append");
            }
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(bin());
        command
            .env("CONBUS_HOME", &self.root)
            .env("HOME", &self.home)
            .env("CONBUS_SKIP_LAUNCHCTL", "1")
            .env("CONBUS_AGY_BIN", "/nonexistent/x")
            .env("CONBUS_CODEX_BIN", "/nonexistent/x")
            .env_remove("XDG_STATE_HOME")
            .env_remove("CONBUS_NO_CACHE")
            .stdin(Stdio::null());
        command
    }

    fn conbus(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run conbus")
    }

    fn backup(&self, to: &Path) -> Output {
        self.command()
            .args(["backup", "--to"])
            .arg(to)
            .output()
            .expect("run conbus backup")
    }

    fn verify(&self, to: &Path) -> Output {
        self.command()
            .args(["backup", "--verify", "--to"])
            .arg(to)
            .output()
            .expect("run conbus backup --verify")
    }

    /// What the two machine-readable reads say about this root right now.
    fn reads(&self) -> (String, String) {
        (
            ok(&self.conbus(&["log", "--json"])),
            ok(&self.conbus(&["usage", "--json", "--since", DAY])),
        )
    }

    fn doctor_line(&self) -> String {
        let doctor = ok(&self.conbus(&["doctor"]));
        let lines: Vec<&str> = doctor
            .lines()
            .filter(|line| line.starts_with("backup "))
            .collect();
        assert_eq!(lines.len(), 1, "doctor gets one backup line: {doctor}");
        lines[0].to_string()
    }

    fn record(&self) -> serde_json::Value {
        let text = fs::read_to_string(self.root.join("backup.json")).expect("backup.json");
        serde_json::from_str(&text).expect("backup.json is JSON")
    }
}

fn entry(second: u64, event: Event) -> Entry {
    Entry {
        bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
        observed_at: Some(
            format!(
                "{DAY}T{:02}:{:02}:{:02}.000Z",
                second / 3600,
                second / 60 % 60,
                second % 60
            )
            .parse()
            .expect("ts"),
        ),
        source: Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: None,
            vendor_session_id: None,
            workspace: Some("/tmp/project".into()),
            entrypoint: Some(Entrypoint::new("cli")),
            backfilled_from: None,
            profile: None,
        },
        vendor_turn_id: None,
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
        event,
    }
}

/// One turn. The prompt's length varies, so no two segments are the same size.
fn turn_events(turn: u64) -> [Event; 3] {
    [
        Event::TurnStarted {
            author: None,
            prompt: Body::kept(format!(
                "prompt {turn} {}",
                "p".repeat((turn % 37) as usize)
            )),
        },
        Event::ToolUse {
            tool: "Bash".into(),
            tool_use_id: None,
            duration_ms: Some(40),
            input: Body::kept("cargo test"),
            outcome: ToolOutcome::Succeeded {
                output: Body::kept("ok"),
            },
        },
        Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: Some(Body::kept("a reply")),
            tokens: None,
        },
    ]
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "expected success, got {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        stdout(out),
        stderr(out)
    );
    stdout(out)
}

/// The command ran and failed (exit 1, not a usage error), and said why.
fn refused(out: &Output, why: &str) {
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected a refusal\nstdout: {}\nstderr: {}",
        stdout(out),
        stderr(out)
    );
    assert!(
        stderr(out).contains(why),
        "the refusal must say {why:?}: {}",
        stderr(out)
    );
}

fn segment_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.starts_with("segment-") && name.ends_with(".jsonl"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn split_open(world: &World) -> (Vec<String>, String) {
    let mut names = segment_names(&world.log());
    let open = names.pop().expect("an open segment");
    (names, open)
}

fn backup_log(folder: &Path) -> PathBuf {
    folder.join("conbus-backup").join("log")
}

fn manifest_rows(folder: &Path) -> BTreeMap<String, (u64, String)> {
    let text = fs::read_to_string(folder.join("conbus-backup").join("manifest.json"))
        .expect("the manifest is written");
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("manifest is JSON");
    manifest["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .map(|row| {
            (
                row["name"].as_str().expect("name").to_string(),
                (
                    row["size"].as_u64().expect("size"),
                    row["sha256"].as_str().expect("sha256").to_string(),
                ),
            )
        })
        .collect()
}

fn sha256(path: &Path) -> String {
    Sha256::digest(fs::read(path).expect("read"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

fn inodes(dir: &Path) -> BTreeMap<String, u64> {
    segment_names(dir)
        .into_iter()
        .map(|name| {
            let ino = fs::metadata(dir.join(&name)).expect("metadata").ino();
            (name, ino)
        })
        .collect()
}

/// Every byte of every file under `dir`, so a test can say "untouched".
fn fingerprint(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in fs::read_dir(&next).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                waiting.push(path);
            } else {
                found.insert(path.clone(), fs::read(&path).expect("read"));
            }
        }
    }
    found
}

/// The manual's restore, as written: a shell, a glob and `cp -n`. Its exit
/// status is not asserted because `cp -n` over an existing file exits 1 on some
/// platforms; what landed is what the tests check.
fn restore(folder: &Path, root: &Path) {
    Command::new("/bin/sh")
        .args([
            "-c",
            r#"mkdir -p -m 700 "$2/log"
               find "$2/log" -name 'segment-*.jsonl' -empty -delete
               cp -n "$1"/conbus-backup/log/*.jsonl "$2/log/"
               chmod 600 "$2"/log/*.jsonl
               rm -rf "$2/cache/meta""#,
            "sh",
        ])
        .arg(folder)
        .arg(root)
        .stderr(Stdio::null())
        .status()
        .expect("run the restore");
}

// ---- What it copies ----

#[test]
fn closed_segments_are_copied_with_a_manifest_and_the_open_one_is_not() {
    let world = World::with_log();
    let (closed, open) = split_open(&world);
    let folder = world.base.join("dest");

    let said = ok(&world.backup(&folder));

    let copied = segment_names(&backup_log(&folder));
    assert_eq!(copied, closed, "exactly the closed segments");
    assert!(!copied.contains(&open), "never the open one");
    let rows = manifest_rows(&folder);
    assert_eq!(rows.len(), closed.len());
    for name in &closed {
        let source = world.log().join(name);
        let copy = backup_log(&folder).join(name);
        assert_eq!(fs::read(&source).expect("s"), fs::read(&copy).expect("c"));
        let (size, sha) = &rows[name];
        assert_eq!(*size, fs::metadata(&source).expect("meta").len());
        assert_eq!(*sha, sha256(&source));
    }
    assert!(
        said.contains(&format!("copied {} segment(s)", closed.len())),
        "{said}"
    );
    assert!(said.contains("read back"), "says it checked: {said}");
}

#[test]
fn it_says_how_many_records_are_newer_than_the_backup() {
    let world = World::with_log();
    let (_, open) = split_open(&world);
    let open_records = fs::read_to_string(world.log().join(&open))
        .expect("read")
        .lines()
        .count();
    assert!(open_records > 0, "the fixture's open segment holds records");

    let said = ok(&world.backup(&world.base.join("dest")));

    assert!(
        said.contains(&format!("newer than the backup: {open_records} record(s)")),
        "{said}"
    );
}

#[test]
fn a_torn_line_in_the_open_segment_is_not_counted_as_a_record() {
    use std::io::Write;
    let world = World::with_log();
    let (_, open) = split_open(&world);
    let open_path = world.log().join(&open);
    let whole = fs::read_to_string(&open_path)
        .expect("read")
        .lines()
        .count();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&open_path)
        .expect("open");
    file.write_all(br#"{"v":3,"ts":"2026-10-"#).expect("tear");
    drop(file);

    let said = ok(&world.backup(&world.base.join("dest")));

    assert!(
        said.contains(&format!("newer than the backup: {whole} record(s)")),
        "{said}"
    );
}

#[test]
fn a_rerun_copies_only_what_is_new() {
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let first = inodes(&backup_log(&folder));

    let manifest = folder.join("conbus-backup/manifest.json");
    let manifest_ino = fs::metadata(&manifest).expect("manifest").ino();
    let said = ok(&world.backup(&folder));
    assert!(said.contains("copied 0 segment(s)"), "{said}");
    assert_eq!(inodes(&backup_log(&folder)), first, "nothing was rewritten");
    assert_eq!(
        fs::metadata(&manifest).expect("manifest").ino(),
        manifest_ino,
        "a run with nothing new writes nothing at the destination"
    );

    world.write_turns(9, SMALL_SEGMENT);
    let (closed, _) = split_open(&world);
    let new = closed.len() - first.len();
    assert!(new > 0, "the fixture must roll again");
    let said = ok(&world.backup(&folder));
    assert!(said.contains(&format!("copied {new} segment(s)")), "{said}");
    assert_eq!(segment_names(&backup_log(&folder)), closed);
    let now = inodes(&backup_log(&folder));
    for (name, ino) in &first {
        assert_eq!(now[name], *ino, "{name} was rewritten");
    }
    assert_eq!(manifest_rows(&folder).len(), closed.len());
    ok(&world.verify(&folder));
}

#[test]
fn a_log_with_nothing_closed_yet_says_so_and_still_has_one_copy() {
    let world = World::empty();
    world.write_turns(2, 1024 * 1024);
    assert_eq!(segment_names(&world.log()).len(), 1);
    let folder = world.base.join("dest");

    let said = ok(&world.backup(&folder));

    assert!(said.contains("copied 0 segment(s)"), "{said}");
    assert!(said.contains("this log still has one copy"), "{said}");
    assert!(
        said.contains("newer than the backup: 6 record(s)"),
        "{said}"
    );
    assert!(segment_names(&backup_log(&folder)).is_empty());
    let line = world.doctor_line();
    assert!(
        line.starts_with("backup    none: this log has one copy"),
        "no segment is in the backup, whatever the run said: {line}"
    );
}

#[test]
fn a_root_with_no_log_is_refused_and_nothing_is_created() {
    let world = World::empty();
    let folder = world.base.join("dest");

    refused(&world.backup(&folder), "no log");

    assert!(!folder.exists());
    assert!(!world.root.join("backup.json").exists());
}

#[test]
fn what_is_copied_is_closed_to_its_owner_even_when_the_source_is_not() {
    // A log from before 0.2.5 can still hold 0644 segments. An external disk
    // is the wrong place for a copy to be more open than it has to be.
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let source = world.log().join(&closed[0]);
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).expect("chmod");
    let folder = world.base.join("dest");

    ok(&world.backup(&folder));

    assert_eq!(mode(&folder), 0o700, "a folder it made");
    assert_eq!(mode(&folder.join("conbus-backup")), 0o700);
    assert_eq!(mode(&backup_log(&folder)), 0o700);
    assert_eq!(mode(&folder.join("conbus-backup/manifest.json")), 0o600);
    for name in &closed {
        assert_eq!(mode(&backup_log(&folder).join(name)), 0o600, "{name}");
    }
    assert_eq!(mode(&world.root.join("backup.json")), 0o600);
    assert_eq!(mode(&source), 0o644, "the source is not its to change");
}

#[test]
fn a_folder_that_already_exists_keeps_its_own_mode() {
    // The folder may be a disk's root, or somewhere else that is the person's.
    let world = World::with_log();
    let folder = world.base.join("dest");
    fs::create_dir(&folder).expect("dir");
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).expect("chmod");

    ok(&world.backup(&folder));

    assert_eq!(mode(&folder), 0o755);
}

#[test]
fn a_relative_path_is_recorded_as_where_it_really_is() {
    let world = World::with_log();
    let out = world
        .command()
        .current_dir(&world.base)
        .args(["backup", "--to", "dest"])
        .output()
        .expect("run");
    ok(&out);
    assert_eq!(
        world.record()["destination"].as_str(),
        world.base.join("dest").to_str()
    );
}

// ---- An interrupted run ----

#[test]
fn a_run_stopped_midway_leaves_only_whole_files_and_the_next_run_finishes() {
    let world = World::empty();
    // Large enough that a debug build is still working when the signal lands.
    world.write_turns(3000, 32 * 1024);
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");

    let mut child = world
        .command()
        .args(["backup", "--to"])
        .arg(&folder)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    std::thread::sleep(std::time::Duration::from_millis(15));
    // SAFETY: `child` has not been waited on, so its pid is still this child's.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    child.wait().expect("wait");

    // Whatever has its final name is complete, whenever the signal landed.
    for name in segment_names(&backup_log(&folder)) {
        assert_eq!(
            fs::read(backup_log(&folder).join(&name)).expect("copy"),
            fs::read(world.log().join(&name)).expect("source"),
            "{name} has its final name and is not whole"
        );
    }

    ok(&world.backup(&folder));

    assert_eq!(segment_names(&backup_log(&folder)), closed);
    ok(&world.verify(&folder));
    let leftovers: Vec<String> = fs::read_dir(backup_log(&folder))
        .expect("read dir")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with("segment-"))
        .collect();
    assert_eq!(
        leftovers,
        Vec::<String>::new(),
        "no temporary file survives"
    );
}

/// A child that goes when the test does, whatever way the test ends: one left
/// blocked on a FIFO would hold a disk image mounted for ever.
struct Stopping(std::process::Child);

impl Drop for Stopping {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            // SAFETY: not yet reaped, so the pid is still this child's.
            unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM) };
            let _ = self.0.wait();
        }
    }
}

/// A closed "segment" that is a FIFO: the run blocks reading it, at a moment the
/// test chooses, with part of the file written. No timing is involved. Only a real
/// copy gets this far, so `folder` must be on a volume where no clone is made.
fn a_copy_caught_halfway_is_not_a_segment(world: &World, folder: &Path) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};

    let (closed, _) = split_open(world);
    let caught = closed.last().expect("a closed segment");
    let source = world.log().join(caught);
    let whole = fs::read(&source).expect("read");
    fs::remove_file(&source).expect("rm");
    let fifo = std::ffi::CString::new(source.to_str().expect("utf-8")).expect("path");
    // SAFETY: `fifo` is a NUL-terminated path that lives across the call.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "mkfifo");
    let mut child = Stopping(
        world
            .command()
            .args(["backup", "--to"])
            .arg(folder)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn"),
    );
    // Not blocking: a run that never reaches this file must fail the test, not
    // hang it. With no reader yet the open fails, so ask until there is one.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut feed = loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&source)
        {
            Ok(feed) => break feed,
            Err(err) => {
                assert!(Instant::now() < deadline, "the run never opened it: {err}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    feed.write_all(&whole[..whole.len() / 2])
        .expect("feed half");
    let log = backup_log(folder);
    let temp = log.join(format!(".{caught}.{}.tmp", child.0.id()));
    while fs::metadata(&temp).map_or(0, |meta| meta.len()) == 0 {
        assert!(Instant::now() < deadline, "no temporary file appeared");
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        !log.join(caught).exists(),
        "half a file must not have the segment's name"
    );
    assert!(!segment_names(&log).contains(caught));
    let glob = Command::new("/bin/sh")
        .args(["-c", r#"ls "$1"/*.jsonl"#, "sh"])
        .arg(&log)
        .output()
        .expect("run ls");
    assert!(
        !stdout(&glob).contains(caught.as_str()),
        "the manual's glob must not take it for one"
    );

    // SAFETY: not yet reaped, so the pid is still this child's.
    unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) };
    child.0.wait().expect("wait");
    drop(feed);
    assert!(temp.exists(), "a killed run cannot tidy up");
    assert!(!log.join(caught).exists());

    fs::remove_file(&source).expect("rm the fifo");
    fs::write(&source, &whole).expect("put the segment back");
    ok(&world.backup(folder));
    assert!(!temp.exists(), "the next run removes it");
    assert_eq!(fs::read(log.join(caught)).expect("read"), whole);
    ok(&world.verify(folder));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn half_a_copy_never_has_the_segments_name() {
    let world = World::with_log();
    a_copy_caught_halfway_is_not_a_segment(&world, &world.base.join("dest"));
}

/// Segments this size cannot be written under the `ulimit -f 1` below.
const LARGER_THAN_THE_LIMIT: u64 = 8 * 1024;

/// A write the OS refuses partway: a file-size limit stands in for a disk that
/// fills up after the free-space check passed. Only a real copy writes, so
/// `folder` must be on a volume where no clone is made.
fn a_copy_that_cannot_finish_leaves_nothing_that_looks_finished(world: &World, folder: &Path) {
    let shell = world.command();
    let limited = Command::new("/bin/sh")
        .args(["-c", r#"trap "" XFSZ; ulimit -f 1; exec "$@""#, "sh"])
        .arg(bin())
        .args(["backup", "--to"])
        .arg(folder)
        .envs(
            shell
                .get_envs()
                .filter_map(|(key, value)| Some((key.to_owned(), value?.to_owned()))),
        )
        .env_remove("XDG_STATE_HOME")
        .env_remove("CONBUS_NO_CACHE")
        .stdin(Stdio::null())
        .output()
        .expect("run under a file-size limit");

    assert_eq!(limited.status.code(), Some(1), "{}", stderr(&limited));
    assert!(stderr(&limited).contains("segment-00000000.jsonl"));
    let log = backup_log(folder);
    let left: Vec<String> = fs::read_dir(&log)
        .expect("read dir")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    assert_eq!(left, Vec::<String>::new(), "no half file, by any name");
    assert!(
        !world.root.join("backup.json").exists(),
        "a run that failed is not a good run"
    );

    ok(&world.backup(folder));
    ok(&world.verify(folder));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_write_the_os_refuses_leaves_no_half_file_and_no_record() {
    let world = World::empty();
    world.write_turns(40, LARGER_THAN_THE_LIMIT);
    let folder = world.base.join("dest");
    a_copy_that_cannot_finish_leaves_nothing_that_looks_finished(&world, &folder);
}

#[test]
fn a_half_written_temporary_file_is_not_a_segment_and_does_not_survive() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let log = backup_log(&folder);
    let half = log.join(format!(".{}.4242.tmp", closed[0]));
    fs::write(&half, b"half a segm").expect("plant");

    // Neither a reader of the backup nor the manual's glob takes it for one.
    ok(&world.verify(&folder));
    assert_eq!(segment_names(&log), closed);

    ok(&world.backup(&folder));
    assert!(
        !half.exists(),
        "the next run removes what the last one left"
    );
}

#[test]
fn a_segment_with_no_manifest_row_is_checked_against_the_log_and_kept() {
    // The state a run killed before writing the manifest leaves behind.
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let before = inodes(&backup_log(&folder));
    fs::remove_file(folder.join("conbus-backup/manifest.json")).expect("rm");

    let said = ok(&world.backup(&folder));

    assert!(said.contains("copied 0 segment(s)"), "{said}");
    assert_eq!(inodes(&backup_log(&folder)), before, "kept, not rewritten");
    assert_eq!(manifest_rows(&folder).len(), closed.len());
    ok(&world.verify(&folder));
}

#[test]
fn a_segment_with_no_manifest_row_that_differs_from_the_log_is_refused() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    fs::remove_file(folder.join("conbus-backup/manifest.json")).expect("rm");
    let copy = backup_log(&folder).join(&closed[0]);
    fs::write(&copy, b"not that segment\n").expect("write");

    refused(&world.backup(&folder), &closed[0]);

    assert_eq!(fs::read(&copy).expect("read"), b"not that segment\n");
}

#[test]
fn a_copy_with_no_row_and_no_segment_left_to_compare_is_kept() {
    // A run that died before its manifest, then a stray rm in the log. The copy
    // may be all that is left of those records.
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    fs::remove_file(folder.join("conbus-backup/manifest.json")).expect("rm");
    fs::remove_file(world.log().join(&closed[0])).expect("rm");

    let said = ok(&world.backup(&folder));

    assert!(said.contains("1 no longer in the log"), "{said}");
    assert_eq!(manifest_rows(&folder).len(), closed.len());
    ok(&world.verify(&folder));
}

// ---- Verify ----

#[test]
fn verify_passes_a_good_backup_and_changes_nothing() {
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let record = fs::read(world.root.join("backup.json")).expect("record");
    let held = fingerprint(&folder);

    let said = ok(&world.verify(&folder));

    assert!(said.contains("verified"), "{said}");
    assert_eq!(fingerprint(&folder), held);
    assert_eq!(fs::read(world.root.join("backup.json")).expect("r"), record);
}

#[test]
fn verify_names_a_flipped_byte_a_missing_file_and_a_short_file() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let sources = fingerprint(&world.log());
    let log = backup_log(&folder);

    let flipped = log.join(&closed[0]);
    let mut bytes = fs::read(&flipped).expect("read");
    bytes[7] ^= 0x01;
    fs::write(&flipped, &bytes).expect("flip");
    fs::remove_file(log.join(&closed[1])).expect("rm");
    let short = log.join(&closed[2]);
    let full = fs::metadata(&short).expect("meta").len();
    File::options()
        .write(true)
        .open(&short)
        .expect("open")
        .set_len(full - 1)
        .expect("truncate");

    let out = world.verify(&folder);

    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    let said = stderr(&out);
    for name in &closed[..3] {
        assert!(said.contains(name.as_str()), "must name {name}: {said}");
    }
    assert!(
        !said.contains(&closed[3]),
        "a good file is not named: {said}"
    );
    assert_eq!(
        fingerprint(&world.log()),
        sources,
        "damage to a copy, clone or not, never reaches the log"
    );
}

#[test]
fn a_damaged_copy_is_mended_from_the_log_once_it_is_deleted() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let flipped = backup_log(&folder).join(&closed[0]);
    let mut bytes = fs::read(&flipped).expect("read");
    bytes[7] ^= 0x01;
    fs::write(&flipped, &bytes).expect("flip");

    // A rerun does not read what is already there: that is what --verify is for.
    assert!(ok(&world.backup(&folder)).contains("copied 0 segment(s)"));
    assert_eq!(world.verify(&folder).status.code(), Some(1));

    fs::remove_file(&flipped).expect("rm");
    assert!(ok(&world.backup(&folder)).contains("copied 1 segment(s)"));
    ok(&world.verify(&folder));
}

#[test]
fn a_copy_cut_short_at_the_destination_is_mended_by_a_rerun() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let short = backup_log(&folder).join(&closed[0]);
    let full = fs::metadata(&short).expect("meta").len();
    File::options()
        .write(true)
        .open(&short)
        .expect("open")
        .set_len(full / 2)
        .expect("truncate");

    assert!(ok(&world.backup(&folder)).contains("copied 1 segment(s)"));
    ok(&world.verify(&folder));
}

#[test]
fn verify_names_a_segment_the_manifest_does_not_vouch_for() {
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let stray = "segment-99999999.jsonl";
    fs::write(backup_log(&folder).join(stray), b"{}\n").expect("plant");

    let out = world.verify(&folder);

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains(stray), "{}", stderr(&out));
}

#[test]
fn verify_ignores_what_an_os_leaves_beside_a_segment() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let log = backup_log(&folder);
    fs::write(log.join(".DS_Store"), b"x").expect("plant");
    fs::write(log.join(format!("._{}", closed[0])), b"x").expect("plant");

    ok(&world.verify(&folder));
}

#[test]
fn verify_without_a_backup_there_names_the_manifest_and_creates_nothing() {
    let world = World::with_log();
    let folder = world.base.join("not-there");

    let out = world.verify(&folder);

    refused(&out, "manifest.json");
    assert!(!folder.exists());

    let empty = world.base.join("empty");
    fs::create_dir(&empty).expect("dir");
    refused(&world.verify(&empty), "manifest.json");
    assert_eq!(fs::read_dir(&empty).expect("read").count(), 0);
}

#[test]
fn verify_says_a_manifest_that_does_not_read_is_damaged() {
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let manifest = folder.join("conbus-backup/manifest.json");
    fs::write(&manifest, b"{not json").expect("damage");

    refused(&world.verify(&folder), "manifest.json");
    // And a run does not paper over it by starting a new one.
    refused(&world.backup(&folder), "manifest.json");
    assert_eq!(fs::read(&manifest).expect("read"), b"{not json");
}

#[test]
fn a_manifest_row_cannot_walk_out_of_the_backup() {
    // The manifest is a file on a disk that may have been anywhere.
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let outside = world.base.join("segment-00000000.jsonl");
    fs::write(&outside, b"outside\n").expect("write");
    let manifest = serde_json::json!({
        "format": 1,
        "segments": [{
            "name": "../../../segment-00000000.jsonl",
            "size": 8,
            "sha256": sha256(&outside),
        }],
    });
    fs::write(
        folder.join("conbus-backup/manifest.json"),
        manifest.to_string(),
    )
    .expect("write");

    refused(&world.verify(&folder), "manifest.json");
    refused(&world.backup(&folder), "manifest.json");
}

#[test]
fn a_manifest_from_a_newer_build_is_refused_rather_than_guessed_at() {
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let manifest = folder.join("conbus-backup/manifest.json");
    fs::write(&manifest, br#"{"format":2,"segments":[]}"#).expect("write");

    refused(&world.verify(&folder), "manifest.json");
    refused(&world.backup(&folder), "manifest.json");
}

#[test]
fn verify_needs_no_data_root_at_all() {
    // Checking a disk on another machine is part of what a backup is for.
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    fs::remove_dir_all(&world.root).expect("rm");

    ok(&world.verify(&folder));

    assert!(!world.root.exists(), "and it does not make one");
}

// ---- The clone ----

#[cfg(target_os = "macos")]
#[test]
fn the_same_apfs_volume_gets_a_clone_and_it_says_so() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");

    let said = ok(&world.backup(&folder));

    assert!(
        said.contains(&format!("cloned {} of {}", closed.len(), closed.len())),
        "{said}"
    );
    assert!(said.contains("almost no space"), "{said}");
    assert!(said.contains(SAME_DISK), "{said}");
    // A clone is a file of its own: the source going takes nothing with it.
    fs::remove_file(world.log().join(&closed[0])).expect("rm");
    ok(&world.verify(&folder));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn elsewhere_it_is_a_plain_copy_and_it_does_not_claim_a_clone() {
    let world = World::with_log();

    let said = ok(&world.backup(&world.base.join("dest")));

    assert!(!said.contains("cloned"), "{said}");
    assert!(said.contains(SAME_DISK), "{said}");
}

// ---- The record, and doctor's line ----

#[test]
fn a_good_run_records_where_when_and_how_much() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    let before = Timestamp::now();

    ok(&world.backup(&folder));

    let record = world.record();
    assert_eq!(record["destination"].as_str(), folder.to_str());
    assert_eq!(record["segments"].as_u64(), Some(closed.len() as u64));
    let bytes: u64 = closed
        .iter()
        .map(|name| fs::metadata(world.log().join(name)).expect("meta").len())
        .sum();
    assert_eq!(record["bytes"].as_u64(), Some(bytes));
    assert_eq!(record["same_volume"].as_bool(), Some(true));
    let last_ok: Timestamp = record["last_ok"]
        .as_str()
        .expect("last_ok")
        .parse()
        .expect("a conbus timestamp");
    assert!(last_ok >= before);
}

#[test]
fn a_run_that_fails_does_not_move_the_last_good_time() {
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let record = fs::read(world.root.join("backup.json")).expect("record");

    let (closed, _) = split_open(&world);
    let source = world.log().join(&closed[0]);
    let full = fs::metadata(&source).expect("meta").len();
    File::options()
        .write(true)
        .open(&source)
        .expect("open")
        .set_len(full - 1)
        .expect("truncate");
    refused(&world.backup(&folder), &closed[0]);

    assert_eq!(fs::read(world.root.join("backup.json")).expect("r"), record);
}

#[test]
fn doctor_says_when_there_is_no_backup() {
    let world = World::with_log();
    assert_eq!(world.doctor_line(), "backup    none: this log has one copy");
}

#[test]
fn doctor_reads_the_record_and_never_the_destination() {
    let world = World::with_log();
    let three_hours = Timestamp::from_unix_seconds(
        Timestamp::now().seconds_since(Timestamp::from_unix_seconds(0).expect("epoch"))
            - 3 * 3_600
            - 5,
    )
    .expect("ts");
    // A disk that is not there. Looking for it would change the answer, or
    // spin up a sleeping disk for a report that must stay fast.
    let record = |same_volume: bool| {
        serde_json::json!({
            "destination": "/Volumes/No Such Disk",
            "last_ok": three_hours.to_string(),
            "segments": 1460,
            "bytes": 12_000_000_000u64,
            "same_volume": same_volume,
        })
        .to_string()
    };

    fs::write(world.root.join("backup.json"), record(false)).expect("write");
    assert_eq!(
        world.doctor_line(),
        "backup    last ok 3h ago to /Volumes/No Such Disk (1,460 segments)"
    );

    fs::write(world.root.join("backup.json"), record(true)).expect("write");
    assert_eq!(
        world.doctor_line(),
        format!("backup    last ok 3h ago to /Volumes/No Such Disk (1,460 segments) — {SAME_DISK}")
    );
}

#[test]
fn doctor_says_a_record_it_cannot_read_rather_than_guessing() {
    let world = World::with_log();
    fs::write(world.root.join("backup.json"), b"{not json").expect("write");

    let line = world.doctor_line();

    assert!(line.starts_with("backup    "), "{line}");
    assert!(line.contains("backup.json"), "{line}");
    assert!(!line.contains("last ok"), "{line}");
}

#[test]
fn doctor_after_a_real_run_on_the_same_volume_says_same_disk() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));

    let line = world.doctor_line();

    assert!(line.contains(folder.to_str().expect("utf-8")), "{line}");
    assert!(
        line.contains(&format!("({} segments)", closed.len())),
        "{line}"
    );
    assert!(line.ends_with(SAME_DISK), "{line}");
}

// ---- What it refuses ----

#[test]
fn a_destination_inside_the_root_is_refused() {
    let world = World::with_log();
    let inside = [
        world.root.clone(),
        world.root.join("backups"),
        world.log(),
        world.log().join("backups"),
    ];
    for to in inside {
        refused(&world.backup(&to), "inside the data root");
    }
    assert!(!world.root.join("backups").exists());
    assert!(!world.log().join("backups").exists());
}

#[test]
fn a_destination_holding_the_root_is_refused() {
    let world = World::with_log();
    for to in [world.base.clone(), PathBuf::from("/")] {
        refused(&world.backup(&to), "holds the data root");
    }
    assert!(!world.base.join("conbus-backup").exists());
}

#[test]
fn the_same_refusals_hold_through_a_symlink() {
    let world = World::with_log();
    let into_root = world.base.join("into-root");
    symlink(&world.root, &into_root).expect("symlink");
    refused(
        &world.backup(&into_root.join("backups")),
        "inside the data root",
    );
    refused(&world.backup(&into_root), "inside the data root");
    assert!(!world.root.join("backups").exists());

    // The root itself reached through a symlink.
    let linked_root = world.base.join("linked-root");
    symlink(&world.root, &linked_root).expect("symlink");
    let out = world
        .command()
        .env("CONBUS_HOME", &linked_root)
        .args(["backup", "--to"])
        .arg(world.root.join("backups"))
        .output()
        .expect("run");
    refused(&out, "inside the data root");
}

#[cfg(target_os = "macos")]
#[test]
fn a_root_spelled_in_another_case_is_still_the_root() {
    // The default Mac volume does not care about case, so text would be fooled.
    let world = World::with_log();
    let shouted = world.base.join("ROOT").join("backups");

    let out = world.backup(&shouted);

    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(!world.root.join("backups").exists());
}

fn cloud_refused(out: &Output) {
    refused(out, "cloud-synced");
    let said = stderr(out);
    assert!(said.contains("do not leave this machine"), "{said}");
    assert!(said.contains("not offered yet"), "{said}");
}

const CLOUD_FOLDERS: &[&str] = &[
    "Library/Mobile Documents",
    "Library/Mobile Documents/com~apple~CloudDocs",
    "Library/CloudStorage",
    "Library/CloudStorage/GoogleDrive-someone",
    "Dropbox",
    "Google Drive",
    "OneDrive",
    "OneDrive - Acme",
];

#[test]
fn every_cloud_folder_is_refused_and_nothing_lands_there() {
    let world = World::with_log();
    for folder in CLOUD_FOLDERS {
        let cloud = world.home.join(folder);
        fs::create_dir_all(&cloud).expect("dir");
        cloud_refused(&world.backup(&cloud));
        cloud_refused(&world.backup(&cloud.join("backups")));
        assert!(!cloud.join("backups").exists(), "{folder}");
        assert!(!cloud.join("conbus-backup").exists(), "{folder}");
    }
}

#[test]
fn a_cloud_folder_is_refused_before_it_exists() {
    // A client installed tomorrow syncs what is already there.
    let world = World::with_log();
    for folder in [
        "Dropbox",
        "DROPBOX",
        "Google Drive",
        "google drive",
        "OneDrive",
        "ONEDRIVE - Acme",
        "OneDriveBackups",
    ] {
        let cloud = world.home.join(folder);
        cloud_refused(&world.backup(&cloud));
        assert!(!cloud.exists(), "{folder}");
    }
}

#[test]
fn a_cloud_folder_is_refused_whatever_case_it_is_spelled_in() {
    let world = World::with_log();
    for folder in [
        "dropbox",
        "GOOGLE DRIVE",
        "onedrive",
        "library/cloudstorage",
    ] {
        let parent = world.home.join(folder);
        fs::create_dir_all(&parent).expect("dir");
        cloud_refused(&world.backup(&parent));
    }
}

#[test]
fn a_symlink_does_not_get_a_folder_past_the_cloud_check() {
    let world = World::with_log();
    for folder in CLOUD_FOLDERS {
        let cloud = world.home.join(folder);
        fs::create_dir_all(&cloud).expect("dir");
        let link = world
            .base
            .join(format!("link-{}", folder.replace('/', "-")));
        symlink(&cloud, &link).expect("symlink");
        cloud_refused(&world.backup(&link));
        cloud_refused(&world.backup(&link.join("backups")));
        assert!(!cloud.join("backups").exists(), "{folder}");
    }
}

#[test]
fn a_cloud_folder_that_is_itself_a_symlink_is_followed() {
    // `~/Dropbox` moved to another disk, with a link left in its place: the
    // files there still sync, by whichever spelling they are reached.
    let world = World::with_log();
    let moved = world.base.join("moved-dropbox");
    fs::create_dir(&moved).expect("dir");
    symlink(&moved, world.home.join("Dropbox")).expect("symlink");
    let moved_onedrive = world.base.join("moved-onedrive");
    fs::create_dir(&moved_onedrive).expect("dir");
    symlink(&moved_onedrive, world.home.join("OneDrive - Acme")).expect("symlink");

    cloud_refused(&world.backup(&moved.join("backups")));
    cloud_refused(&world.backup(&moved_onedrive.join("backups")));
    cloud_refused(&world.backup(&world.home.join("Dropbox").join("backups")));
    assert!(!moved.join("backups").exists());
    assert!(!moved_onedrive.join("backups").exists());
}

#[test]
fn a_planted_symlink_inside_the_backup_does_not_lead_into_a_cloud_folder() {
    let world = World::with_log();
    let cloud = world.home.join("Dropbox");
    fs::create_dir_all(&cloud).expect("dir");
    let folder = world.base.join("dest");
    fs::create_dir(&folder).expect("dir");
    symlink(&cloud, folder.join("conbus-backup")).expect("symlink");

    cloud_refused(&world.backup(&folder));

    assert_eq!(fs::read_dir(&cloud).expect("read").count(), 0);
}

#[test]
fn a_folder_merely_named_like_a_cloud_folder_is_not_refused() {
    let world = World::with_log();
    for folder in [
        "Dropbox-notes",
        "not-cloud",
        "Library/Application Support/Dropbox",
    ] {
        let parent = world.home.join(folder);
        fs::create_dir_all(&parent).expect("dir");
        ok(&world.backup(&parent));
    }
    // And one outside the home altogether, whatever it is called.
    let elsewhere = world.base.join("Dropbox");
    fs::create_dir(&elsewhere).expect("dir");
    ok(&world.backup(&elsewhere));
}

#[test]
fn there_is_no_override_flag() {
    let world = World::with_log();
    let cloud = world.home.join("Dropbox");
    fs::create_dir(&cloud).expect("dir");
    for flag in ["--force", "--yes", "--allow-cloud"] {
        let out = world
            .command()
            .args(["backup", flag, "--to"])
            .arg(&cloud)
            .output()
            .expect("run");
        assert_eq!(out.status.code(), Some(2), "{flag}: {}", stderr(&out));
    }
}

#[test]
fn with_no_home_to_check_against_it_refuses_rather_than_hoping() {
    let world = World::with_log();
    let folder = world.base.join("dest");
    let out = world
        .command()
        .env_remove("HOME")
        .args(["backup", "--to"])
        .arg(&folder)
        .output()
        .expect("run");

    refused(&out, "HOME");
    assert!(!folder.exists());
}

#[test]
fn a_destination_whose_parent_is_missing_is_refused_and_nothing_is_created() {
    let world = World::with_log();
    let missing = world.base.join("unplugged");
    for to in [missing.join("backups"), missing.join("a b").join("c")] {
        refused(&world.backup(&to), "does not exist");
    }
    assert!(!missing.exists());
}

#[test]
fn a_disk_that_is_not_plugged_in_is_refused_and_no_folder_appears_for_it() {
    // A folder made at `/Volumes/My Disk` with the disk unplugged would sit on
    // the internal disk under a name that looks external. Whether `/Volumes` lets
    // this account try depends on the Mac, so every spelling has to be refused by
    // name rather than left to a permission error.
    let world = World::with_log();
    let name = format!("/Volumes/conbus test absent {}", std::process::id());
    let volume = Path::new(&name);
    for to in [
        volume.to_path_buf(),
        volume.join("backups"),
        volume.join("conbus").join("backups"),
    ] {
        let out = world.backup(&to);
        assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
        let said = stderr(&out);
        assert!(
            said.contains("not mounted") || said.contains("does not exist"),
            "{said}"
        );
        assert!(said.contains(&name), "{said}");
        assert!(!volume.exists(), "created {name} on the internal disk");
    }
    assert!(!world.root.join("backup.json").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn the_folder_that_holds_the_mounted_disks_is_not_a_disk() {
    let world = World::with_log();

    let out = world.backup(Path::new("/Volumes"));

    refused(&out, "/Volumes");
    assert!(!Path::new("/Volumes/conbus-backup").exists());
}

#[test]
fn a_symlink_to_a_disk_that_is_not_there_is_refused() {
    let world = World::with_log();
    let target = world.base.join("gone").join("disk");
    let link = world.base.join("disk");
    symlink(&target, &link).expect("symlink");

    refused(&world.backup(&link), "not there");
    refused(&world.backup(&link.join("backups")), "does not exist");

    assert!(!world.base.join("gone").exists());
    assert!(fs::symlink_metadata(&link).expect("the link").is_symlink());
}

#[test]
fn a_destination_that_is_a_file_is_refused() {
    let world = World::with_log();
    let file = world.base.join("a-file");
    fs::write(&file, b"mine").expect("write");

    refused(&world.backup(&file), "not a folder");
    let out = world.backup(&file.join("backups"));
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));

    assert_eq!(fs::read(&file).expect("read"), b"mine");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_volume_too_small_for_the_copy_is_refused_before_anything_is_copied() {
    // A sparse closed segment larger than the free space on this very volume:
    // the size says it cannot fit, and refusing must not need to read it.
    let world = World::with_log();
    let df = Command::new("df")
        .args(["-Pk"])
        .arg(&world.base)
        .output()
        .expect("df");
    let df = stdout(&df);
    let free_kib: u64 = df
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|free| free.parse().ok())
        .expect("df's Available column");
    let (closed, _) = split_open(&world);
    let huge = world.log().join(&closed[0]);
    File::options()
        .write(true)
        .open(&huge)
        .expect("open")
        .set_len((free_kib + 1024 * 1024) * 1024)
        .expect("a sparse segment");
    let folder = world.base.join("dest");

    refused(&world.backup(&folder), "not enough free space");

    assert!(
        !folder.exists(),
        "checked before anything is made or copied"
    );
    assert!(!world.root.join("backup.json").exists());
}

#[test]
fn a_second_run_at_once_is_refused() {
    let world = World::with_log();
    let held = File::open(world.log()).expect("open the log directory");
    held.try_lock()
        .expect("hold the lock a running backup holds");
    let folder = world.base.join("dest");

    refused(&world.backup(&folder), "another conbus backup");
    assert!(!folder.exists());

    drop(held);
    // Another test thread forking right now keeps a copy of that descriptor until
    // its child execs, and the lock with it. Only a test shares a process that way.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut again = world.backup(&folder);
    while !again.status.success() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        again = world.backup(&folder);
    }
    ok(&again);
}

#[test]
fn usage_mistakes_exit_two() {
    let world = World::with_log();
    for args in [
        &["backup"][..],
        &["backup", "--to"][..],
        &["backup", "--to", ""][..],
        &["backup", "--verify"][..],
        &["backup", "--to", "/tmp/x", "stray"][..],
        &["backup", "--bogus", "--to", "/tmp/x"][..],
    ] {
        let out = world.conbus(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
}

// ---- A folder with a space, the real shape of an external disk's name ----

#[test]
fn a_folder_with_a_space_in_its_name_works_all_the_way_through() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("My Disk");
    fs::create_dir(&folder).expect("dir");
    let (before_log, _) = world.reads();

    ok(&world.backup(&folder));
    ok(&world.verify(&folder));

    assert_eq!(segment_names(&backup_log(&folder)), closed);
    assert_eq!(world.record()["destination"].as_str(), folder.to_str());
    assert!(
        world
            .doctor_line()
            .contains(&format!("to {} (", folder.display())),
        "{}",
        world.doctor_line()
    );

    // And back again through the manual's restore, with its quoting.
    fs::remove_dir_all(world.log()).expect("rm");
    restore(&folder, &world.root);
    assert_eq!(segment_names(&world.log()), closed);
    let (after_log, _) = world.reads();
    assert!(before_log.starts_with(&after_log));
}

#[test]
fn a_destination_named_with_a_space_that_does_not_exist_is_made() {
    let world = World::with_log();
    let folder = world.base.join("my backups");

    ok(&world.backup(&folder));

    ok(&world.verify(&folder));
}

// ---- Saying what a backup is not ----

fn warns(said: &str) -> bool {
    said.contains("Keychain")
        && said.contains("does not copy")
        && said.contains("another machine")
        && said.contains("plain text")
        && said.contains("unencrypted")
}

#[test]
fn a_first_run_to_a_new_destination_says_what_a_backup_is_not() {
    let world = World::with_log();
    let folder = world.base.join("dest");

    let first = ok(&world.backup(&folder));
    assert!(warns(&first), "the first run says both: {first}");
    assert!(first.contains("full disk"), "{first}");

    let second = ok(&world.backup(&folder));
    assert!(!warns(&second), "once is enough for one folder: {second}");

    let another = ok(&world.backup(&world.base.join("another")));
    assert!(warns(&another), "a new destination hears it again");
}

// ---- A destination that is not this log's backup ----

#[test]
fn a_log_that_is_not_the_one_in_the_backup_is_refused_and_the_backup_is_untouched() {
    // A second root, or a new log after a lost one that was never restored,
    // pointed at the old folder. Overwriting would destroy the only good copy.
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let held = fingerprint(&folder);

    let other = World::empty();
    other.write_turns(30, SMALL_SEGMENT + 77);
    let out = other.backup(&folder);

    refused(&out, "differs");
    assert_eq!(fingerprint(&folder), held);
    assert!(!other.root.join("backup.json").exists());
}

#[test]
fn a_closed_segment_that_shrank_is_refused_and_the_good_copy_is_kept() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let held = fingerprint(&folder);
    let source = world.log().join(&closed[1]);
    let full = fs::metadata(&source).expect("meta").len();
    File::options()
        .write(true)
        .open(&source)
        .expect("open")
        .set_len(full / 2)
        .expect("truncate");

    let out = world.backup(&folder);

    refused(&out, &closed[1]);
    refused(&out, "differs");
    assert_eq!(fingerprint(&folder), held);
}

#[test]
fn a_closed_segment_that_grew_into_other_bytes_is_refused() {
    use std::io::Write;
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let held = fingerprint(&folder);
    // Longer than the copy, and not the copy plus more.
    let source = world.log().join(&closed[1]);
    let mut bytes = fs::read(&source).expect("read");
    bytes[0] ^= 0x01;
    fs::write(&source, &bytes).expect("write");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&source)
        .expect("open");
    file.write_all(b"more\n").expect("append");
    drop(file);

    refused(&world.backup(&folder), "differs");

    assert_eq!(fingerprint(&folder), held);
}

#[test]
fn segments_gone_from_the_log_stay_in_the_backup_and_it_says_so() {
    // No deletion, ever: this run may be the one thing left after a stray rm.
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let held = fingerprint(&backup_log(&folder));
    fs::remove_file(world.log().join(&closed[0])).expect("rm");
    fs::remove_file(world.log().join(&closed[1])).expect("rm");

    let said = ok(&world.backup(&folder));

    assert!(said.contains("2 no longer in the log"), "{said}");
    assert_eq!(fingerprint(&backup_log(&folder)), held);
    assert_eq!(manifest_rows(&folder).len(), closed.len());
    assert_eq!(
        world.record()["segments"].as_u64(),
        Some(closed.len() as u64)
    );
    ok(&world.verify(&folder));
}

// ---- Restore: the manual's copy, and what the two reads say after it ----

/// The only state where nothing is newer than the backup: the recorder rolled
/// to a new segment and has not written to it yet.
fn roll_with_nothing_written(world: &World) {
    let names = segment_names(&world.log());
    let last: u64 = names
        .last()
        .and_then(|name| name.strip_prefix("segment-"))
        .and_then(|name| name.strip_suffix(".jsonl"))
        .and_then(|index| index.parse().ok())
        .expect("an index");
    File::create(world.log().join(format!("segment-{:08}.jsonl", last + 1))).expect("create");
}

#[test]
fn with_nothing_newer_a_deleted_log_restores_to_identical_reads() {
    let world = World::with_log();
    roll_with_nothing_written(&world);
    let before = world.reads();
    assert!(!before.0.is_empty() && !before.1.is_empty());
    let folder = world.base.join("dest");
    let said = ok(&world.backup(&folder));
    assert!(
        said.contains("newer than the backup: 0 record(s)"),
        "{said}"
    );

    fs::remove_dir_all(world.log()).expect("delete the log");
    restore(&folder, &world.root);

    assert_eq!(world.reads(), before);
    assert_eq!(mode(&world.log()), 0o700);
    for name in segment_names(&world.log()) {
        assert_eq!(mode(&world.log().join(&name)), 0o600, "{name}");
    }
}

#[test]
fn losing_the_whole_root_restores_to_identical_reads_too() {
    // Not just the log: the derived cache is gone as well, as on a new Mac.
    let world = World::with_log();
    roll_with_nothing_written(&world);
    let before = world.reads();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));

    fs::remove_dir_all(&world.root).expect("delete the root");
    restore(&folder, &world.root);

    assert_eq!(world.reads(), before);
}

#[test]
fn deleting_the_log_loses_exactly_the_records_the_run_called_newer() {
    // The backup never holds the open segment, so a total loss cannot read as
    // it did before. What it costs has to be exactly what the run reported, and
    // the rest has to read exactly as the closed segments alone read.
    let world = World::with_log();
    let (closed, open) = split_open(&world);
    let (full_log, _) = world.reads();

    let closed_only = World::empty();
    fs::create_dir(closed_only.log()).expect("log");
    for name in &closed {
        fs::copy(world.log().join(name), closed_only.log().join(name)).expect("copy");
    }
    let closed_reads = closed_only.reads();

    let folder = world.base.join("dest");
    let said = ok(&world.backup(&folder));
    let newer = fs::read_to_string(world.log().join(&open))
        .expect("read")
        .lines()
        .count();
    assert!(newer > 0);
    assert!(
        said.contains(&format!("newer than the backup: {newer} record(s)")),
        "{said}"
    );

    fs::remove_dir_all(world.log()).expect("delete the log");
    restore(&folder, &world.root);

    let restored = world.reads();
    assert_eq!(restored, closed_reads);
    let full: Vec<&str> = full_log.lines().collect();
    let kept: Vec<&str> = restored.0.lines().collect();
    assert_eq!(kept.len(), full.len() - newer);
    assert_eq!(kept, full[..kept.len()], "the oldest records, unchanged");
}

#[test]
fn a_partly_missing_log_restores_to_identical_reads() {
    // A stray rm of some closed segments, with the open one still there.
    let world = World::with_log();
    let (closed, open) = split_open(&world);
    let before = world.reads();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let open_bytes = fs::read(world.log().join(&open)).expect("read");
    for name in [&closed[0], &closed[2]] {
        fs::remove_file(world.log().join(name)).expect("rm");
    }
    assert_ne!(
        world.reads().0,
        before.0,
        "the loss shows before the restore"
    );

    restore(&folder, &world.root);

    assert_eq!(world.reads(), before);
    assert_eq!(
        fs::read(world.log().join(&open)).expect("read"),
        open_bytes,
        "the open segment is not the backup's to touch"
    );
}

#[test]
fn an_empty_segment_does_not_stand_in_for_a_real_one_in_a_restore() {
    // A stray `rm` of every segment, then `doctor`, the natural first move. Its
    // writable probe makes an empty segment-00000000.jsonl, and `cp -n` alone
    // would keep that over the real one: a third of the log gone, with no word.
    let world = World::with_log();
    roll_with_nothing_written(&world);
    let before = world.reads();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    for name in segment_names(&world.log()) {
        fs::remove_file(world.log().join(name)).expect("rm");
    }
    ok(&world.conbus(&["doctor"]));

    restore(&folder, &world.root);

    assert_eq!(world.reads(), before);
}

#[test]
fn a_run_after_a_restore_that_went_wrong_says_so() {
    // The safety net the manual names: a segment that did not come back fully
    // is refused by the next run rather than passed over.
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    File::create(world.log().join(&closed[0])).expect("an empty segment");

    let out = world.backup(&folder);

    refused(&out, "differs");
    refused(&out, &closed[0]);
}

#[test]
fn an_overlap_of_identical_segments_changes_nothing() {
    // Every closed segment is in both places, with no records doubled: a
    // segment has one name, so a restore can only put back what is missing.
    let world = World::with_log();
    let before = world.reads();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    let log_before = fingerprint(&world.log());

    restore(&folder, &world.root);

    assert_eq!(fingerprint(&world.log()), log_before);
    assert_eq!(world.reads(), before);
}

#[test]
fn an_overlap_never_overwrites_records_the_backup_does_not_have() {
    // After a restore the newest restored segment is the open one again and
    // grows. Restoring a second time must not cut those records off, which is
    // what a plain `cp` would do and why the manual says `cp -n`.
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    fs::remove_dir_all(world.log()).expect("delete the log");
    restore(&folder, &world.root);
    world.write_turns(1, 1024 * 1024);
    let grown = world.reads();

    restore(&folder, &world.root);

    assert_eq!(world.reads(), grown);
}

#[test]
fn a_damaged_segment_is_replaced_by_deleting_it_and_restoring() {
    // Same length, one line unreadable. A run keeps the good copy without
    // noticing, the log says what is wrong, and the manual's way out works even
    // after a read has cached the damage under the same name and length.
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let folder = world.base.join("dest");
    let before = world.reads();
    ok(&world.backup(&folder));
    let source = world.log().join(&closed[1]);
    let mut bytes = fs::read(&source).expect("read");
    bytes[0] = b'z';
    fs::write(&source, &bytes).expect("damage");
    fs::remove_dir_all(world.root.join("cache")).expect("no cache from before");
    assert_ne!(world.reads(), before, "the read builds its cache from this");

    assert!(ok(&world.backup(&folder)).contains("copied 0 segment(s)"));
    ok(&world.verify(&folder));

    fs::remove_file(&source).expect("rm");
    restore(&folder, &world.root);

    assert_eq!(world.reads(), before);
}

#[test]
fn recording_after_a_restore_continues_and_so_does_the_backup() {
    let world = World::with_log();
    let (closed, _) = split_open(&world);
    let last_restored = closed.last().expect("a closed segment").clone();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    fs::remove_dir_all(world.log()).expect("delete the log");
    restore(&folder, &world.root);

    // The recorder's writer on the restored log: it resumes the sequence, and
    // the last restored segment takes the records, so it is longer than the
    // copy in the backup.
    let highest = |text: &str| -> u64 {
        text.lines()
            .map(|line| {
                let record: serde_json::Value = serde_json::from_str(line).expect("json");
                record["seq"].as_u64().expect("seq")
            })
            .max()
            .expect("records")
    };
    let restored_highest = highest(&world.reads().0);
    world.write_turns(1, 1024 * 1024);
    let (log, _) = world.reads();
    assert_eq!(
        log.lines()
            .map(|line| {
                let record: serde_json::Value = serde_json::from_str(line).expect("json");
                record["seq"].as_u64().expect("seq")
            })
            .collect::<Vec<u64>>(),
        (0..=highest(&log)).collect::<Vec<u64>>(),
        "the sequence goes on with no number used twice"
    );
    assert!(highest(&log) > restored_highest);
    assert_ne!(
        fs::metadata(world.log().join(&last_restored))
            .expect("meta")
            .len(),
        manifest_rows(&folder)[&last_restored].0,
        "the fixture must grow the restored segment"
    );

    // While it is the open one, a backup leaves it and counts what it gained.
    let said = ok(&world.backup(&folder));
    assert!(said.contains("copied 0 segment(s)"), "{said}");
    assert!(
        said.contains("newer than the backup: 3 record(s)"),
        "{said}"
    );

    // Once it rolls, the copy is a prefix of it: that is growth, not damage,
    // and the backup takes the longer one rather than refusing for ever.
    world.write_turns(2, 64);
    let (closed, _) = split_open(&world);
    assert!(closed.contains(&last_restored));
    ok(&world.backup(&folder));
    assert_eq!(
        fs::read(backup_log(&folder).join(&last_restored)).expect("copy"),
        fs::read(world.log().join(&last_restored)).expect("source"),
    );
    ok(&world.verify(&folder));
    assert_eq!(segment_names(&backup_log(&folder)), closed);
}

#[test]
fn a_backup_of_a_restored_log_is_whole_again_after_a_second_loss() {
    let world = World::with_log();
    let folder = world.base.join("dest");
    ok(&world.backup(&folder));
    fs::remove_dir_all(world.log()).expect("delete the log");
    restore(&folder, &world.root);
    world.write_turns(1, 1024 * 1024);
    world.write_turns(2, 64);
    roll_with_nothing_written(&world);
    let before = world.reads();
    ok(&world.backup(&folder));

    fs::remove_dir_all(world.log()).expect("delete the log again");
    restore(&folder, &world.root);

    assert_eq!(world.reads(), before);
}

// ---- Real separate volumes, on a Mac ----

#[cfg(target_os = "macos")]
mod volumes {
    use super::*;

    /// A real volume on a disk image, mounted under the test's own folder.
    struct Image {
        mount: PathBuf,
        device: String,
        attached: std::cell::Cell<bool>,
    }

    impl Image {
        fn attach(base: &Path, filesystem: &str, size: &str) -> Self {
            let image = base.join("disk.dmg");
            let created = Command::new("hdiutil")
                .args(["create", "-quiet", "-size", size, "-fs", filesystem])
                .args(["-volname", "cbtest"])
                .arg(&image)
                .status()
                .expect("hdiutil create");
            assert!(created.success(), "hdiutil create");
            let mount = base.join("mnt");
            fs::create_dir(&mount).expect("mountpoint");
            let attached = Command::new("hdiutil")
                .args(["attach", "-nobrowse", "-noverify", "-mountpoint"])
                .arg(&mount)
                .arg(&image)
                .output()
                .expect("hdiutil attach");
            assert!(attached.status.success(), "hdiutil attach");
            // The first line names the image's whole disk.
            let device = stdout(&attached)
                .split_whitespace()
                .next()
                .filter(|device| device.starts_with("/dev/disk"))
                .expect("hdiutil names the device")
                .to_string();
            Image {
                mount,
                device,
                attached: std::cell::Cell::new(true),
            }
        }

        /// By device, not mount point: detaching a mount point can unmount it and
        /// still fail to release the device, which then holds its deleted image's
        /// space until a reboot. Once this succeeds the number may be reused, by
        /// a real disk too, so it is never tried again.
        fn detach(&self) {
            for attempt in 0..40 {
                if !self.attached.get() || !Path::new(&self.device).exists() {
                    break;
                }
                let mut detach = Command::new("hdiutil");
                detach.args(["detach", "-quiet"]);
                if attempt >= 20 {
                    detach.arg("-force");
                }
                if detach.arg(&self.device).status().is_ok_and(|s| s.success()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            self.attached.set(false);
        }
    }

    impl Drop for Image {
        fn drop(&mut self) {
            self.detach();
        }
    }

    #[test]
    fn a_volume_too_small_for_the_copy_is_refused_before_anything_is_copied() {
        let world = World::with_log();
        let image = Image::attach(&world.base, "HFS+", "16m");
        let (closed, _) = split_open(&world);
        // Sparse, so no real disk is spent saying it would not fit.
        File::options()
            .write(true)
            .open(world.log().join(&closed[0]))
            .expect("open")
            .set_len(64 * 1024 * 1024)
            .expect("a sparse segment");
        let folder = image.mount.join("backups");

        refused(&world.backup(&folder), "not enough free space");

        assert!(!folder.exists(), "checked before anything is made");
        assert!(!world.root.join("backup.json").exists());
    }

    #[test]
    fn another_volume_gets_a_real_copy_and_no_same_disk_warning() {
        let world = World::with_log();
        let (closed, _) = split_open(&world);
        let image = Image::attach(&world.base, "HFS+", "16m");
        let folder = image.mount.join("backups");

        let said = ok(&world.backup(&folder));

        assert!(!said.contains("cloned"), "{said}");
        assert!(!said.contains("same disk"), "{said}");
        assert_eq!(segment_names(&backup_log(&folder)), closed);
        assert_eq!(world.record()["same_volume"].as_bool(), Some(false));
        assert!(!world.doctor_line().contains("same disk"));
        ok(&world.verify(&folder));
    }

    #[test]
    fn an_apfs_volume_that_is_not_the_logs_gets_a_real_copy_too() {
        // Both sides clone-capable, and a clone still cannot cross volumes.
        let world = World::with_log();
        let image = Image::attach(&world.base, "APFS", "64m");
        let folder = image.mount.join("backups");

        let said = ok(&world.backup(&folder));

        assert!(!said.contains("cloned"), "{said}");
        assert!(!said.contains("same disk"), "{said}");
        ok(&world.verify(&folder));
    }

    #[test]
    fn half_a_copy_never_has_the_segments_name() {
        let world = World::with_log();
        let image = Image::attach(&world.base, "HFS+", "16m");
        a_copy_caught_halfway_is_not_a_segment(&world, &image.mount.join("backups"));
    }

    #[test]
    fn a_write_the_os_refuses_leaves_no_half_file_and_no_record() {
        let world = World::empty();
        world.write_turns(40, LARGER_THAN_THE_LIMIT);
        let image = Image::attach(&world.base, "HFS+", "16m");
        let folder = image.mount.join("backups");
        a_copy_that_cannot_finish_leaves_nothing_that_looks_finished(&world, &folder);
    }

    #[test]
    fn a_disk_that_went_away_leaving_its_folder_behind_is_refused() {
        // The mount folder stays once the disk is gone, as `/mnt/disk` does on
        // Linux and a stale `/Volumes/Name` can on a Mac. The folder is now on
        // the log's own disk, which is the one thing a backup there cannot be.
        let world = World::with_log();
        let image = Image::attach(&world.base, "HFS+", "16m");
        ok(&world.backup(&image.mount));
        image.detach();
        fs::create_dir_all(&image.mount).expect("the folder stays behind");

        refused(&world.backup(&image.mount), "not mounted");

        assert_eq!(fs::read_dir(&image.mount).expect("read").count(), 0);
    }
}
