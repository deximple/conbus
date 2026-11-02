//! Which build is running, and which build the daemon is.
//!
//! Deploying is three steps — merge, rename the binary, restart the daemon —
//! and between the last two `doctor` runs new code while the daemon runs old
//! code. On a real machine that window was seconds; in a harness that blocks
//! the restart it can be hours, and nothing said so. So the daemon writes its
//! build beside its socket when it starts, the binary can name its own, and
//! `doctor` puts the two on one line.

use std::io;
use std::path::PathBuf;

use conbus_core::clock::Timestamp;
use conbus_core::paths::Paths;
use serde::{Deserialize, Serialize};

/// The commit this binary was built from — `<sha>`, `<sha>+dirty`, or
/// `unknown` outside a checkout. Set by `build.rs`. This, not the version, is
/// what `doctor` compares with the daemon: two builds of one version differ.
pub const BUILD: &str = env!("CONBUS_BUILD");

/// The release this binary belongs to, from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version where `scripts/sign-release.sh` can read it from a binary it cannot
/// run: the Linux packages, on a Mac. Strings run together in a release build, so the
/// plain version cannot be told from a longer one, and `1.0.0` is inside `1.0.0-rc.3`.
const VERSION_STAMP: &str = concat!("conbus-version ", env!("CARGO_PKG_VERSION"), ";");

/// What `--version` prints: `0.1.0 (a1b2c3d4e)`. It reads the stamp, so the stamp is
/// in every binary that can answer.
pub fn describe() -> String {
    let version = VERSION_STAMP
        .strip_prefix("conbus-version ")
        .and_then(|rest| rest.strip_suffix(';'))
        .unwrap_or(VERSION);
    format!("{version} ({BUILD})")
}

fn stamp_file(paths: &Paths) -> PathBuf {
    paths.root().join("daemon.build")
}

/// What the daemon leaves beside its socket.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Stamp {
    pub build: String,
    pub pid: u32,
    pub started: String,
    /// The retention level this daemon records under, named at first write so
    /// a hook can ask whether spooling is consented to. Absent on older stamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
    /// Set only once the socket is bound and answered — a stamp without it is
    /// a daemon still preparing, and no client should wait on its socket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serving: Option<String>,
    /// Set after restore+relock. Absent on a first write or an older daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_dumps_disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_lock_error: Option<String>,
}

/// Write this process's build beside the socket — the first stamp, before
/// the socket exists, so a client sees a daemon preparing rather than a dead
/// one. Written beside and renamed, so a `doctor` reading mid-write sees the
/// old stamp or the new one, never half of either.
pub fn stamp(paths: &Paths, retention: &str) -> io::Result<()> {
    write_stamp(paths, None, Some(retention), false)
}

/// The second stamp: the socket is bound and answering. Anything before this
/// is a starting daemon, which `doctor` and a spooling hook both tell apart
/// from a serving one.
pub fn stamp_serving(paths: &Paths) -> io::Result<()> {
    write_stamp(paths, None, None, true)
}

/// Record what locking actually did, after restore grew the heap.
pub fn stamp_hardening(
    paths: &Paths,
    core_dumps_disabled: bool,
    memory_locked: bool,
    memory_lock_error: Option<String>,
) -> io::Result<()> {
    write_stamp(
        paths,
        Some(HardenStamp {
            core_dumps_disabled,
            memory_locked,
            memory_lock_error,
        }),
        None,
        false,
    )
}

struct HardenStamp {
    core_dumps_disabled: bool,
    memory_locked: bool,
    memory_lock_error: Option<String>,
}

