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
//! behind, and the next run checks it against the log and keeps it. One with no
//! segment left to compare with stays too, but nothing vouches for it.
//!
//! A copy is never overwritten with different bytes. The one exception is a copy
//! that is an exact prefix of the log's segment: after a restore the newest
//! restored segment is open again and grows, and taking the longer file loses
//! nothing. The open segment itself is compared the same way, so a new log under
//! an old name is refused at once, not when it closes.
//!
//! `<root>/backup.json` lists every folder a backup has gone to, on record before
//! the first copy lands, because a person removing conbus has nothing else to tell
//! them where plain text was left.

mod copy;
mod destination;
mod record;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use conbus_core::clock::Timestamp;
use conbus_core::paths::Paths;
use conbus_core::segment::SegmentLog;
use serde::{Deserialize, Serialize};

use crate::report::gib;
use copy::{Job, digest_file, digest_start, write_atomic};
use record::{LastRun, Record, read_record, write_record};
pub use record::{describe, left_behind};

const BACKUP_DIR: &str = "conbus-backup";
const MANIFEST: &str = "manifest.json";
const MANIFEST_FORMAT: u32 = 1;
const RECORD: &str = "backup.json";

const SAME_DISK: &str = "same disk: protects against deletion, not a failed disk";

const NETWORK: &str = "network share: this copy leaves this machine, in plain text";

const FIRST_RUN_NOTES: &[&str] = &[
    "note      prompts and replies are plain text in this backup, and so are tool inputs and \
     outputs if the log was written under CONBUS_RETENTION=full: an unencrypted external disk \
     exposes them to whoever holds it",
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
    // The only list of where plain text went, so a run must not overwrite one it
    // cannot read.
    let record = read_record(paths)?.unwrap_or_default();
    let layout = Layout::of(&folder);
    destination::refuse_if_disk_went_away(paths, &record, &folder, &layout)?;
    let segments = SegmentLog::segments(&log_dir)?;
    let manifest = read_manifest(&layout.manifest)?;
    let first_here = manifest.is_none();
    let rows = manifest.unwrap_or_default();
    let planned = plan(&segments, &rows, &layout.log)?;

    let standing = destination::standing(&log_dir, &folder)?;
    let new_bytes = planned.copy.iter().map(|job| job.size).sum();
    let new_files = planned.copy.len() as u64;
    let room_for = rows.len() as u64;
    destination::refuse_if_full(&folder, new_bytes, new_files, room_for, standing.clones)?;

    // On record before the first copy lands: however this run ends, something
    // says where the plain text went.
    let name = folder.to_string_lossy().into_owned();
    let record = if record.knows(&name) {
        record
    } else {
        let record = record.with_destination(&name);
        write_record(paths, &record)?;
        record
    };
    layout.create(&folder)?;
    if first_here {
        for note in FIRST_RUN_NOTES {
            println!("{note}");
        }
    }
    copy::remove_leftovers(&layout, paths.root())?;
    let copies = planned
        .copy
        .iter()
        .map(|job| copy::copy_verified(job, &layout.log, standing.clones))
        .collect::<io::Result<Vec<_>>>()?;
    copy::sync_dir(&layout.log);
    let finished = Finished {
        folder: &name,
        layout: &layout,
        segments: &segments,
        rows,
        adopted: planned.adopted,
        unvouched: planned.unvouched,
        copies,
        first_here,
        standing,
    };
    finish(paths, &log_dir, record, finished)
}

/// What a run has done by the time every copy is in place.
struct Finished<'a> {
    folder: &'a str,
    layout: &'a Layout,
    segments: &'a [PathBuf],
    rows: Rows,
    adopted: Vec<Row>,
    unvouched: Vec<String>,
    copies: Vec<(Row, bool)>,
    first_here: bool,
    standing: destination::Standing,
}

