//! `<root>/backup.json`: where backups went and the last good run, and `doctor`'s
//! one line, which is read from this and nothing else.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use conbus_core::clock::{Timestamp, ago};
use conbus_core::paths::Paths;
use serde::{Deserialize, Serialize};

use super::copy::write_atomic;
use super::{Layout, NETWORK, SAME_DISK, named};

const ONE_COPY: &str = "backup    none: this log has one copy";

/// The last run that finished.
#[derive(Serialize, Deserialize)]
pub(super) struct LastRun {
    pub destination: String,
    pub last_ok: Timestamp,
    /// Copies that are really there, not rows in the manifest.
    pub segments: u64,
    pub bytes: u64,
    pub same_volume: bool,
    #[serde(default)]
    pub same_disk: bool,
    #[serde(default)]
    pub network: bool,
    /// Manifest rows gone from the backup and from the log too.
    #[serde(default)]
    pub lost: u64,
}

/// A folder a backup has gone to. Kept for good: it may hold plain text, and
/// nothing else says where it is.
#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Destination {
    pub path: String,
    /// A run has finished here, so a backup is expected to be here.
    pub finished: bool,
}

/// `<root>/backup.json`. All `doctor` and `uninstall` ever read.
#[derive(Serialize, Deserialize, Default)]
pub(super) struct Record {
    #[serde(flatten)]
    pub last: Option<LastRun>,
    #[serde(default)]
    pub destinations: Vec<Destination>,
}

impl Record {
    pub fn knows(&self, folder: &str) -> bool {
        self.destinations.iter().any(|known| known.path == folder)
    }

    pub fn with_destination(mut self, folder: &str) -> Self {
        if !self.knows(folder) {
            self.destinations.push(Destination {
                path: folder.to_string(),
                finished: false,
            });
        }
        self
    }

    pub fn finished(mut self, last: LastRun) -> Self {
        for known in &mut self.destinations {
            known.finished = known.finished || known.path == last.destination;
        }
        self.last = Some(last);
        self
    }
}

pub(super) fn read_record(paths: &Paths) -> io::Result<Option<Record>> {
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

pub(super) fn write_record(paths: &Paths, record: &Record) -> io::Result<()> {
    let text = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    write_atomic(&paths.backup_record(), &text)
}

/// Every folder a backup has gone to, for someone removing conbus.
pub fn left_behind(paths: &Paths) -> io::Result<Vec<PathBuf>> {
    Ok(read_record(paths)?
        .unwrap_or_default()
        .destinations
        .iter()
        .map(|known| Layout::of(Path::new(&known.path)).dir)
        .collect())
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
    let Some(last) = &record.last else {
        return match record.destinations.last() {
            Some(started) => format!(
                "{ONE_COPY}; a run that did not finish left files in {}",
                started.path
            ),
            None => ONE_COPY.to_string(),
        };
    };
    if last.segments == 0 {
        return format!(
            "{ONE_COPY} ({} holds no segment yet: nothing had closed)",
            last.destination
        );
    }
    let count = match last.segments {
        1 => "1 segment".to_string(),
        n => format!("{} segments", grouped(n)),
    };
    let lost = match last.lost {
        0 => String::new(),
        lost => format!(", {} lost", grouped(lost)),
    };
    let line = format!(
        "backup    last ok {} ago to {} ({count}{lost})",
        ago(now.seconds_since(last.last_ok)),
        last.destination
    );
    if last.same_volume || last.same_disk {
        format!("{line} — {SAME_DISK}")
    } else if last.network {
        format!("{line} — {NETWORK}")
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
                last: Some(LastRun {
                    destination: "/Volumes/My Disk".into(),
                    last_ok,
                    segments,
                    bytes: 1,
                    same_volume,
                    same_disk: same_volume,
                    network: false,
                    lost: 0,
                }),
                destinations: Vec::new(),
            },
            now,
        )
    }

    #[test]
    fn a_second_volume_on_the_disk_and_a_network_share_each_say_so() {
        let (mut last, now) = record(2, false, 60);
        let run = last.last.as_mut().expect("a run");
        run.same_disk = true;
        assert!(describe_record(&last, now).ends_with(SAME_DISK));

        let run = last.last.as_mut().expect("a run");
        run.same_disk = false;
        run.network = true;
        assert!(describe_record(&last, now).ends_with(NETWORK));
    }

    #[test]
    fn a_record_says_where_it_went_before_any_run_has_finished() {
        let record = Record::default()
            .with_destination("/a")
            .with_destination("/a");
        assert_eq!(record.destinations.len(), 1);
        assert!(!record.destinations[0].finished);
        let text = serde_json::to_value(&record).expect("json");
        assert_eq!(text.get("last_ok"), None);
        let back: Record = serde_json::from_value(text).expect("read back");
        assert!(back.last.is_none());
        assert_eq!(back.destinations[0].path, "/a");
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
