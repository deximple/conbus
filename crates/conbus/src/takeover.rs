//! How `conbus run` learns that a hop wants the next vendor in this TTY.
//!
//! The hop records the decision first. This file is only the handoff to the
//! process that already owns the window — it is not a second source of truth.
//!
//! A handoff nobody takes is not an instruction for later. `conbus run` looks
//! every 100 ms, so a live one takes a takeover a moment after it is written;
//! one still on disk minutes later was meant for a process that is not there.
//! On 2026-09-08 the automatic hops of a conversation nobody was running under
//! `conbus run` left one behind, and fourteen days later the next
//! `conbus run claude_code` would still have resumed that conversation's Codex
//! thread, in another project's checkout. So a takeover carries when it was
//! written, and one older than [`FRESH_FOR_SECS`] is thrown away where it is
//! found rather than followed.

use std::fs;
use std::io;

use conbus_core::clock::{Timestamp, ago};
use conbus_core::paths::Paths;
use serde::{Deserialize, Serialize};

/// How long a takeover stays an instruction. Long enough to run `conbus run`
/// by hand after `conbus go` in another terminal; short enough that nothing
/// written yesterday is ever followed.
pub const FRESH_FOR_SECS: i64 = 10 * 60;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Takeover {
    pub next: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    pub bus_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// What is on disk: the takeover, and when it was written.
#[derive(Serialize)]
struct Written<'a> {
    #[serde(flatten)]
    takeover: &'a Takeover,
    written_at: Timestamp,
}

/// What is read back. `written_at` is absent on files written before it
/// existed, and those are exactly the ones that have sat there longest.
#[derive(Deserialize)]
struct Found {
    #[serde(flatten)]
    takeover: Takeover,
    #[serde(default)]
    written_at: Option<Timestamp>,
}

/// What `take` found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Taken {
    /// Nothing was waiting.
    Nothing,
    /// A hop written `age` seconds ago, for the process that owns this window.
    Fresh { takeover: Takeover, age: i64 },
    /// A hop nobody took in time — or one that cannot be read — now removed.
    /// `age` is `None` when the file does not say when it was written.
    Stale { next: String, age: Option<i64> },
}

pub fn write(paths: &Paths, takeover: &Takeover) -> io::Result<()> {
    let text = serde_json::to_string(&Written {
        takeover,
        written_at: Timestamp::now(),
    })
    .map_err(io::Error::other)?;
    // `conbus run` reads this every 100 ms; a torn file there is a hop lost.
    crate::install::write_atomically(&paths.takeover(), &text)
}

/// Take the pending hop, if there is one, and remove it either way.
pub fn take(paths: &Paths) -> io::Result<Taken> {
    take_at(paths, Timestamp::now())
}

fn take_at(paths: &Paths, now: Timestamp) -> io::Result<Taken> {
    let taken = match read(paths)? {
        None => return Ok(Taken::Nothing),
        Some(found) => judge(found, now),
    };
    match fs::remove_file(paths.takeover()) {
        Ok(()) => {}
        // Another reader took it between our read and our remove.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Taken::Nothing),
        Err(err) => return Err(err),
    }
    Ok(taken)
}

/// The pending hop without taking it — fresh ones only. A stale file is not a
/// reason to stop the leg that is running.
pub fn peek(paths: &Paths) -> io::Result<Option<Takeover>> {
    Ok(
        match read(paths)?.map(|found| judge(found, Timestamp::now())) {
            Some(Taken::Fresh { takeover, .. }) => Some(takeover),
            _ => None,
        },
    )
}

/// One line for `doctor` and `status` when a takeover is waiting.
pub fn describe(paths: &Paths) -> Option<String> {
    describe_at(paths, Timestamp::now())
}

fn describe_at(paths: &Paths, now: Timestamp) -> Option<String> {
    let taken = judge(read(paths).ok()??, now);
    Some(match taken {
        Taken::Fresh { takeover, age } => format!(
            "pending → {}{} · written {} ago; the running `conbus run` takes it",
            takeover.next,
            takeover
                .thread
                .as_deref()
                .map(|id| format!(" · warm {id}"))
                .unwrap_or_default(),
            ago(age)
        ),
        Taken::Stale { next, age } => format!(
            "stale → {next} · {}; the next `conbus run` discards it instead of following it",
            match age {
                Some(age) => format!("written {} ago", ago(age)),
                None => "written by an older build that did not say when".into(),
            }
        ),
        Taken::Nothing => return None,
    })
}

