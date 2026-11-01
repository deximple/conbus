//! `conbus backup`: a second copy of the log, made when a person asks for one.
//!
//! Nothing in conbus runs this. A copy made behind someone's back would outlive
//! them deleting `~/.conbus` to remove their prompts, so only the command line
//! reaches this module, and no recorder or hook path does.
//!
//! It copies closed segments: every one but the newest, the line `conbus log
//! index` draws, because only those have stopped changing. Each is written
//! under a temporary name, read back, and only then renamed, so a segment's real
//! name at the destination always means a whole file. The manifest is written
//! last; a file with its real name and no row is what a run that died leaves
//! behind, and the next run checks it against the log and keeps it.
//!
//! A copy is never overwritten with different bytes. The one exception is a copy
//! that is an exact prefix of the log's segment: after a restore the newest
//! restored segment is open again and grows, and taking the longer file loses
//! nothing.

mod copy;
mod destination;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use conbus_core::clock::{Timestamp, ago};
use conbus_core::paths::Paths;
use conbus_core::segment::SegmentLog;
use serde::{Deserialize, Serialize};

use crate::report::gib;
use copy::{Job, digest_file, digest_start, write_atomic};

const BACKUP_DIR: &str = "conbus-backup";
const MANIFEST: &str = "manifest.json";
const MANIFEST_FORMAT: u32 = 1;

const SAME_DISK: &str = "same disk: protects against deletion, not a failed disk";
const ONE_COPY: &str = "backup    none: this log has one copy";

const FIRST_RUN_NOTES: &[&str] = &[
    "note      prompts and replies are plain text in this backup: an unencrypted \
     external disk exposes them to whoever holds it",
    "note      tool bodies sealed under encrypted_tools need the log-kek key in the login \
     Keychain of the Mac that wrote them, which a backup does not copy: on another machine \
     they stay unreadable",
    "note      a backup does not stop a full disk from losing new records",
];

/// Where a backup keeps its files under the folder a person names.
struct Layout {
    dir: PathBuf,
    log: PathBuf,
    manifest: PathBuf,
}

impl Layout {
    fn of(folder: &Path) -> Self {
        let dir = folder.join(BACKUP_DIR);
        Layout {
            log: dir.join("log"),
            manifest: dir.join(MANIFEST),
            dir,
        }
    }

    /// Make the folders, one level at a time and never a parent: a disk that
    /// vanished after the checks must fail here, not be conjured on another one.
    fn create(&self, folder: &Path) -> io::Result<()> {
        for dir in [folder, &self.dir, &self.log] {
            match fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => {}
                Err(err) => return Err(named(dir)(err)),
            }
        }
        // `folder` may be a disk's root or anything else of the person's; these
        // two are ours, and may be left open by an older copy of them. A volume
        // without modes refuses this or ignores it. That is one reason the first
        // run says what an external disk exposes.
        for dir in [&self.dir, &self.log] {
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }
}

/// One segment as the manifest vouches for it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
struct Row {
    name: String,
    size: u64,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: u32,
    segments: Vec<Row>,
}

type Rows = BTreeMap<String, Row>;

/// `<root>/backup.json`: the last good run. All `doctor` ever reads.
#[derive(Serialize, Deserialize)]
struct Record {
    destination: String,
    last_ok: Timestamp,
    segments: u64,
    bytes: u64,
    same_volume: bool,
}

fn refuse(why: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("backup refused: {why}"),
    )
}

fn named(path: &Path) -> impl Fn(io::Error) -> io::Error {
    move |err| io::Error::new(err.kind(), format!("{}: {err}", path.display()))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_string()
}

fn keyed(row: Row) -> (String, Row) {
    (row.name.clone(), row)
}

