//! The append-only JSONL log. This is the source of truth; the SQLite index
//! that comes later is derived from it and always rebuildable.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::clock::Timestamp;
use crate::event::{Body, Event, ToolOutcome};
use crate::record::{Entry, Record, SCHEMA_VERSION};
use crate::retention::Retention;

const SEGMENT_PREFIX: &str = "segment-";
const SEGMENT_SUFFIX: &str = ".jsonl";

/// How large a segment grows before the log rolls over to the next one.
pub const DEFAULT_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;

fn segment_name(index: u64) -> String {
    format!("{SEGMENT_PREFIX}{index:08}{SEGMENT_SUFFIX}")
}

fn segment_index(path: &Path) -> Option<u64> {
    path.file_name()
        .and_then(|name| name.to_str())?
        .strip_prefix(SEGMENT_PREFIX)?
        .strip_suffix(SEGMENT_SUFFIX)?
        .parse()
        .ok()
}

/// Segment files in append order.
fn segment_paths(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| segment_index(path).is_some())
            .collect(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err),
    };
    // Zero padding makes the byte order the append order.
    paths.sort();
    Ok(paths)
}

/// Make the log directory, owner only: the segments hold prompts and replies.
fn make_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("could not make {} private: {err}", dir.display()),
        )
    })
}

/// Close an unterminated last line left by a crash.
///
/// Without this the next append is glued onto the torn line and both records
/// become unreadable — the torn one was already lost, the new one need not be.
fn heal_torn_tail(path: &Path, file: &mut File) -> io::Result<u64> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(0);
    }
    let mut reader = File::open(path)?;
    reader.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    reader.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(len);
    }
    file.write_all(b"\n")?;
    Ok(len + 1)
}

/// Just enough of a record to resume the sequence.
///
/// Deliberately not `Record`: a line written by a newer build may carry an
/// event variant this one cannot name, and parsing the whole record would make
/// that line's sequence number invisible. The next append would then reuse a
/// number already on disk — duplicate keys in the source of truth, with no
/// defect to say so. A sequence is a sequence whatever the event turns out
/// to be.
#[derive(serde::Deserialize)]
struct SequenceOnly {
    seq: u64,
}

/// Read a segment as byte lines.
///
/// Bytes, not `String` lines. A crash does not aim for a character boundary, so
/// any non-ASCII text in the log — a Korean prompt, an emoji, box drawing in a
/// tool body — can be cut mid-character. `BufRead::lines` turns that into an
/// `io::Error` for the whole file, and every caller here treats an error as
/// fatal: `scan` gives up on the entire log directory and the daemon refuses to
/// start, so recording ends permanently over one damaged line. Decoding is a
/// per-line concern, and a line that fails it is a defect like any other.
fn byte_lines(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut lines = Vec::new();
    loop {
        let mut buf = Vec::new();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        lines.push(buf);
    }
    Ok(lines)
}

/// The highest sequence number already on disk.
///
/// Walks segments newest-first and stops at the first one holding a numbered
/// line, so a restart costs one segment rather than the whole log. A line this
/// build cannot read — whether its event is unfamiliar or its bytes are not
/// valid UTF-8 — is skipped rather than ending the walk, because a sequence
/// number after it is still a sequence number, and missing it means handing out
/// one that is already on disk.
fn last_sequence(segments: &[PathBuf]) -> io::Result<Option<u64>> {
    for segment in segments.iter().rev() {
        let highest = byte_lines(segment)?
            .iter()
            .filter_map(|line| serde_json::from_slice::<SequenceOnly>(line).ok())
            .map(|numbered| numbered.seq)
            .max();
        if highest.is_some() {
            return Ok(highest);
        }
    }
    Ok(None)
}

/// A line that could not be read back, and why.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Defect {
    pub segment: PathBuf,
    /// 1-based line number within the segment.
    pub line: u64,
    pub reason: String,
}

/// Everything a directory of segments holds, plus whatever could not be read.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Scan {
    pub records: Vec<Record>,
    pub defects: Vec<Defect>,
}

/// The segment currently being appended to.
struct Current {
    index: u64,
    file: File,
    bytes: u64,
}

/// An append-only log directory.
pub struct SegmentLog {
    dir: PathBuf,
    retention: Retention,
    max_segment_bytes: u64,
    next_seq: u64,
    /// Opened on first append, so a retention level that writes nothing leaves
    /// nothing behind — not even an empty segment.
    current: Option<Current>,
}

impl SegmentLog {
    /// Open (creating if needed) the log in `dir`, resuming its sequence.
    pub fn open(dir: impl AsRef<Path>, retention: Retention) -> io::Result<Self> {
        Self::open_with_segment_size(dir, retention, DEFAULT_SEGMENT_BYTES)
    }

