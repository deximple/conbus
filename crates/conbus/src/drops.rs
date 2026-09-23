//! What the recorder failed to record.
//!
//! A dropped record has to be reported — a log that quietly misses turns is
//! worse than one that says it missed them. But the obvious channel is a bad
//! one: hook stderr is swallowed by the vendor in normal use, and when it is
//! *not* swallowed it becomes a transcript attachment, so a persistent fault
//! like "the daemon is not running" writes one per hook per turn and buries the
//! conversation it was supposed to protect.
//!
//! So drops go somewhere durable that `conbus doctor` reads, and stderr speaks
//! sparingly. Three things about how, each of which was got wrong first:
//!
//! - **One write per line.** Every hook is a separate process appending to the
//!   same file, and the vendor's own contract says tool hooks "may run
//!   concurrently for parallel tool calls", so concurrent appends are the
//!   normal case. `writeln!` on an unbuffered `File` issues a syscall per
//!   formatting fragment, which interleaves at JSON-token granularity and
//!   shreds the file. One `write_all` of one complete line is atomic on an
//!   `O_APPEND` regular file.
//! - **Never rewritten.** Trimming means a read-modify-write racing those same
//!   appenders, and it makes the count a lie. The file only grows, and stops
//!   growing at a cap.
//! - **The throttle keys on when we last *spoke*, not on when we last dropped.**
//!   Keying on the drop file meant every drop pushed the window forward, so a
//!   standing fault spoke once and then never again. It is also keyed on the
//!   reason, so a standing benign fault cannot silence a new one — "no mapping
//!   for hook event X" means the vendor's schema moved and must never wait
//!   behind "the daemon is down".

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{Duration, SystemTime};

use conbus_core::clock::Timestamp;
use conbus_core::paths::Paths;

/// How long the same reason stays quiet after it has been said once.
const STDERR_EVERY: Duration = Duration::from_secs(60);

/// The drop log stops growing here. A drop line is around 150 bytes, so this is
/// tens of thousands of them: far past the point where the count is the story
/// and the individual lines are not.
const CAP_BYTES: u64 = 4 * 1024 * 1024;

fn drops_file(paths: &Paths) -> std::path::PathBuf {
    paths.root().join("drops.jsonl")
}

fn spoke_file(paths: &Paths) -> std::path::PathBuf {
    paths.root().join("drops.spoke")
}

/// A record that did not land: why, and — when the hook knew — what it was.
///
/// `reason` is the stderr line and the throttle key. `detail` names the event
/// and the session for the durable record only, because thirty-one orphans on
/// a real log all said "no binding" and nothing else, and a ghost session's
/// `SessionEnd` could not be told from a lost tool call. Identifiers, never
/// content: the log already carries the event name and the whole session id.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Dropped {
    pub reason: String,
    pub detail: Option<String>,
}

impl Dropped {
    pub fn new(reason: impl Into<String>) -> Self {
        Dropped {
            reason: reason.into(),
            detail: None,
        }
    }

    pub fn with_detail(self, detail: impl Into<String>) -> Self {
        Dropped {
            detail: Some(detail.into()),
            ..self
        }
    }
}

impl From<String> for Dropped {
    fn from(reason: String) -> Self {
        Dropped::new(reason)
    }
}

impl std::fmt::Display for Dropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Some(detail) => write!(f, "{} ({detail})", self.reason),
            None => f.write_str(&self.reason),
        }
    }
}

/// Record a dropped record. Returns whether the caller should also say so on
/// stderr.
///
/// Never fails and never panics: it runs on the path that exists in order not
/// to fail.
pub fn record(dropped: &Dropped) -> bool {
    let Ok(paths) = Paths::resolve() else {
        return true;
    };
    let _ = paths.keep_private();
    record_at(&paths, dropped)
}

fn record_at(paths: &Paths, dropped: &Dropped) -> bool {
    append(paths, dropped);
    should_speak(paths, &dropped.reason)
}