pub fn run(paths: &Paths, to: &str, verify_only: bool) -> io::Result<()> {
    if verify_only {
        return verify(Path::new(to));
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    copy_to(paths, Path::new(to), home.as_deref())
}

fn copy_to(paths: &Paths, to: &Path, home: Option<&Path>) -> io::Result<()> {
    let log_dir = paths.log_dir();
    if !log_dir.is_dir() {
        return Err(refuse(format!(
            "no log to back up at {}",
            log_dir.display()
        )));
    }
    let folder = destination::check(paths.root(), to, home)?;
    let _running = hold_lock(&log_dir)?;
    let layout = Layout::of(&folder);
    let segments = SegmentLog::segments(&log_dir)?;
    let closed = segments.split_last().map_or(&[][..], |(_, closed)| closed);
    let manifest = read_manifest(&layout.manifest)?;
    let first_here = manifest.is_none();
    let mut rows = manifest.unwrap_or_default();
    let planned = plan(closed, &rows, &layout.log)?;

    let same_volume = destination::on_log_volume(&log_dir, &folder)?;
    let clones = same_volume && copy::volume_clones(destination::existing_folder(&folder));
    destination::refuse_if_disk_went_away(paths, &folder, same_volume)?;
    let new_bytes = planned.copy.iter().map(|job| job.size).sum();
    let new_files = planned.copy.len() as u64;
    destination::refuse_if_full(&folder, new_bytes, new_files, rows.len() as u64, clones)?;

    layout.create(&folder)?;
    if first_here {
        for note in FIRST_RUN_NOTES {
            println!("{note}");
        }
    }
    copy::remove_leftovers(&layout)?;
    let copies = planned
        .copy
        .iter()
        .map(|job| copy::copy_verified(job, &layout.log, clones))
        .collect::<io::Result<Vec<_>>>()?;
    copy::sync_dir(&layout.log);

    // Nothing new, nothing written: a rerun must not wear a backup disk, or fail
    // on a full or read-only one with nothing left to do.
    if first_here || !copies.is_empty() || !planned.adopted.is_empty() {
        let written = copies.iter().map(|(row, _)| row.clone());
        rows.extend(planned.adopted.into_iter().chain(written).map(keyed));
        write_manifest(&layout.manifest, &rows)?;
        copy::sync_dir(&layout.dir);
    }
    let bytes = rows.values().map(|row| row.size).sum();
    write_record(paths, &folder, &rows, bytes, same_volume)?;

    let names: BTreeSet<String> = segments.iter().map(|path| file_name(path)).collect();
    print_report(&Report {
        layout: &layout,
        copied: copies.len(),
        cloned: copies.iter().filter(|(_, cloned)| *cloned).count(),
        copied_bytes: copies.iter().map(|(row, _)| row.size).sum(),
        held: rows.len(),
        held_bytes: bytes,
        gone: rows.keys().filter(|name| !names.contains(*name)).count(),
        newer: newer_records(&log_dir, &rows)?,
        same_volume,
    });
    Ok(())
}

/// One run per log at a time, or a slow first copy and the scheduler's next run
/// would overwrite each other's manifest. Held on the log directory itself,
/// released by the kernel however the process ends, so no lock file can go stale.
fn hold_lock(log_dir: &Path) -> io::Result<File> {
    let held = File::open(log_dir).map_err(named(log_dir))?;
    match held.try_lock() {
        Err(fs::TryLockError::WouldBlock) => Err(refuse(format!(
            "another conbus backup is running for {}",
            log_dir.display()
        ))),
        // A file system with no locks. Temporary names are per process, and the
        // next run mends a manifest that loses a row, so this can go on alone.
        Ok(()) | Err(fs::TryLockError::Error(_)) => Ok(held),
    }
}

// ---- What to copy ----

struct Plan {
    copy: Vec<Job>,
    adopted: Vec<Row>,
}

enum Step {
    Keep,
    Copy,
    Adopt(Row),
}

/// Decide everything before writing anything, so a log that is not the one in
/// this backup is refused with the backup untouched rather than half mixed in.
/// A file is only hashed when its size cannot settle the question, so a rerun
/// costs a `stat` per segment. A closed segment changed in place at the same size
/// is therefore not noticed here; the copy stays as it was.
fn plan(closed: &[PathBuf], rows: &Rows, log: &Path) -> io::Result<Plan> {
    let held = held_sizes(log)?;
    let mut copy = Vec::new();
    let mut adopted = Vec::new();
    for source in closed {
        let name = file_name(source);
        let size = fs::metadata(source).map_err(named(source))?.len();
        let there = held.get(&name).copied();
        match step(source, &name, size, rows.get(&name), there, log)? {
            Step::Keep => {}
            Step::Copy => copy.push(Job {
                source: source.clone(),
                name,
                size,
            }),
            Step::Adopt(row) => adopted.push(row),
        }
    }
    // Its real name means it was read back once, and the segment it came from is
    // gone or open. There is nothing to compare it with, and it may be all
    // that is left of those records.
    let closed_names: BTreeSet<String> = closed.iter().map(|path| file_name(path)).collect();
    for name in held.keys() {
        if !rows.contains_key(name) && !closed_names.contains(name) {
            adopted.push(digest_file(&log.join(name))?.into_row(name));
        }
    }
    Ok(Plan { copy, adopted })
}

fn held_sizes(log: &Path) -> io::Result<BTreeMap<String, u64>> {
    SegmentLog::segments(log)
        .map_err(named(log))?
        .iter()
        .map(|path| {
            let size = fs::metadata(path).map_err(named(path))?.len();
            Ok((file_name(path), size))
        })
        .collect()
}

fn step(
    source: &Path,
    name: &str,
    size: u64,
    row: Option<&Row>,
    there: Option<u64>,
    log: &Path,
) -> io::Result<Step> {
    let Some(row) = row else {
        let Some(there) = there else {
            return Ok(Step::Copy);
        };
        // A run that died before its manifest, or a manifest deleted by hand.
        let copy = digest_file(&log.join(name))?;
        return if digest_file(source)? == copy {
            Ok(Step::Adopt(copy.into_row(name)))
        } else {
            Err(differs(name, size, there))
        };
    };
    if size == row.size && there == Some(row.size) {
        return Ok(Step::Keep);
    }
    let vouched = if size == row.size {
        // The copy is gone or cut short. Only what the manifest vouches for may
        // take its place.
        digest_file(source)?.sha256 == row.sha256
    } else {
        // Longer, and the copy is exactly how it starts: a restored segment that
        // was the open one and went on growing.
        size > row.size && digest_start(source, row.size)?.sha256 == row.sha256
    };
    if vouched {
        Ok(Step::Copy)
    } else {
        Err(differs(name, size, row.size))
    }
}

fn differs(name: &str, in_log: u64, in_backup: u64) -> io::Error {
    refuse(format!(
        "{name} in the log ({in_log} bytes) differs from the copy in the backup ({in_backup} \
         bytes). Nothing was copied and the backup is untouched. If the log's copy is damaged, \
         restore that file from the backup (manual, section 11); if this folder holds another \
         log's backup, choose another folder"
    ))
}

// ---- The manifest ----

/// `None` when there is no manifest, an error naming it when there is one that
/// cannot be trusted. It is a file on a disk that may have been anywhere, so a
/// row's name has to be a bare segment name before it is joined to a path.
fn read_manifest(path: &Path) -> io::Result<Option<Rows>> {
    let text = match fs::read(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(named(path)(err)),
    };
    let damaged = |why: String| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} cannot be trusted: {why}", path.display()),
        )
    };
    let manifest: Manifest =
        serde_json::from_slice(&text).map_err(|err| damaged(err.to_string()))?;
    if manifest.format != MANIFEST_FORMAT {
        return Err(damaged(format!(
            "format {} is not the {MANIFEST_FORMAT} this build reads",
            manifest.format
        )));
    }
    if let Some(row) = manifest
        .segments
        .iter()
        .find(|row| !SegmentLog::is_segment_name(&row.name))
    {
        return Err(damaged(format!("{:?} is not a segment name", row.name)));
    }
    Ok(Some(manifest.segments.into_iter().map(keyed).collect()))
}

