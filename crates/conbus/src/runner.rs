//! `conbus run` — the same TTY stays the window while the harness changes.
//!
//! The vendor TUI is still what the person sees. This process is the parent:
//! it starts the current leg, and when `hop --apply` writes a takeover it
//! stops that child and execs the next one. No chat UI. No inject into a
//! thread the user opened — Codex is resumed only by a `warm:` id we minted.

use std::io;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use conbus_core::event::Harness;
use conbus_core::paths::Paths;

use crate::appserver;
use crate::takeover::{self, Takeover};

pub fn run(paths: &Paths, harness: &str) -> io::Result<()> {
    let mut pending = takeover::take(paths)?;
    let mut current = match pending.as_ref() {
        Some(next) => normalize(&next.next)?,
        None => normalize(harness)?,
    };
    loop {
        spawn(paths, &current, pending.as_ref())?;
        pending = takeover::take(paths)?;
        match pending.as_ref() {
            Some(next) => {
                println!(
                    "conbus run: hop → {}{}",
                    next.next,
                    next.thread
                        .as_deref()
                        .map(|id| format!(" · warm {id}"))
                        .unwrap_or_default()
                );
                current = normalize(&next.next)?;
            }
            None => return Ok(()),
        }
    }
}

fn normalize(name: &str) -> io::Result<String> {
    conbus_core::attach::resolve(name)
        .map(str::to_string)
        .ok_or_else(|| {
            let catalog = conbus_core::attach::slugs().collect::<Vec<_>>().join(", ");
            io::Error::other(format!(
                "conbus run starts a catalog leg ({catalog}), not {name:?}"
            ))
        })
}

fn spawn(paths: &Paths, harness: &str, takeover: Option<&Takeover>) -> io::Result<()> {
    let mut child = vendor(harness, takeover)?;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("{harness} exited {status}")))
            };
        }
        if takeover::peek(paths)?.is_some_and(|next| next.next != harness) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn vendor(harness: &str, takeover: Option<&Takeover>) -> io::Result<std::process::Child> {
    let mut cmd = match harness {
        Harness::CODEX => {
            let mut cmd = Command::new(appserver::codex_binary());
            if let Some(thread) = takeover.and_then(|t| t.thread.as_deref()) {
                cmd.args(["exec", "resume", thread, "continue"]);
            }
            cmd
        }
        Harness::CLAUDE_CODE => {
            let mut cmd = Command::new(claude_binary());
            if let Some(thread) = takeover.and_then(|t| t.thread.as_deref()) {
                cmd.args(["--resume", thread]);
            }
            cmd
        }
        Harness::CURSOR => {
            let mut cmd = Command::new(crate::landing::cursor_app_cli());
            cmd.arg("agent");
            if let Some(cwd) = takeover.and_then(|t| t.cwd.as_deref()) {
                cmd.args(["--workspace", cwd, "--trust"]);
            }
            cmd
        }
        _ => Command::new(leg_binary(harness)),
    };
    if let Some(cwd) = takeover.and_then(|t| t.cwd.as_deref())
        && harness != Harness::CURSOR
    {
        cmd.current_dir(cwd);
    }
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env("PATH", appserver::path_for_vendor())
        .spawn()
}

fn claude_binary() -> std::ffi::OsString {
    env_binary("CONBUS_CLAUDE_BIN", "claude")
}

fn leg_binary(harness: &str) -> std::ffi::OsString {
    match harness {
        Harness::CURSOR => env_binary("CONBUS_CURSOR_BIN", "agent"),
        Harness::AGY => env_binary("CONBUS_AGY_BIN", "agy"),
        Harness::OPENCODE => env_binary("CONBUS_OPENCODE_BIN", "opencode"),
        Harness::GROK => env_binary("CONBUS_GROK_BIN", "grok"),
        _ => env_binary("CONBUS_CLAUDE_BIN", "claude"),
    }
}

fn env_binary(key: &str, default: &str) -> std::ffi::OsString {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::takeover::Takeover;
    use conbus_core::paths::Paths;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    static ENV: Mutex<()> = Mutex::new(());

    fn machine() -> (tempfile::TempDir, Paths) {
        let root = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::from_env(root.path().join("conbus").to_str(), None, None).expect("paths");
        fs::create_dir_all(paths.root()).expect("root");
        (root, paths)
    }

    fn stub(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[test]
    fn a_takeover_replaces_the_child_in_the_same_process() {
        let (root, paths) = machine();
        let claude = stub(root.path(), "claude", "sleep 30");
        let marker = root.path().join("codex-started");
        let codex = stub(
            root.path(),
            "codex",
            &format!("echo \"$1 $2 $3\" > {}", marker.display()),
        );
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("CONBUS_CLAUDE_BIN", &claude);
            std::env::set_var("CONBUS_CODEX_BIN", &codex);
        }
        takeover::write(
            &paths,
            &Takeover {
                next: "codex".into(),
                thread: Some("thr_bus_owned".into()),
                bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
                cwd: None,
            },
        )
        .expect("takeover");

        run(&paths, "claude_code").expect("run");

        let text = fs::read_to_string(&marker).expect("codex ran");
        assert!(
            text.contains("exec resume thr_bus_owned"),
            "the same process must start the warm thread, got {text:?}"
        );
        unsafe {
            std::env::remove_var("CONBUS_CLAUDE_BIN");
            std::env::remove_var("CONBUS_CODEX_BIN");
        }
    }

    #[test]
    fn catalog_legs_are_runnable() {
        assert_eq!(normalize("cursor").expect("ok"), Harness::CURSOR);
        assert_eq!(normalize("antigravity").expect("ok"), Harness::AGY);
        assert!(normalize("not_a_harness").is_err());
    }

    #[test]
    fn a_claude_takeover_resumes_the_bus_owned_session() {
        let (root, paths) = machine();
        let marker = root.path().join("claude-started");
        let claude = stub(
            root.path(),
            "claude",
            &format!("echo \"$1 $2\" > {}", marker.display()),
        );
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("CONBUS_CLAUDE_BIN", &claude);
        }
        takeover::write(
            &paths,
            &Takeover {
                next: "claude_code".into(),
                thread: Some("sess_bus_owned".into()),
                bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
                cwd: None,
            },
        )
        .expect("takeover");

        run(&paths, "codex").expect("run");

        let text = fs::read_to_string(&marker).expect("claude ran");
        assert!(
            text.contains("--resume sess_bus_owned"),
            "hop-back must resume the bus-owned session, got {text:?}"
        );
        unsafe {
            std::env::remove_var("CONBUS_CLAUDE_BIN");
        }
    }
}