fn write_stamp(
    paths: &Paths,
    harden: Option<HardenStamp>,
    retention: Option<&str>,
    serving: bool,
) -> io::Result<()> {
    let previous = std::fs::read_to_string(stamp_file(paths))
        .ok()
        .and_then(|text| serde_json::from_str::<Stamp>(text.trim()).ok())
        .filter(|seen| seen.pid == std::process::id() && seen.build == BUILD);
    let stamp = Stamp {
        build: BUILD.to_string(),
        pid: std::process::id(),
        started: previous
            .as_ref()
            .map(|seen| seen.started.clone())
            .unwrap_or_else(|| Timestamp::now().to_string()),
        retention: retention
            .map(String::from)
            .or_else(|| previous.as_ref().and_then(|s| s.retention.clone())),
        serving: if serving {
            Some(Timestamp::now().to_string())
        } else {
            previous.as_ref().and_then(|s| s.serving.clone())
        },
        core_dumps_disabled: harden
            .as_ref()
            .map(|h| h.core_dumps_disabled)
            .or_else(|| previous.as_ref().and_then(|s| s.core_dumps_disabled)),
        memory_locked: harden
            .as_ref()
            .map(|h| h.memory_locked)
            .or_else(|| previous.as_ref().and_then(|s| s.memory_locked)),
        memory_lock_error: match &harden {
            Some(h) => h.memory_lock_error.clone(),
            None => previous.and_then(|s| s.memory_lock_error),
        },
    };
    let text = serde_json::to_string(&stamp).map_err(io::Error::other)?;
    let file = stamp_file(paths);
    let beside = file.with_extension("build.new");
    std::fs::write(&beside, text + "\n")?;
    std::fs::rename(&beside, &file)
}

/// The daemon's stamp, and whether the process that wrote it is still there.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DaemonBuild {
    /// A daemon wrote it and is alive under that pid.
    Running(Stamp),
    /// A daemon wrote it and is gone; the stamp is what it left.
    Stale(Stamp),
    /// No stamp at all: no daemon has started here, or one older than this
    /// feature did.
    Unstamped,
}

pub fn daemon_build(paths: &Paths) -> DaemonBuild {
    let Ok(text) = std::fs::read_to_string(stamp_file(paths)) else {
        return DaemonBuild::Unstamped;
    };
    let Ok(stamp) = serde_json::from_str::<Stamp>(text.trim()) else {
        return DaemonBuild::Unstamped;
    };
    if alive(stamp.pid) {
        DaemonBuild::Running(stamp)
    } else {
        DaemonBuild::Stale(stamp)
    }
}

/// Whether a process with this pid exists. Signal 0 delivers nothing and
/// only asks; `EPERM` means it exists and belongs to somebody else, which is
/// still "alive" for this question.
fn alive(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 sends no signal; it only checks for the
    // process. The pid is a plain integer read from our own stamp file and
    // any value is safe to ask about.
    let answer = unsafe { libc::kill(pid as libc::pid_t, 0) };
    answer == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_is_named_and_a_stamp_round_trips() {
        assert!(!BUILD.is_empty());
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");

        stamp(&paths, "conversation_only").expect("stamp");

        match daemon_build(&paths) {
            DaemonBuild::Running(found) => {
                assert_eq!(found.build, BUILD);
                assert_eq!(found.pid, std::process::id());
            }
            other => panic!("this process is alive and wrote the stamp: {other:?}"),
        }
    }

    #[test]
    fn a_stamp_from_a_process_that_is_gone_is_stale_not_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        // The largest pid macOS hands out is far below this.
        std::fs::write(
            stamp_file(&paths),
            r#"{"build":"abc","pid":4000000000,"started":"2026-09-02T08:14:53.000Z"}"#,
        )
        .expect("write");

        assert!(matches!(daemon_build(&paths), DaemonBuild::Stale(_)));
    }

    #[test]
    fn hardening_is_stamped_after_the_build_and_an_old_stamp_still_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        stamp(&paths, "conversation_only").expect("stamp");
        match daemon_build(&paths) {
            DaemonBuild::Running(found) => assert_eq!(found.memory_locked, None),
            other => panic!("{other:?}"),
        }

        stamp_hardening(&paths, true, true, None).expect("harden");
        match daemon_build(&paths) {
            DaemonBuild::Running(found) => {
                assert_eq!(found.core_dumps_disabled, Some(true));
                assert_eq!(found.memory_locked, Some(true));
                assert_eq!(found.memory_lock_error, None);
            }
            other => panic!("{other:?}"),
        }

        serde_json::from_str::<Stamp>(
            r#"{"build":"abc","pid":1,"started":"2026-09-02T08:14:53.000Z"}"#,
        )
        .expect("old shape still reads");
    }

    #[test]
    fn no_stamp_is_reported_as_no_stamp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");

        assert_eq!(daemon_build(&paths), DaemonBuild::Unstamped);
    }
}