fn write_manifest(path: &Path, rows: &Rows) -> io::Result<()> {
    let manifest = Manifest {
        format: MANIFEST_FORMAT,
        segments: rows.values().cloned().collect(),
    };
    let text = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    write_atomic(path, &text)
}

// ---- The record, and doctor's line ----

fn read_record(paths: &Paths) -> io::Result<Option<Record>> {
    let path = paths.backup_record();
    let text = match fs::read(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(named(&path)(err)),
    };
    serde_json::from_slice(&text)
        .map(Some)
        .map_err(|err| named(&path)(io::Error::other(err)))
}

fn write_record(
    paths: &Paths,
    folder: &Path,
    rows: &Rows,
    bytes: u64,
    same_volume: bool,
) -> io::Result<()> {
    let record = Record {
        destination: folder.to_string_lossy().into_owned(),
        last_ok: Timestamp::now(),
        segments: rows.len() as u64,
        bytes,
        same_volume,
    };
    let text = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
    write_atomic(&paths.backup_record(), &text)
}

/// `doctor`'s one line, from the record alone. It never looks at the destination:
/// that could be an unplugged disk, or a sleeping one this would wake.
pub fn describe(paths: &Paths) -> String {
    match read_record(paths) {
        Ok(Some(record)) => describe_record(&record, Timestamp::now()),
        Ok(None) => ONE_COPY.to_string(),
        Err(err) => format!("backup    unknown: {err}; this log may have one copy"),
    }
}