    /// As [`SegmentLog::open`], with an explicit rollover size.
    pub fn open_with_segment_size(
        dir: impl AsRef<Path>,
        retention: Retention,
        max_segment_bytes: u64,
    ) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        make_private_dir(&dir)?;
        let existing = segment_paths(&dir)?;
        let next_seq = last_sequence(&existing)?.map_or(0, |seq| seq + 1);
        Ok(SegmentLog {
            dir,
            retention,
            max_segment_bytes,
            next_seq,
            current: None,
        })
    }

    /// The segment a writer would append to right now, whether or not it
    /// exists yet. Exposed so a diagnostic can probe the same file the writer
    /// opens rather than guessing from the directory.
    pub fn current_segment(dir: &Path) -> io::Result<PathBuf> {
        let existing = segment_paths(dir)?;
        Ok(existing
            .last()
            .cloned()
            .unwrap_or_else(|| dir.join(segment_name(0))))
    }

    /// The sequence number the next appended record will carry.
    pub fn next_sequence(&self) -> u64 {
        self.next_seq
    }

    /// Append one event. Returns the sequence number it was written under, or
    /// `None` when retention withheld it.
    /// Returns the record as it reached disk — redacted, sequenced and stamped
    /// — so a caller can fold what was actually written rather than what it
    /// meant to write. Retention can change a body on the way through, and
    /// derived state built from the pre-retention value would disagree with the
    /// log it claims to summarise.
    pub fn append(&mut self, entry: Entry) -> io::Result<Option<Record>> {
        let record = Record {
            schema_version: SCHEMA_VERSION,
            seq: self.next_seq,
            // Backfill knows when the thing actually happened; live mirroring
            // does not need to, and the write time is the truth for it.
            ts: entry.observed_at.unwrap_or_else(Timestamp::now),
            bus_id: entry.bus_id,
            vendor_turn_id: entry.vendor_turn_id,
            vendor_agent_id: entry.vendor_agent_id,
            hook_id: entry.hook_id,
            spooled_at: entry.spooled_at,
            source: entry.source,
            event: entry.event,
        };
        // Retention is applied here, inside the only path to disk, so that no
        // caller can write a body the workspace said to withhold.
        let Some(record) = self.retention.redact(&record) else {
            return Ok(None);
        };
        let mut line = serde_json::to_string(&record).map_err(io::Error::other)?;
        line.push('\n');

        let current = self.segment_for(line.len() as u64)?;
        if let Err(err) = current.file.write_all(line.as_bytes()) {
            // A short write — a full disk is the ordinary cause — leaves part
            // of the line on disk with no newline. Appending onto that glues
            // the next record to the wreck and loses both. Drop the handle so
            // the next append reopens the segment, which heals the tail and
            // recomputes the size from the file rather than from what we meant
            // to write.
            self.current = None;
            return Err(err);
        }
        current.bytes += line.len() as u64;
        self.next_seq += 1;
        Ok(Some(record))
    }

    /// The segment that should receive `incoming` bytes, rolling over first if
    /// this record would push a non-empty segment past its size.
    fn segment_for(&mut self, incoming: u64) -> io::Result<&mut Current> {
        // A file deleted while open goes on taking writes that nothing can ever
        // read, and each one reports success.
        if self
            .current
            .as_ref()
            .is_some_and(|current| self.has_left_its_name(current))
        {
            self.current = None;
        }
        if self.current.is_none() {
            if !self.dir.is_dir() {
                make_private_dir(&self.dir)?;
            }
            let existing = segment_paths(&self.dir)?;
            let index = existing.last().and_then(|p| segment_index(p)).unwrap_or(0);
            self.current = Some(self.open_segment(index)?);
        }
        let needs_rollover = self
            .current
            .as_ref()
            .is_some_and(|c| c.bytes > 0 && c.bytes + incoming > self.max_segment_bytes);
        if needs_rollover {
            let next = self.current.as_ref().map_or(0, |c| c.index) + 1;
            self.current = Some(self.open_segment(next)?);
        }
        Ok(self.current.as_mut().expect("segment was just opened"))
    }

    /// Whether the file this writer holds is no longer the one at its name:
    /// deleted, moved aside, or replaced by a restore. A name that cannot be
    /// looked up for any other reason says nothing about the file.
    fn has_left_its_name(&self, current: &Current) -> bool {
        match fs::metadata(self.dir.join(segment_name(current.index))) {
            Ok(named) => current
                .file
                .metadata()
                .is_ok_and(|held| (held.dev(), held.ino()) != (named.dev(), named.ino())),
            Err(err) => err.kind() == io::ErrorKind::NotFound,
        }
    }

    fn open_segment(&self, index: u64) -> io::Result<Current> {
        let path = self.dir.join(segment_name(index));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        let bytes = heal_torn_tail(&path, &mut file)?;
        Ok(Current { index, file, bytes })
    }

    /// Read every segment in `dir` back, in append order.
    pub fn scan(dir: impl AsRef<Path>) -> io::Result<Scan> {
        let mut records = Vec::new();
        let defects = Self::scan_each(dir, |record| records.push(record))?;
        Ok(Scan { records, defects })
    }

    /// Read every segment in `dir` back, in append order, handing each record
    /// to `each` as it is parsed rather than collecting them, and return what
    /// could not be read.
    ///
    /// For a reader that folds the log and keeps only the fold. The daemon's
    /// start-up fold over a collected scan cost as much memory as the log
    /// itself — 564 MB for 298k records on 2026-09-28 — and most of it stayed
    /// with the process for weeks.
    pub fn scan_each(
        dir: impl AsRef<Path>,
        mut each: impl FnMut(Record),
    ) -> io::Result<Vec<Defect>> {
        let mut defects = Vec::new();
        for segment in segment_paths(dir.as_ref())? {
            for (index, line) in byte_lines(&segment)?.into_iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                // `from_slice` validates UTF-8 as part of parsing, so a line
                // whose bytes are damaged and a line whose JSON is damaged take
                // the same path: one defect, and the rest of the log survives.
                match serde_json::from_slice::<Record>(&line) {
                    // A newer writer may have changed what a field means. Reading
                    // it as though we understood it is worse than reporting it.
                    Ok(record) if record.schema_version > SCHEMA_VERSION => defects.push(Defect {
                        segment: segment.clone(),
                        line: index as u64 + 1,
                        reason: format!(
                            "record schema v{} is newer than this build's v{SCHEMA_VERSION}",
                            record.schema_version
                        ),
                    }),
                    Ok(record) => each(record),
                    Err(err) => defects.push(Defect {
                        segment: segment.clone(),
                        line: index as u64 + 1,
                        reason: err.to_string(),
                    }),
                }
            }
        }
        Ok(defects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Body, Event, Harness, Source, TurnState};
    use crate::ids::BusId;

    /// What a fresh read of `dir` finds: each record's sequence number, in order.
    fn seqs_in(dir: &Path) -> Vec<u64> {
        SegmentLog::scan(dir)
            .expect("scan")
            .records
            .iter()
            .map(|record| record.seq)
            .collect()
    }

    #[test]
    fn a_segment_deleted_under_the_writer_is_not_written_into_the_void() {
        // A deleted file goes on taking writes through an open handle. Every
        // record after a stray `rm` reported success and reached nothing.
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("log");
        let mut log = SegmentLog::open(&dir, Retention::default()).expect("open");
        log.append(entry(BusId::mint(), prompt("before")))
            .expect("append");
        fs::remove_file(SegmentLog::current_segment(&dir).expect("segment")).expect("rm");

        log.append(entry(BusId::mint(), prompt("after")))
            .expect("append");

        assert_eq!(seqs_in(&dir), [1], "the record went nowhere");
    }

    #[test]
    fn a_log_folder_deleted_under_the_writer_comes_back_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("log");
        let mut log = SegmentLog::open(&dir, Retention::default()).expect("open");
        log.append(entry(BusId::mint(), prompt("before")))
            .expect("append");
        fs::remove_dir_all(&dir).expect("rm -r");

        log.append(entry(BusId::mint(), prompt("after")))
            .expect("append");

        assert_eq!(seqs_in(&dir), [1]);
        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700, "the log directory");
        let segment = SegmentLog::current_segment(&dir).expect("segment");
        assert_eq!(mode(&segment), 0o600, "a segment");
    }

    #[test]
    fn a_log_folder_deleted_before_the_first_write_comes_back_too() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("log");
        let mut log = SegmentLog::open(&dir, Retention::default()).expect("open");
        fs::remove_dir_all(&dir).expect("rm -r");

        log.append(entry(BusId::mint(), prompt("first")))
            .expect("append");

        assert_eq!(seqs_in(&dir), [0]);
    }

    #[test]
    fn a_log_moved_aside_under_the_writer_keeps_what_it_had() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("log");
        let aside = home.path().join("log.old");
        let mut log = SegmentLog::open(&dir, Retention::default()).expect("open");
        log.append(entry(BusId::mint(), prompt("before")))
            .expect("append");
        fs::rename(&dir, &aside).expect("mv");

        log.append(entry(BusId::mint(), prompt("after")))
            .expect("append");

        assert_eq!(seqs_in(&aside), [0], "what was moved aside took a write");
        assert_eq!(seqs_in(&dir), [1]);
    }

    #[test]
    fn a_segment_put_back_under_the_writer_takes_the_next_record() {
        // A restore while the recorder runs: the same name, a new file, and the
        // old handle writing to neither of them.
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("log");
        let mut log = SegmentLog::open(&dir, Retention::default()).expect("open");
        log.append(entry(BusId::mint(), prompt("before")))
            .expect("append");
        let segment = SegmentLog::current_segment(&dir).expect("segment");
        let copy = fs::read(&segment).expect("read");
        fs::remove_file(&segment).expect("rm");
        fs::write(&segment, &copy).expect("restore");

        log.append(entry(BusId::mint(), prompt("after")))
            .expect("append");

        assert_eq!(seqs_in(&dir), [0, 1]);
    }

    #[test]
    fn a_writer_with_its_file_in_place_is_not_told_it_went_away() {
        // Reopening heals the tail and measures the file, which is not a cost to
        // pay on every record.
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("log");
        let mut log = SegmentLog::open(&dir, Retention::default()).expect("open");
        log.append(entry(BusId::mint(), prompt("one")))
            .expect("append");
        let current = log.current.as_ref().expect("open");
        assert!(!log.has_left_its_name(current));
    }

    #[test]
    fn a_new_log_is_readable_by_its_owner_only() {
        // The log holds prompts and replies. On macOS a home directory is
        // 0755, so a log written with the default umask was readable by every
        // other account on the Mac.
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("log");
        let mut log = SegmentLog::open(&dir, Retention::default()).expect("open");
        log.append(entry(BusId::mint(), prompt("hi")))
            .expect("append");

        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700, "the log directory");
        let segment = SegmentLog::current_segment(&dir).expect("segment");
        assert_eq!(mode(&segment), 0o600, "a segment");
    }

    fn source() -> Source {
        Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: None,
            vendor_session_id: None,
            workspace: None,
            entrypoint: None,
            backfilled_from: None,
            profile: None,
        }
    }

    fn entry(bus_id: BusId, event: Event) -> Entry {
        Entry {
            bus_id,
            observed_at: None,
            source: source(),
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event,
        }
    }

    fn prompt(text: &str) -> Event {
        Event::TurnStarted {
            author: None,
            prompt: Body::kept(text),
        }
    }

    #[test]
    fn a_full_segment_rolls_over_to_the_next_one_without_losing_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bus_id = BusId::mint();
        // A tiny cap so a handful of records spans several segments.
        let mut log = SegmentLog::open_with_segment_size(dir.path(), Retention::default(), 512)
            .expect("open");

        for n in 0..12 {
            log.append(entry(bus_id, prompt(&format!("turn {n}"))))
                .expect("append");
        }

        let segments = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .count();
        assert!(
            segments > 1,
            "expected a rollover, found {segments} segment(s)"
        );

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert_eq!(scan.defects, vec![]);
        assert_eq!(
            scan.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
            (0..12).collect::<Vec<_>>(),
            "rollover must preserve append order across segments"
        );
    }

    #[test]
    fn a_rolled_over_log_still_resumes_its_sequence_after_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bus_id = BusId::mint();
        let mut log = SegmentLog::open_with_segment_size(dir.path(), Retention::default(), 512)
            .expect("open");
        for n in 0..12 {
            log.append(entry(bus_id, prompt(&format!("turn {n}"))))
                .expect("append");
        }
        drop(log);

        let mut log = SegmentLog::open_with_segment_size(dir.path(), Retention::default(), 512)
            .expect("reopen");
        let seq = log
            .append(entry(
                bus_id,
                Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                    tokens: None,
                },
            ))
            .expect("append");

        assert_eq!(seq.map(|record| record.seq), Some(12));
    }

    #[test]
    fn a_line_this_build_cannot_parse_still_advances_the_sequence() {
        // A newer build may write an event variant this one does not know. If
        // resuming skips those lines, the next append reuses a sequence number
        // that is already on disk — silent duplicate keys in the source of
        // truth, and no defect anywhere to say so.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        log.append(entry(BusId::mint(), prompt("from this build")))
            .expect("append");
        let segment = dir.path().join("segment-00000000.jsonl");
        let mut newer = OpenOptions::new()
            .append(true)
            .open(&segment)
            .expect("open segment");
        newer
            .write_all(
                br#"{"v":1,"seq":41,"ts":"2026-08-30T07:41:02.481Z","bus_id":"01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e","harness":"claude_code","event":"message_displayed","delta":"hi"}"#,
            )
            .expect("write");
        newer.write_all(b"\n").expect("newline");
        drop(newer);

        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("reopen");
        let seq = log
            .append(entry(BusId::mint(), prompt("from this build again")))
            .expect("append");

        assert_eq!(
            seq.map(|record| record.seq),
            Some(42),
            "resumed on top of a sequence already on disk"
        );
    }

    #[test]
    fn scanning_record_by_record_reads_what_a_whole_scan_reads() {
        // A daemon folds its state from the log once at start. Collecting the
        // whole log first cost as much memory as the log; handed over one at
        // a time, the records must still be exactly the ones `scan` returns,
        // in order, with damaged lines reported the same way.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = SegmentLog::open_with_segment_size(dir.path(), Retention::default(), 200)
            .expect("open");
        for text in ["one", "two", "three"] {
            log.append(entry(BusId::mint(), prompt(text)))
                .expect("append");
        }
        drop(log);
        let first = dir.path().join("segment-00000000.jsonl");
        let mut body = std::fs::read_to_string(&first).expect("read");
        body.push_str("{not json\n");
        std::fs::write(&first, body).expect("write");

        let whole = SegmentLog::scan(dir.path()).expect("scan");
        let mut each = Vec::new();
        let defects =
            SegmentLog::scan_each(dir.path(), |record| each.push(record)).expect("scan_each");

        assert!(
            segment_paths(dir.path()).expect("segments").len() > 1,
            "the records span segments"
        );
        assert_eq!(whole.records.len(), 3);
        assert_eq!(each, whole.records);
        assert_eq!(defects, whole.defects);
        assert_eq!(defects.len(), 1);
    }

    #[test]
    fn a_record_from_a_newer_build_is_reported_rather_than_read_as_if_understood() {
        // A newer writer may have changed what a field means. Parsing it with
        // this build's assumptions is a silent misread of the source of truth,
        // which is worse than saying we cannot read it.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        log.append(entry(BusId::mint(), prompt("from this build")))
            .expect("append");
        let segment = dir.path().join("segment-00000000.jsonl");
        let future = std::fs::read_to_string(&segment)
            .expect("read")
            // Against the constant, not a literal: a schema bump would
            // otherwise leave this replacing nothing, and a test that mutates
            // nothing passes for the wrong reason.
            .replace(&format!("\"v\":{SCHEMA_VERSION}"), "\"v\":99");
        std::fs::write(&segment, future).expect("write");

        let scan = SegmentLog::scan(dir.path()).expect("scan");

        assert_eq!(scan.records, vec![], "a v99 record must not be read as v1");
        assert_eq!(scan.defects.len(), 1, "and it must be reported");
        assert!(
            scan.defects[0].reason.contains("v99"),
            "{:?}",
            scan.defects[0]
        );
    }

    #[test]
    fn ephemeral_retention_leaves_no_trace_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bus_id = BusId::mint();
        let mut log = SegmentLog::open(dir.path(), Retention::Ephemeral).expect("open");

        let seq = log.append(entry(bus_id, prompt("secret"))).expect("append");

        assert_eq!(seq, None);
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("read dir").count(),
            0,
            "ephemeral retention must not even create a segment"
        );
    }

    #[test]
    fn a_tear_inside_a_character_is_a_defect_not_the_end_of_the_log() {
        // The existing torn-line test cuts at an ASCII boundary. A real crash
        // does not aim: any non-ASCII text — a Korean prompt, an emoji, a box
        // drawing in a tool body — can be cut mid-character. Reading the log as
        // UTF-8 lines turns that into an `io::Error` for the WHOLE directory,
        // which stops `conbus log`, `doctor`, and the daemon itself, so
        // recording ends permanently and nothing says why.
        let dir = tempfile::tempdir().expect("tempdir");
        let bus_id = BusId::mint();
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        log.append(entry(bus_id, prompt("핸드오프 읽고 recorder-first부터")))
            .expect("append");
        let segment = dir.path().join("segment-00000000.jsonl");
        let mut torn = OpenOptions::new()
            .append(true)
            .open(&segment)
            .expect("open segment");
        // The first byte of a three-byte character, and then the power goes.
        torn.write_all(br#"{"v":1,"seq":1,"ts":"2026-08-31T04:12:00.000Z","prompt":""#)
            .expect("tear");
        torn.write_all(&[0xEC]).expect("tear");
        drop(torn);

        let scan = SegmentLog::scan(dir.path()).expect("a torn character must not fail the scan");

        assert_eq!(scan.records.len(), 1, "the intact record must survive");
        assert_eq!(scan.defects.len(), 1, "the torn line must be reported");

        // And the log must still be appendable afterwards. The torn record's
        // own number is free again: a tear at the tail means that record never
        // finished being written, so nothing on disk claims it. (A damaged line
        // in the *middle* is different — the numbers after it are readable and
        // must still be seen; `sequences_after_an_undecodable_line_are_still_seen`
        // covers that.)
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("reopen");
        assert_eq!(
            log.append(entry(bus_id, prompt("after")))
                .expect("append")
                .map(|record| record.seq),
            Some(1),
            "the log stopped being appendable after a torn character"
        );
    }

    #[test]
    fn sequences_after_an_undecodable_line_are_still_seen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let segment = dir.path().join("segment-00000000.jsonl");
        std::fs::create_dir_all(dir.path()).expect("dir");
        let mut file = std::fs::File::create(&segment).expect("create");
        file.write_all(br#"{"v":1,"seq":0,"ts":"2026-08-31T04:12:00.000Z","bus_id":"01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e","harness":"claude_code","event":"session_ended"}"#).expect("w");
        file.write_all(b"\n").expect("w");
        file.write_all(&[0x7b, 0xEC, 0x0a]).expect("w"); // an undecodable line
        file.write_all(br#"{"v":1,"seq":7,"ts":"2026-08-31T04:12:00.000Z","bus_id":"01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e","harness":"claude_code","event":"session_ended"}"#).expect("w");
        file.write_all(b"\n").expect("w");
        drop(file);

        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");

        assert_eq!(
            log.append(entry(BusId::mint(), prompt("next")))
                .expect("append")
                .map(|record| record.seq),
            Some(8),
            "resuming stopped at the line it could not decode and reused a sequence"
        );
    }

    #[test]
    fn a_line_torn_by_a_crash_is_reported_and_the_rest_of_the_log_survives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bus_id = BusId::mint();
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        log.append(entry(bus_id, prompt("intact"))).expect("append");
        drop(log);

        // The process died halfway through writing the next record.
        let segment = dir.path().join("segment-00000000.jsonl");
        let mut torn = OpenOptions::new()
            .append(true)
            .open(&segment)
            .expect("open segment");
        torn.write_all(br#"{"v":1,"seq":1,"ts":"2026-08-30T07:4"#)
            .expect("tear");
        drop(torn);

        let scan = SegmentLog::scan(dir.path()).expect("scan");

        assert_eq!(scan.records.len(), 1, "the complete record must survive");
        assert_eq!(
            scan.defects.len(),
            1,
            "the torn line must be reported, not swallowed"
        );
        assert_eq!(scan.defects[0].segment, segment);
        assert_eq!(scan.defects[0].line, 2);

        // And a restart must not reuse the sequence of the record that survived.
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("reopen");
        let seq = log
            .append(entry(
                bus_id,
                Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                    tokens: None,
                },
            ))
            .expect("append");
        assert_eq!(seq.map(|record| record.seq), Some(1));

        // The record written after the tear must be readable. Without healing
        // the missing newline it would be glued onto the torn line and lost.
        let scan = SegmentLog::scan(dir.path()).expect("rescan");
        assert_eq!(
            scan.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1],
            "the record written after the tear was swallowed by it"
        );
        assert_eq!(scan.defects.len(), 1, "still exactly one torn line");
    }

    #[test]
    fn reopening_resumes_the_sequence_where_the_last_run_stopped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bus_id = BusId::mint();

        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        log.append(entry(bus_id, prompt("before"))).expect("append");
        log.append(entry(
            bus_id,
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: None,
                tokens: None,
            },
        ))
        .expect("append");
        drop(log);

        // The daemon died here.
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("reopen");
        let seq = log.append(entry(bus_id, prompt("after"))).expect("append");

        assert_eq!(
            seq.map(|record| record.seq),
            Some(2),
            "the sequence must not restart at zero"
        );
        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert_eq!(scan.defects, vec![]);
        assert_eq!(
            scan.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn a_record_that_knows_when_it_happened_keeps_that_time() {
        // Backfilled history is not today's history. A log that stamps a
        // conversation from April with today's date is worse than one that
        // never recovered it, because it looks right.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let happened: crate::clock::Timestamp = "2026-04-02T09:15:00.000Z".parse().expect("ts");

        log.append(Entry {
            observed_at: Some(happened),
            ..entry(BusId::mint(), prompt("from april"))
        })
        .expect("append");

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert_eq!(scan.records[0].ts, happened);
    }

    #[test]
    fn a_record_that_does_not_know_is_stamped_with_the_write_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let before = crate::clock::Timestamp::now();
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");

        log.append(entry(BusId::mint(), prompt("now")))
            .expect("append");

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert!(scan.records[0].ts >= before);
    }

    #[test]
    fn appended_events_read_back_in_order_with_ascending_sequence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bus_id = BusId::mint();
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("open");

        log.append(entry(bus_id, prompt("first"))).expect("append");
        log.append(entry(bus_id, prompt("second"))).expect("append");

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert_eq!(scan.defects, vec![]);
        assert_eq!(scan.records.len(), 2);
        assert_eq!(scan.records[0].seq, 0);
        assert_eq!(scan.records[1].seq, 1);
        assert_eq!(
            scan.records[0].event,
            Event::TurnStarted {
                author: None,
                prompt: Body::kept("first")
            }
        );
    }
}