/// The manifest, then the count, then the record: the last good time moves only
/// once all of it has worked.
fn finish(paths: &Paths, log_dir: &Path, record: Record, run: Finished) -> io::Result<()> {
    let Finished {
        folder,
        layout,
        segments,
        mut rows,
        adopted,
        unvouched,
        copies,
        first_here,
        standing,
    } = run;
    // Nothing new, nothing written: a rerun must not wear a backup disk, or fail
    // on a full or read-only one with nothing left to do.
    if first_here || !copies.is_empty() || !adopted.is_empty() {
        let written = copies.iter().map(|(row, _)| row.clone());
        rows.extend(adopted.into_iter().chain(written).map(keyed));
        write_manifest(&layout.manifest, &rows)?;
        copy::sync_dir(&layout.dir);
    }
    let newer = newer_records(log_dir, &rows)?;

    let there = held_sizes(&layout.log)?;
    let in_log: BTreeSet<String> = segments.iter().map(|path| file_name(path)).collect();
    let held: Vec<&Row> = rows
        .values()
        .filter(|row| there.contains_key(&row.name))
        .collect();
    let bytes = held.iter().map(|row| row.size).sum();
    let lost: Vec<String> = rows
        .keys()
        .filter(|name| !there.contains_key(*name) && !in_log.contains(*name))
        .cloned()
        .collect();
    let record = record.finished(LastRun {
        destination: folder.to_string(),
        last_ok: Timestamp::now(),
        segments: held.len() as u64,
        bytes,
        same_volume: standing.same_volume,
        same_disk: standing.same_disk,
        network: standing.network,
        lost: lost.len() as u64,
    });
    write_record(paths, &record)?;

    print_report(&Report {
        layout,
        copied: copies.len(),
        cloned: copies.iter().filter(|(_, cloned)| *cloned).count(),
        copied_bytes: copies.iter().map(|(row, _)| row.size).sum(),
        held: held.len(),
        held_bytes: bytes,
        gone: held
            .iter()
            .filter(|row| !in_log.contains(&row.name))
            .count(),
        lost: &lost,
        unvouched: &unvouched,
        newer,
        same_disk: standing.same_disk,
        network: standing.network,
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
    /// Copies with no row and no closed segment to compare them with.
    unvouched: Vec<String>,
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
fn plan(segments: &[PathBuf], rows: &Rows, log: &Path) -> io::Result<Plan> {
    let closed = segments.split_last().map_or(&[][..], |(_, closed)| closed);
    if let Some(open) = segments.last() {
        refuse_if_not_its_copy(open, rows)?;
    }
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
    // It may be all that is left of those records, so it stays. But with nothing
    // to compare it with, nothing can say it is whole or even from this log.
    let closed_names: BTreeSet<String> = closed.iter().map(|path| file_name(path)).collect();
    let unvouched = held
        .keys()
        .filter(|name| !rows.contains_key(*name) && !closed_names.contains(*name))
        .cloned()
        .collect();
    Ok(Plan {
        copy,
        adopted,
        unvouched,
    })
}

/// The open segment is never copied, but with a row in the manifest it has to be
/// that copy and perhaps more: a restored segment growing. Anything else is a new
/// log under an old name, and counting it from the old row's length would call its
/// records already safe. At most one segment to hash, and only when it has a row.
fn refuse_if_not_its_copy(open: &Path, rows: &Rows) -> io::Result<()> {
    let name = file_name(open);
    let Some(row) = rows.get(&name) else {
        return Ok(());
    };
    let size = fs::metadata(open).map_err(named(open))?.len();
    if size < row.size || digest_start(open, row.size)?.sha256 != row.sha256 {
        return Err(differs(&name, size, row.size));
    }
    Ok(())
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
         delete that file from the log and restore it from the backup (manual, section 11). If \
         a recorder started a new log here, stop it and move that log aside before restoring. \
         If this folder holds another log's backup, choose another folder"
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

// ---- What a run says ----

/// Records the backup does not hold: everything in a segment it has no row for,
/// and whatever a restored segment has gained past the copy it does hold. Listed
/// afresh, since the log may have rolled during the run.
fn newer_records(log_dir: &Path, rows: &Rows) -> io::Result<u64> {
    let mut newer = 0;
    for segment in SegmentLog::segments(log_dir)? {
        let held = rows.get(&file_name(&segment)).map_or(0, |row| row.size);
        // No longer than its copy: nothing to read, and no reason to open it.
        if held > 0 && fs::metadata(&segment).map_err(named(&segment))?.len() <= held {
            continue;
        }
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
    lost: &'a [String],
    unvouched: &'a [String],
    newer: u64,
    same_disk: bool,
    network: bool,
}

/// A few names, and how many more there are.
fn some_of(names: &[String]) -> String {
    const SHOWN: usize = 5;
    let shown = names[..names.len().min(SHOWN)].join(", ");
    match names.len().saturating_sub(SHOWN) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
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
    if !report.lost.is_empty() {
        println!(
            "backup    LOST {} segment(s), gone from the log and from this backup: {}; --verify \
             keeps naming them",
            report.lost.len(),
            some_of(report.lost)
        );
    }
    if !report.unvouched.is_empty() {
        println!(
            "backup    not vouched for: {}. No manifest row and no segment in the log to compare \
             with, so --verify keeps naming them. They may be all that is left of those records, \
             or not from this log",
            some_of(report.unvouched)
        );
    }
    if report.held == 0 {
        println!("backup    nothing has closed yet: this log still has one copy");
    }
    println!(
        "backup    newer than the backup: {} record(s); the open segment is copied once it \
         rolls over",
        report.newer
    );
    if report.same_disk {
        println!("backup    {SAME_DISK}");
    }
    if report.network {
        println!("backup    {NETWORK}");
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
                "{}: not in the manifest, so nothing vouches for it. If the log still has that \
                 segment, conbus backup compares them and adds it. If not, it may be all that \
                 is left of those records, or not from this log",
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

    #[test]
    fn a_few_names_and_how_many_more() {
        let names: Vec<String> = (0..7).map(|n| format!("s{n}")).collect();
        assert_eq!(some_of(&names[..2]), "s0, s1");
        assert_eq!(some_of(&names), "s0, s1, s2, s3, s4 and 2 more");
    }
}
