//! The append-only JSONL log. This is the source of truth; the SQLite index
//! that comes later is derived from it and always rebuildable.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::clock::Timestamp;
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

/// The highest sequence number already on disk.
///
/// Walks segments newest-first and stops at the first one holding a numbered
/// line, so a restart costs one segment rather than the whole log.
fn last_sequence(segments: &[PathBuf]) -> io::Result<Option<u64>> {
    for segment in segments.iter().rev() {
        let reader = BufReader::new(File::open(segment)?);
        let highest = reader
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<SequenceOnly>(&line).ok())
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
        fs::create_dir_all(&dir)?;
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

    /// The sequence number the next appended record will carry.
    pub fn next_sequence(&self) -> u64 {
        self.next_seq
    }

    /// Append one event. Returns the sequence number it was written under, or
    /// `None` when retention withheld it.
    pub fn append(&mut self, entry: Entry) -> io::Result<Option<u64>> {
        let record = Record {
            schema_version: SCHEMA_VERSION,
            seq: self.next_seq,
            ts: Timestamp::now(),
            bus_id: entry.bus_id,
            vendor_turn_id: entry.vendor_turn_id,
            vendor_agent_id: entry.vendor_agent_id,
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
        current.file.write_all(line.as_bytes())?;
        current.bytes += line.len() as u64;
        self.next_seq += 1;
        Ok(Some(record.seq))
    }

    /// The segment that should receive `incoming` bytes, rolling over first if
    /// this record would push a non-empty segment past its size.
    fn segment_for(&mut self, incoming: u64) -> io::Result<&mut Current> {
        if self.current.is_none() {
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

    fn open_segment(&self, index: u64) -> io::Result<Current> {
        let path = self.dir.join(segment_name(index));
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = heal_torn_tail(&path, &mut file)?;
        Ok(Current { index, file, bytes })
    }

    /// Read every segment in `dir` back, in append order.
    pub fn scan(dir: impl AsRef<Path>) -> io::Result<Scan> {
        let mut scan = Scan::default();
        for segment in segment_paths(dir.as_ref())? {
            let reader = BufReader::new(File::open(&segment)?);
            for (index, line) in reader.lines().enumerate() {
                let line = line?;
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Record>(&line) {
                    // A newer writer may have changed what a field means. Reading
                    // it as though we understood it is worse than reporting it.
                    Ok(record) if record.schema_version > SCHEMA_VERSION => {
                        scan.defects.push(Defect {
                            segment: segment.clone(),
                            line: index as u64 + 1,
                            reason: format!(
                                "record schema v{} is newer than this build's v{SCHEMA_VERSION}",
                                record.schema_version
                            ),
                        })
                    }
                    Ok(record) => scan.records.push(record),
                    Err(err) => scan.defects.push(Defect {
                        segment: segment.clone(),
                        line: index as u64 + 1,
                        reason: err.to_string(),
                    }),
                }
            }
        }
        Ok(scan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Body, Event, Harness, Source, TurnState};
    use crate::ids::BusId;

    fn source() -> Source {
        Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: None,
            vendor_session_id: None,
        }
    }

    fn entry(bus_id: BusId, event: Event) -> Entry {
        Entry {
            bus_id,
            source: source(),
            vendor_turn_id: None,
            vendor_agent_id: None,
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
                },
            ))
            .expect("append");

        assert_eq!(seq, Some(12));
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
            seq,
            Some(42),
            "resumed on top of a sequence already on disk"
        );
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
            .replace("\"v\":1", "\"v\":99");
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
                },
            ))
            .expect("append");
        assert_eq!(seq, Some(1));

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
            },
        ))
        .expect("append");
        drop(log);

        // The daemon died here.
        let mut log = SegmentLog::open(dir.path(), Retention::default()).expect("reopen");
        let seq = log.append(entry(bus_id, prompt("after"))).expect("append");

        assert_eq!(seq, Some(2), "the sequence must not restart at zero");
        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert_eq!(scan.defects, vec![]);
        assert_eq!(
            scan.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
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