// ---- The derived sidecar cache ----
//
// Reads that never print a tool body do not need one: every field the fold,
// the report and the daemon look at sits on the envelope, and the one place a
// body matters is equality — the twin check compares whole events. `meta`
// replaces only the bodies a tool call carries with an equality-preserving
// digest, and the sidecars under `cache/meta/` hold the transformed lines for
// every closed segment. They are derived data the way the SQLite index is:
// delete them and nothing is lost.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// `D(text)` — a digest that cannot be mistaken for the text it stands for:
/// the NUL byte is not valid in a prompt or a shell transcript's shape, and
/// two bodies are equal after `meta` iff they were equal before, so the twin
/// check and every hash that covers a body keep their answers.
fn digest(text: &str) -> String {
    let mut hexed = String::with_capacity(71);
    let _ = write!(hexed, "\0sha256:");
    for byte in Sha256::digest(text.as_bytes()) {
        let _ = write!(hexed, "{byte:02x}");
    }
    hexed
}

/// One tool body as the cache keeps it. Variant, nonce and the withheld size
/// survive, so equality is preserved in every direction.
fn meta_body(body: &Body) -> Body {
    match body {
        Body::Kept { text } => Body::Kept { text: digest(text) },
        Body::Clipped {
            text,
            original_bytes,
        } => Body::Clipped {
            text: digest(text),
            original_bytes: *original_bytes,
        },
        Body::Sealed {
            alg,
            nonce,
            ciphertext,
            original_bytes,
        } => Body::Sealed {
            alg: alg.clone(),
            nonce: nonce.clone(),
            ciphertext: digest(ciphertext),
            original_bytes: *original_bytes,
        },
        other => other.clone(),
    }
}

