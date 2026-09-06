//! How `conbus run` learns that a hop wants the next vendor in this TTY.
//!
//! The hop records the decision first. This file is only the handoff to the
//! process that already owns the window — it is not a second source of truth.

use std::fs;
use std::io;

use conbus_core::paths::Paths;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Takeover {
    pub next: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    pub bus_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

pub fn write(paths: &Paths, takeover: &Takeover) -> io::Result<()> {
    if let Some(parent) = paths.takeover().parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string(takeover).map_err(io::Error::other)?;
    fs::write(paths.takeover(), text)
}

pub fn take(paths: &Paths) -> io::Result<Option<Takeover>> {
    let path = paths.takeover();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let takeover = serde_json::from_str(&text).map_err(io::Error::other)?;
    fs::remove_file(&path)?;
    Ok(Some(takeover))
}

pub fn peek(paths: &Paths) -> io::Result<Option<Takeover>> {
    let text = match fs::read_to_string(paths.takeover()) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    serde_json::from_str(&text).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::paths::Paths;

    #[test]
    fn a_written_takeover_is_consumed_once() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(root.path().to_str(), None, None).expect("paths");
        write(
            &paths,
            &Takeover {
                next: "codex".into(),
                thread: Some("thr_bus".into()),
                bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
                cwd: Some("/repo".into()),
            },
        )
        .expect("write");

        let first = take(&paths).expect("take").expect("present");
        assert_eq!(first.next, "codex");
        assert_eq!(first.thread.as_deref(), Some("thr_bus"));
        assert!(take(&paths).expect("empty").is_none());
    }
}