fn read(paths: &Paths) -> io::Result<Option<Result<Found, String>>> {
    match fs::read_to_string(paths.takeover()) {
        Ok(text) => Ok(Some(
            serde_json::from_str::<Found>(&text).map_err(|err| err.to_string()),
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn judge(found: Result<Found, String>, now: Timestamp) -> Taken {
    let Ok(found) = found else {
        // Unreadable is unfollowable. Refusing to start the vendor over it
        // would turn a stray file into an outage.
        return Taken::Stale {
            next: "an unreadable file".into(),
            age: None,
        };
    };
    let age = found.written_at.map(|at| now.seconds_since(at));
    match age {
        // A clock that moved backwards by less than the window is still fresh.
        Some(age) if (-FRESH_FOR_SECS..=FRESH_FOR_SECS).contains(&age) => Taken::Fresh {
            takeover: found.takeover,
            age: age.max(0),
        },
        _ => Taken::Stale {
            next: found.takeover.next,
            age,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::paths::Paths;

    fn machine() -> (tempfile::TempDir, Paths) {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(root.path().to_str(), None, None).expect("paths");
        (root, paths)
    }

    fn hop() -> Takeover {
        Takeover {
            next: "codex".into(),
            thread: Some("thr_bus".into()),
            bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
            cwd: Some("/repo".into()),
        }
    }

    fn later(seconds: i64) -> Timestamp {
        let now = Timestamp::now();
        let unix = now.seconds_since(Timestamp::from_unix_seconds(0).expect("epoch"));
        Timestamp::from_unix_seconds(unix + seconds).expect("ts")
    }

    #[test]
    fn a_written_takeover_is_consumed_once() {
        let (_root, paths) = machine();
        write(&paths, &hop()).expect("write");

        let Taken::Fresh { takeover, age } = take(&paths).expect("take") else {
            panic!("a takeover written just now is fresh");
        };
        assert_eq!(takeover, hop());
        assert!(age < 5, "{age}");
        assert_eq!(take(&paths).expect("empty"), Taken::Nothing);
    }

    #[test]
    fn a_takeover_nobody_took_in_time_is_discarded_not_followed() {
        let (_root, paths) = machine();
        write(&paths, &hop()).expect("write");

        // `later` is whole seconds, so it can land up to a second short.
        let taken = take_at(&paths, later(FRESH_FOR_SECS + 5)).expect("take");

        let Taken::Stale {
            next,
            age: Some(age),
        } = taken
        else {
            panic!("a takeover past the window must be stale: {taken:?}");
        };
        assert_eq!(next, "codex");
        assert!(age > FRESH_FOR_SECS, "{age}");
        assert!(
            !paths.takeover().exists(),
            "a stale takeover must not linger"
        );
    }

    #[test]
    fn a_takeover_from_an_older_build_says_nothing_about_its_age_and_is_stale() {
        // The exact shape found on this machine fourteen days after it was
        // written: no `written_at`, because nothing wrote one yet.
        let (_root, paths) = machine();
        fs::create_dir_all(paths.root()).expect("root");
        fs::write(
            paths.takeover(),
            r#"{"next":"codex","thread":"01a081ef-918e-7042-a2ce-f0336e458ea9","bus_id":"01a081ef-889d-76d0-9e03-2352684e8e0c","cwd":"/elsewhere"}"#,
        )
        .expect("legacy");

        assert!(
            describe(&paths)
                .expect("described")
                .starts_with("stale → codex"),
            "{:?}",
            describe(&paths)
        );
        assert_eq!(
            peek(&paths).expect("peek"),
            None,
            "a stale file must not stop a leg"
        );
        assert_eq!(
            take(&paths).expect("take"),
            Taken::Stale {
                next: "codex".into(),
                age: None
            }
        );
        assert!(!paths.takeover().exists());
    }

    #[test]
    fn an_unreadable_takeover_is_stale_rather_than_an_error_that_stops_the_run() {
        let (_root, paths) = machine();
        fs::create_dir_all(paths.root()).expect("root");
        fs::write(paths.takeover(), "{not json").expect("torn");

        assert!(matches!(
            take(&paths).expect("take must not fail"),
            Taken::Stale { age: None, .. }
        ));
        assert!(!paths.takeover().exists());
    }

    #[test]
    fn peek_sees_a_fresh_takeover_and_leaves_it() {
        let (_root, paths) = machine();
        write(&paths, &hop()).expect("write");

        assert_eq!(peek(&paths).expect("peek"), Some(hop()));
        assert!(paths.takeover().exists(), "peek must not consume");
        assert!(
            describe(&paths)
                .expect("described")
                .starts_with("pending → codex · warm thr_bus"),
            "{:?}",
            describe(&paths)
        );
    }
}