/// The record the cache holds: the same record with only the tool bodies
/// digested. Conversation text — prompt and assistant message — stays
/// verbatim, because restore's last_prompt and gap fill's missing_from read it
/// as text.
pub fn meta(record: &Record) -> Record {
    let mut record = record.clone();
    match &mut record.event {
        Event::ToolRequested { input, .. } | Event::ToolDenied { input, .. } => {
            *input = meta_body(input);
        }
        Event::PermissionAsked { input, .. } => {
            *input = meta_body(input);
        }
        Event::ToolUse { input, outcome, .. } => {
            *input = meta_body(input);
            match outcome {
                ToolOutcome::Succeeded { output } => *output = meta_body(output),
                ToolOutcome::Failed { error, .. } => *error = meta_body(error),
            }
        }
        _ => {}
    }
    record
}

/// What one segment parses to, in the same rules `scan_each` applies: a newer
/// schema is a defect, a broken line is a defect, and neither ends the read.
fn parse_segment(path: &Path) -> io::Result<(Vec<Record>, Vec<Defect>)> {
    let mut records = Vec::new();
    let mut defects = Vec::new();
    for (index, line) in byte_lines(path)?.into_iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Record>(&line) {
            Ok(record) if record.schema_version > SCHEMA_VERSION => defects.push(Defect {
                segment: path.to_path_buf(),
                line: index as u64 + 1,
                reason: format!(
                    "record schema v{} is newer than this build's v{SCHEMA_VERSION}",
                    record.schema_version
                ),
            }),
            Ok(record) => records.push(record),
            Err(err) => defects.push(Defect {
                segment: path.to_path_buf(),
                line: index as u64 + 1,
                reason: err.to_string(),
            }),
        }
    }
    Ok((records, defects))
}

