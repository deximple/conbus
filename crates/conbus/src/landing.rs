//! Open the destination the person asked for.
//!
//! Claude and Codex keep bus-owned warms and official deep links. Cursor's
//! official folder entry is `cursor -n <workspace>`. TUI legs open a Terminal
//! only when this process has no TTY — `conbus run` already owns that window.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use conbus_core::event::Harness;

use crate::window;

/// Bring the person to `harness` in `workspace`. `session` is a bus-owned
/// warm id when the destination has one.
pub fn enter(harness: &str, workspace: Option<&str>, session: Option<&str>) -> io::Result<bool> {
    match harness {
        Harness::CLAUDE_CODE | Harness::CODEX | Harness::CLAUDE_CHAT => {
            let Some(id) = session else {
                return Ok(false);
            };
            window::reveal(harness, id)
        }
        Harness::CURSOR => open_cursor(&cursor_app_cli(), workspace),
        Harness::OPENCODE | Harness::GROK | Harness::AGY => open_tui(harness, workspace),
        _ => Ok(false),
    }
}

/// `bin` is named rather than read here: tests pass a stand-in, and setting
/// the environment is unsound while another test thread reads it.
fn open_cursor(bin: &Path, workspace: Option<&str>) -> io::Result<bool> {
    let Some(dir) = workspace.filter(|d| Path::new(d).is_dir()) else {
        return Ok(false);
    };
    let status = Command::new(bin)
        .args(["-n", dir])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn open_tui(harness: &str, workspace: Option<&str>) -> io::Result<bool> {
    if io::stdout().is_terminal() {
        return Ok(false);
    }
    let Some(dir) = workspace.filter(|d| Path::new(d).is_dir()) else {
        return Ok(false);
    };
    let bin = tui_binary(harness);
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Terminal\" to do script \"cd {} && exec {}\"",
            shell_single(dir),
            shell_single(&bin)
        );
        let status = Command::new("osascript")
            .args(["-e", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(status.success())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dir, bin);
        Ok(false)
    }
}

/// Cursor.app's CLI. PATH `cursor` on this machine can be a shim that never
/// finds the app, so the bundle path wins unless an operator overrides it.
pub fn cursor_app_cli() -> PathBuf {
    if let Some(bin) = std::env::var_os("CONBUS_CURSOR_APP_BIN").filter(|v| !v.is_empty()) {
        return PathBuf::from(bin);
    }
    let bundled = PathBuf::from("/Applications/Cursor.app/Contents/Resources/app/bin/cursor");
    if bundled.is_file() {
        return bundled;
    }
    PathBuf::from("cursor")
}

fn tui_binary(harness: &str) -> String {
    let (key, default) = match harness {
        Harness::OPENCODE => ("CONBUS_OPENCODE_BIN", "opencode"),
        Harness::GROK => ("CONBUS_GROK_BIN", "grok"),
        Harness::AGY => ("CONBUS_AGY_BIN", "agy"),
        _ => ("CONBUS_CLAUDE_BIN", "claude"),
    };
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.into())
}

fn shell_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn cursor_opens_a_new_window_on_the_checkout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stub = dir.path().join("cursor");
        let mark = dir.path().join("args");
        std::fs::write(
            &stub,
            format!("#!/bin/sh\nprintf '%s' \"$*\" > {}\n", mark.display()),
        )
        .expect("stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let workspace = dir.path().join("repo");
        std::fs::create_dir(&workspace).expect("repo");
        let opened = open_cursor(&stub, workspace.to_str());
        assert!(opened.expect("open"));
        assert_eq!(
            std::fs::read_to_string(&mark).expect("args"),
            format!("-n {}", workspace.display())
        );
    }

    #[test]
    fn a_missing_checkout_is_not_opened() {
        let opened = enter(Harness::CURSOR, Some("/no/such/conbus-checkout"), None);
        assert!(!opened.expect("skip"));
    }
}