fn describe_record(record: &Record, now: Timestamp) -> String {
    if record.segments == 0 {
        return format!(
            "{ONE_COPY} ({} holds no segment yet: nothing had closed)",
            record.destination
        );
    }
    let count = match record.segments {
        1 => "1 segment".to_string(),
        n => format!("{} segments", grouped(n)),
    };
    let line = format!(
        "backup    last ok {} ago to {} ({count})",
        ago(now.seconds_since(record.last_ok)),
        record.destination
    );
    if record.same_volume {
        format!("{line} — {SAME_DISK}")
    } else {
        line
    }
}

fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

// ---- What a run says ----

/// Records the backup does not hold: everything in a segment it has no row for,
/// and whatever a restored segment has gained past the copy it does hold. Listed
/// afresh, since the log may have rolled during the run.
fn newer_records(log_dir: &Path, rows: &Rows) -> io::Result<u64> {
    let mut newer = 0;
    for segment in SegmentLog::segments(log_dir)? {
        let held = rows.get(&file_name(&segment)).map_or(0, |row| row.size);
        newer += SegmentLog::count_records(&segment, held).map_err(named(&segment))?;
    }
    Ok(newer)
}

struct Report<'a> {
    layout: &'a Layout,
    copied: usize,
    cloned: usize,
    copied_bytes: u64,
    held: usize,
    held_bytes: u64,
    gone: usize,
    newer: u64,
    same_volume: bool,
}

fn print_report(report: &Report) {
    println!("backup    {}", report.layout.dir.display());
    if report.copied == 0 {
        println!("backup    copied 0 segment(s): nothing new");
    } else {
        println!(
            "backup    copied {} segment(s), {}; each read back and matched",
            report.copied,
            gib(report.copied_bytes)
        );
    }
    if report.cloned > 0 {
        println!(
            "backup    cloned {} of {} on the same APFS volume: almost no space used",
            report.cloned, report.copied
        );
    }
    println!(
        "backup    holds {} segment(s), {}{}",
        report.held,
        gib(report.held_bytes),
        match report.gone {
            0 => String::new(),
            gone => format!("; {gone} no longer in the log"),
        }
    );
    if report.held == 0 {
        println!("backup    nothing has closed yet: this log still has one copy");
    }
    println!(
        "backup    newer than the backup: {} record(s); the open segment is copied once it \
         rolls over",
        report.newer
    );
    if report.same_volume {
        println!("backup    {SAME_DISK}");
    }
}