/// A sidecar's header line. It is only ever a cache entry: a stale one is
/// ignored and rebuilt, never trusted.
#[derive(Serialize, Deserialize)]
struct SidecarHeader {
    sidecar: u32,
    conbus: String,
    schema: u32,
    segment: String,
    bytes: u64,
    records: u64,
    first_ts: Option<String>,
    last_ts: Option<String>,
    defects: Vec<SidecarDefect>,
}

#[derive(Serialize, Deserialize)]
struct SidecarDefect {
    line: u64,
    reason: String,
}

fn sidecar_path(cache: &Path, segment: &Path) -> PathBuf {
    cache.join("meta").join(
        segment
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("segment")),
    )
}

/// A sidecar only answers for the exact file it was built from: this build's
/// version and schema, the same name, and a length that has not changed. The
/// newest segment never has one — it can still grow.
fn read_sidecar(cache: &Path, segment: &Path) -> Option<(Vec<Record>, Vec<Defect>)> {
    let text = std::fs::read_to_string(sidecar_path(cache, segment)).ok()?;
    let mut lines = text.lines();
    let header: SidecarHeader = serde_json::from_str(lines.next()?).ok()?;
    let bytes = std::fs::metadata(segment).ok()?.len();
    let name = segment.file_name()?.to_str()?;
    if header.sidecar != 1
        || header.conbus != env!("CARGO_PKG_VERSION")
        || header.schema != SCHEMA_VERSION
        || header.segment != name
        || header.bytes != bytes
    {
        return None;
    }
    let mut records = Vec::with_capacity(header.records as usize);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        // A sidecar line that does not read is the cache lying: rebuild.
        records.push(serde_json::from_str::<Record>(line).ok()?);
    }
    if records.len() as u64 != header.records {
        return None;
    }
    let defects = header
        .defects
        .into_iter()
        .map(|defect| Defect {
            segment: segment.to_path_buf(),
            line: defect.line,
            reason: defect.reason,
        })
        .collect();
    Some((records, defects))
}

