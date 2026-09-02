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
/// `unknown` outside a checkout. Set by `build.rs`.
pub const BUILD: &str = env!("CONBUS_BUILD");

fn stamp_file(paths: &Paths) -> PathBuf {
    paths.root().join("daemon.build")
}

/// What the daemon leaves beside its socket.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Stamp {
    pub build: String,
    pub pid: u32,
    pub started: String,
}

/// Write this process's build beside the socket. Written beside and renamed,
/// so a `doctor` reading mid-write sees the old stamp or the new one, never
/// half of either.
pub fn stamp(paths: &Paths) -> io::Result<()> {
    let stamp = Stamp {
        build: BUILD.to_string(),
        pid: std::process::id(),
        started: Timestamp::now().to_string(),
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

        stamp(&paths).expect("stamp");

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
    fn no_stamp_is_reported_as_no_stamp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");

        assert_eq!(daemon_build(&paths), DaemonBuild::Unstamped);
    }
}
