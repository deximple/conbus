//! The hop's `steps` line names every step it timed — decide, lease, warm,
//! open — because a twenty-minute hop once left no account of where the
//! time went.

mod support;

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

#[test]
fn the_steps_line_names_each_step_it_timed() {
    let home = tempfile::tempdir().expect("tempdir");
    let stubs = tempfile::tempdir().expect("stubs");
    let log = home.path().join("log");
    std::fs::create_dir_all(&log).expect("log dir");
    // A real live conversation: a candidate flushed by its first prompt.
    let mut segment = include_str!("../../conbus-core/tests/fixtures/segment-v3.jsonl")
        .lines()
        .take(2)
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    segment.push('\n');
    std::fs::write(log.join("segment-00000000.jsonl"), segment).expect("segment");

    // Vendor stubs that are alive but take a moment — the warm step should
    // measure them, not round them away.
    for name in ["codex", "claude", "cursor"] {
        support::write_executable(&stubs.path().join(name), "#!/bin/sh\nsleep 1\nexit 0\n");
    }

    // `hop --apply` records through the daemon — one must be serving.
    let mut daemon = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .env("HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("daemon");
    let socket = home.path().join("conbus.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !socket.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let output = Command::new(bin())
        .args(["hop", "--to", "cursor", "--apply"])
        .env("CONBUS_HOME", home.path())
        .env("HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", stubs.path().join("codex"))
        .env("CONBUS_CLAUDE_BIN", stubs.path().join("claude"))
        .env("CONBUS_CURSOR_APP_BIN", stubs.path().join("cursor"))
        .output()
        .expect("hop --apply");
    let _ = daemon.kill();
    let _ = daemon.wait();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let steps = stdout
        .lines()
        .find(|line| line.starts_with("steps"))
        .unwrap_or_else(|| panic!("no steps line in {stdout}"));
    for step in ["decide", "lease", "warm", "open", "total"] {
        assert!(
            steps.contains(&format!("{step} ")),
            "the steps line names {step}: {steps}"
        );
    }
}