/// Write the sidecar for a parsed segment, best effort — every failure here
/// is invisible, because the next reader rebuilds the same bytes. Two readers
/// can build the same sidecar at once: each writes a unique temp name beside
/// the target and renames, and a rename is atomic, so the last writer wins
/// with a whole file and no lock is needed.
fn write_sidecar(cache: &Path, segment: &Path, records: &[Record], defects: &[Defect]) {
    let run = || -> io::Result<()> {
        let dir = cache.join("meta");
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700));
        }
        let bytes = std::fs::metadata(segment)?.len();
        let name = segment
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("segment")
            .to_string();
        let header = SidecarHeader {
            sidecar: 1,
            conbus: env!("CARGO_PKG_VERSION").to_string(),
            schema: SCHEMA_VERSION,
            segment: name.clone(),
            bytes,
            records: records.len() as u64,
            first_ts: records.first().map(|r| r.ts.to_string()),
            last_ts: records.last().map(|r| r.ts.to_string()),
            defects: defects
                .iter()
                .map(|d| SidecarDefect {
                    line: d.line,
                    reason: d.reason.clone(),
                })
                .collect(),
        };
        let mut text = serde_json::to_string(&header).map_err(io::Error::other)?;
        text.push('\n');
        for record in records {
            text.push_str(&serde_json::to_string(record).map_err(io::Error::other)?);
            text.push('\n');
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            let _ = builder.create(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            let temp = dir.join(format!(
                ".{name}.{}-{}.tmp",
                std::process::id(),
                Timestamp::now()
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            use std::io::Write as _;
            options
                .open(&temp)
                .and_then(|mut file| file.write_all(text.as_bytes()))?;
            std::fs::rename(&temp, sidecar_path(cache, segment))
        }
        #[cfg(not(unix))]
        {
            let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
            std::fs::write(&temp, &text)?;
            std::fs::rename(&temp, sidecar_path(cache, segment))
        }
    };
    let _ = run();
}

/// Load one segment for a meta scan: a valid sidecar answers, anything else
/// is parsed and — for a closed segment — its sidecar refreshed best effort.
fn load_meta_segment(
    segment: &Path,
    closed: bool,
    cache: Option<&Path>,
) -> io::Result<(Vec<Record>, Vec<Defect>)> {
    if closed
        && let Some(cache) = cache
        && let Some(loaded) = read_sidecar(cache, segment)
    {
        return Ok(loaded);
    }
    let (records, defects) = parse_segment(segment)?;
    let records: Vec<Record> = records.iter().map(meta).collect();
    if closed && let Some(cache) = cache {
        write_sidecar(cache, segment, &records, &defects);
    }
    Ok((records, defects))
}

/// What `build_cache` reports.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CacheReport {
    /// Segments eligible for a sidecar — every one but the newest.
    pub closed: u64,
    /// Sidecars written or rewritten this run.
    pub built: u64,
    /// Sidecars that were already valid.
    pub valid: u64,
}