// ---- Verify ----

/// Recheck everything the manifest vouches for. Reads only, and needs no data
/// root, because checking a disk on another machine is part of having one.
fn verify(to: &Path) -> io::Result<()> {
    let layout = Layout::of(to);
    let Some(rows) = read_manifest(&layout.manifest)? else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no backup at {}: {} does not exist",
                to.display(),
                layout.manifest.display()
            ),
        ));
    };
    let strays = SegmentLog::segments(&layout.log)
        .map_err(named(&layout.log))?
        .into_iter()
        .filter(|path| !rows.contains_key(&file_name(path)))
        .map(|path| {
            format!(
                "{}: not in the manifest; conbus backup checks it and adds it",
                path.display()
            )
        });
    let problems: Vec<String> = rows
        .values()
        .filter_map(|row| row_problem(&layout.log, row))
        .chain(strays)
        .collect();
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("conbus: {problem}");
        }
        return Err(io::Error::other(format!(
            "the backup at {} failed: {} problem(s)",
            layout.dir.display(),
            problems.len()
        )));
    }
    let bytes = rows.values().map(|row| row.size).sum();
    println!(
        "backup    verified {} segment(s), {}, at {}",
        rows.len(),
        gib(bytes),
        layout.dir.display()
    );
    Ok(())
}

fn row_problem(log: &Path, row: &Row) -> Option<String> {
    let path = log.join(&row.name);
    if !path.exists() {
        return Some(format!("{}: missing", path.display()));
    }
    match digest_file(&path) {
        Err(err) => Some(format!("cannot read {err}")),
        Ok(found) => {
            let found = found.into_row(&row.name);
            if found.size != row.size {
                Some(format!(
                    "{}: {} bytes, and the manifest says {}",
                    path.display(),
                    found.size,
                    row.size
                ))
            } else if found.sha256 != row.sha256 {
                Some(format!("{}: differs from the manifest", path.display()))
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(segments: u64, same_volume: bool, age: i64) -> (Record, Timestamp) {
        let now: Timestamp = "2026-11-02T12:00:00.000Z".parse().expect("ts");
        let last_ok = Timestamp::from_unix_seconds(
            now.seconds_since(Timestamp::from_unix_seconds(0).expect("epoch")) - age,
        )
        .expect("ts");
        (
            Record {
                destination: "/Volumes/My Disk".into(),
                last_ok,
                segments,
                bytes: 1,
                same_volume,
            },
            now,
        )
    }

    #[test]
    fn counts_read_the_way_a_person_writes_them() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_460), "1,460");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }

    #[test]
    fn doctors_line_says_how_old_where_and_how_much() {
        let (last, now) = record(1_460, false, 3 * 3_600 + 5);
        assert_eq!(
            describe_record(&last, now),
            "backup    last ok 3h ago to /Volumes/My Disk (1,460 segments)"
        );
        let (last, now) = record(1, false, 90);
        assert_eq!(
            describe_record(&last, now),
            "backup    last ok 1m ago to /Volumes/My Disk (1 segment)"
        );
    }

    #[test]
    fn doctors_line_says_a_same_disk_backup_is_not_a_second_disk() {
        let (last, now) = record(2, true, 60);
        assert!(describe_record(&last, now).ends_with(&format!("(2 segments) — {SAME_DISK}")));
    }

    #[test]
    fn a_backup_holding_nothing_is_still_one_copy() {
        let (last, now) = record(0, true, 60);
        let line = describe_record(&last, now);
        assert!(line.starts_with(ONE_COPY), "{line}");
        assert!(!line.contains("last ok"), "{line}");
    }
}
