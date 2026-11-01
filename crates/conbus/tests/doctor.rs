//! Doctor's vendor probes each get a hard deadline — a `codex` that hangs
//! cannot hold the report.

mod support;

use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

#[test]
fn a_codex_that_never_answers_does_not_hold_doctor() {
    let home = tempfile::tempdir().expect("tempdir");
    let stub_dir = tempfile::tempdir().expect("stub dir");
    // Answers nothing, ever — the shape of a wedged app-server.
    support::write_executable(&stub_dir.path().join("codex"), "#!/bin/sh\nexec sleep 60\n");

    let started = Instant::now();
    let output = Command::new(bin())
        .arg("doctor")
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_CODEX_BIN", stub_dir.path().join("codex"))
        // Left unset this ran the machine's real `agy`, whose answer could also
        // satisfy the assertion below.
        .env("CONBUS_AGY_BIN", "/nonexistent/conbus-test-vendor")
        .output()
        .expect("run doctor");
    let elapsed = started.elapsed();
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(
        elapsed < Duration::from_secs(20),
        "doctor held for {elapsed:?} on a hung vendor"
    );
    assert!(
        text.contains("did not answer within 2s"),
        "the probe names its deadline: {text}"
    );
    assert_eq!(output.status.code(), Some(0));
}