fn append(paths: &Paths, dropped: &Dropped) {
    let file = drops_file(paths);
    if fs::metadata(&file).map(|m| m.len()).unwrap_or(0) >= CAP_BYTES {
        return;
    }
    let Ok(mut handle) = OpenOptions::new().create(true).append(true).open(&file) else {
        return;
    };
    // Built whole, written once. See the module note.
    let mut entry = serde_json::json!({
        "ts": Timestamp::now().to_string(),
        "reason": dropped.reason,
    });
    if let Some(detail) = &dropped.detail {
        entry["detail"] = serde_json::Value::String(detail.clone());
    }
    let mut line = entry.to_string();
    line.push('\n');
    let _ = handle.write_all(line.as_bytes());
}

/// Whether stderr should carry this reason now.
///
/// A different reason from the last one spoken always speaks; the same reason
/// waits out the quiet period. The marker holds the reason so this survives
/// across processes, which is the only place it can live: every hook is its own.
fn should_speak(paths: &Paths, reason: &str) -> bool {
    let marker = spoke_file(paths);
    let speak = match fs::read_to_string(&marker) {
        Ok(last) => {
            let fresh = fs::metadata(&marker)
                .and_then(|m| m.modified())
                .and_then(|at| {
                    SystemTime::now()
                        .duration_since(at)
                        .map_err(std::io::Error::other)
                })
                .map(|since| since < STDERR_EVERY)
                .unwrap_or(false);
            last.trim() != reason || !fresh
        }
        Err(_) => true,
    };
    if speak {
        let _ = fs::write(&marker, reason);
    }
    speak
}

/// A summary for `conbus doctor`.
#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    pub count: usize,
    /// Broken pipe / ENOTCONN during a daemon restart, not a standing fault.
    pub transient: usize,
    /// True once the file stopped growing, so `count` is a floor.
    pub capped: bool,
    pub last: Option<(String, String)>,
}

pub fn is_transient(reason: &str) -> bool {
    reason.contains("Broken pipe")
        || reason.contains("Socket is not connected")
        || reason.contains("Connection reset")
        || reason.contains("Resource temporarily unavailable")
        || reason.contains("unreadable reply \"\"")
}

pub fn summary(paths: &Paths) -> Summary {
    let file = drops_file(paths);
    let Ok(text) = fs::read_to_string(&file) else {
        return Summary {
            count: 0,
            transient: 0,
            capped: false,
            last: None,
        };
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut transient = 0;
    let last = lines.last().and_then(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        let reason = value.get("reason")?.as_str()?.to_string();
        // The detail rides along in the human view, when there is one.
        let reason = match value.get("detail").and_then(|d| d.as_str()) {
            Some(detail) => format!("{reason} ({detail})"),
            None => reason,
        };
        Some((value.get("ts")?.as_str()?.to_string(), reason))
    });
    for line in &lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value
            .get("reason")
            .and_then(|r| r.as_str())
            .is_some_and(is_transient)
        {
            transient += 1;
        }
    }
    Summary {
        count: lines.len(),
        transient,
        capped: text.len() as u64 >= CAP_BYTES,
        last,
    }
}