impl SegmentLog {
    /// The records a reader that never prints tool bodies sees — closed
    /// segments served from their sidecars, the newest parsed live, every
    /// record through `meta`. `cache = None`, or `CONBUS_NO_CACHE=1`, parses
    /// every segment and touches no cache. Output is record-for-record what
    /// `scan_each` plus `meta` gives, defects included.
    pub fn scan_meta_each(
        dir: impl AsRef<Path>,
        cache: Option<&Path>,
        mut each: impl FnMut(Record),
    ) -> io::Result<Vec<Defect>> {
        let segments = segment_paths(dir.as_ref())?;
        let no_cache = cache.is_none() || std::env::var_os("CONBUS_NO_CACHE").is_some();
        let cache = cache.map(Path::to_path_buf);
        let newest = segments.last().cloned();

        // Parse in parallel — order is what matters, so each chunk returns
        // its segments' results and they are emitted strictly in name order.
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8);
        // In waves of `workers` segments, each wave handed over in order before
        // the next is read: parallel, and never more than a wave in memory. The
        // daemon folds its state through this at start, and holding the whole
        // log's records at once is what `scan_each` was written to stop.
        let mut defects = Vec::new();
        for wave in segments.chunks(workers) {
            let loaded: Vec<io::Result<(Vec<Record>, Vec<Defect>)>> = std::thread::scope(|scope| {
                let handles: Vec<_> = wave
                    .iter()
                    .map(|segment| {
                        let closed = newest.as_ref() != Some(segment) && !no_cache;
                        let cache = cache.as_deref();
                        scope.spawn(move || load_meta_segment(segment, closed, cache))
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle
                            .join()
                            .unwrap_or_else(|_| Err(io::Error::other("a cache worker panicked")))
                    })
                    .collect()
            });
            for result in loaded {
                let (records, segment_defects) = result?;
                for record in records {
                    each(record);
                }
                defects.extend(segment_defects);
            }
        }
        Ok(defects)
    }

    /// `scan` for the readers that never print tool bodies.
    pub fn scan_meta(dir: impl AsRef<Path>, cache: Option<&Path>) -> io::Result<Scan> {
        let mut records = Vec::new();
        let defects = Self::scan_meta_each(dir, cache, |record| records.push(record))?;
        Ok(Scan { records, defects })
    }

    /// The explicit build: refresh every closed segment's sidecar and say how
    /// many were written, how many were already valid. Writes only under
    /// `cache`.
    pub fn build_cache(dir: impl AsRef<Path>, cache: impl AsRef<Path>) -> io::Result<CacheReport> {
        let segments = segment_paths(dir.as_ref())?;
        let cache = cache.as_ref();
        let mut report = CacheReport::default();
        for segment in &segments[..segments.len().saturating_sub(1)] {
            report.closed += 1;
            if read_sidecar(cache, segment).is_some() {
                report.valid += 1;
                continue;
            }
            let (records, defects) = parse_segment(segment)?;
            let records: Vec<Record> = records.iter().map(meta).collect();
            write_sidecar(cache, segment, &records, &defects);
            report.built += 1;
        }
        Ok(report)
    }

    /// The last `n` records `keep` accepts, in append order, reading full
    /// records newest-first and never opening a segment it does not need.
    /// Defects come back in segment order, as `scan` reports them.
    pub fn tail(
        dir: impl AsRef<Path>,
        n: usize,
        keep: impl Fn(&Record) -> bool,
    ) -> io::Result<(Vec<Record>, Vec<Defect>)> {
        let segments = segment_paths(dir.as_ref())?;
        let mut kept: Vec<Record> = Vec::new();
        let mut defects = Vec::new();
        for segment in segments.iter().rev() {
            let (records, segment_defects) = parse_segment(segment)?;
            for record in records.iter().rev() {
                if kept.len() >= n {
                    break;
                }
                if keep(record) {
                    kept.push(record.clone());
                }
            }
            defects.extend(segment_defects);
            if kept.len() >= n {
                break;
            }
        }
        kept.reverse();
        defects.sort_by(|a, b| a.segment.cmp(&b.segment).then(a.line.cmp(&b.line)));
        Ok((kept, defects))
    }
}