/// How `doctor` renders it.
pub fn describe(summary: &Summary) -> String {
    match (summary.count, &summary.last) {
        (0, _) => "none recorded".to_string(),
        (n, last) => {
            let at_least = if summary.capped { "at least " } else { "" };
            let standing = n.saturating_sub(summary.transient);
            match (last, summary.transient) {
                (Some((ts, reason)), t) if t > 0 => format!(
                    "{at_least}{n} recorded ({standing} standing, {t} transient connection; last {ts}: {reason})"
                ),
                (Some((ts, reason)), _) => format!("{at_least}{n} recorded; last {ts}: {reason}"),
                (None, _) => format!("{at_least}{n} recorded"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn paths_at(dir: &std::path::Path) -> Paths {
        Paths::from_env(dir.to_str(), None, None).expect("paths")
    }

    #[test]
    fn a_drop_is_written_as_one_readable_line() {
        let dir = home();
        let paths = paths_at(dir.path());

        append(&paths, &Dropped::new("no daemon"));

        let summary = summary(&paths);
        assert_eq!(summary.count, 1);
        assert_eq!(
            summary.last.as_ref().map(|(_, reason)| reason.as_str()),
            Some("no daemon")
        );
    }

    #[test]
    fn a_drop_keeps_what_was_dropped_apart_from_why() {
        // Thirty-one orphan drops on a real log, and no way to tell a ghost
        // session's `SessionEnd` from a lost tool call: the reason said "no
        // binding" and nothing else. The event name and the first eight
        // characters of the vendor's session id are identifiers, not content
        // — the log already carries both in full — and they are enough to
        // sort every one of the thirty-one.
        let dir = home();
        let paths = paths_at(dir.path());

        append(
            &paths,
            &Dropped::new("no conversation for this session")
                .with_detail("SessionEnd for session 01a06300"),
        );

        let line = std::fs::read_to_string(drops_file(&paths)).expect("drop log");
        let value: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(value["reason"], "no conversation for this session");
        assert_eq!(value["detail"], "SessionEnd for session 01a06300");
    }

    #[test]
    fn the_throttle_keys_on_the_reason_and_not_on_the_detail() {
        // Every orphan carries a different session id. If the detail were part
        // of the key, a daemon restart would put one stderr line per orphan
        // into the transcript — the burying this module exists to prevent.
        let dir = home();
        let paths = paths_at(dir.path());

        assert!(should_speak(&paths, "no binding"));
        assert!(!should_speak(&paths, "no binding"));
        let _ = record_at(
            &paths,
            &Dropped::new("no binding").with_detail("Stop for session 11111111"),
        );
        assert!(
            !should_speak(&paths, "no binding"),
            "a different detail is the same reason"
        );
    }

    #[test]
    fn the_same_reason_goes_quiet_but_a_new_one_always_speaks() {
        // Keying the throttle on the drop file meant every drop pushed the
        // window forward, so a standing fault spoke once and then never again.
        // And a standing benign fault must never silence a new one: "no mapping
        // for hook event X" means the vendor's schema moved.
        let dir = home();
        let paths = paths_at(dir.path());

        assert!(
            should_speak(&paths, "no daemon"),
            "the first one must speak"
        );
        assert!(
            !should_speak(&paths, "no daemon"),
            "the same one repeats quietly"
        );
        assert!(!should_speak(&paths, "no daemon"));
        assert!(
            should_speak(&paths, "no mapping for hook event Something"),
            "a new reason must never wait behind a standing one"
        );
        assert!(!should_speak(&paths, "no mapping for hook event Something"));
        assert!(
            should_speak(&paths, "no daemon"),
            "and switching back speaks again"
        );
    }

    #[test]
    fn the_drop_log_stops_growing_and_says_so() {
        let dir = home();
        let paths = paths_at(dir.path());
        let file = drops_file(&paths);
        std::fs::create_dir_all(paths.root()).expect("dir");
        std::fs::write(&file, "x".repeat(CAP_BYTES as usize + 10)).expect("fill");

        append(&paths, &Dropped::new("one more"));

        let grown = std::fs::metadata(&file).expect("meta").len();
        assert_eq!(grown, CAP_BYTES + 10, "it kept growing past the cap");
        assert!(summary(&paths).capped);
        assert!(
            describe(&summary(&paths)).contains("at least"),
            "a capped count must not be reported as exact: {}",
            describe(&summary(&paths))
        );
    }

    #[test]
    fn nothing_recorded_reads_as_nothing() {
        let dir = home();

        assert_eq!(describe(&summary(&paths_at(dir.path()))), "none recorded");
    }

    #[test]
    fn transient_connection_drops_are_counted_apart_from_standing_faults() {
        let dir = home();
        let paths = paths_at(dir.path());
        append(
            &paths,
            &Dropped::new("could not send the record: Broken pipe (os error 32)"),
        );
        append(&paths, &Dropped::new("no daemon on /tmp/x"));
        let summary = summary(&paths);
        assert_eq!(summary.count, 2);
        assert_eq!(summary.transient, 1);
        assert!(
            describe(&summary).contains("1 standing, 1 transient connection"),
            "{}",
            describe(&summary)
        );
    }

    #[test]
    fn an_empty_reply_is_a_transient_drop() {
        assert!(is_transient(
            "unreadable reply \"\": EOF while parsing a value at line 1 column 0"
        ));
        assert!(is_transient(
            "could not send the record: Resource temporarily unavailable (os error 35)"
        ));
        assert!(!is_transient("no conversation for this session"));
    }
}
