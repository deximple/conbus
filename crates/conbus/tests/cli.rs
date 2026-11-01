//! The command line's contract: `conbus <cmd> --help` prints the USAGE text,
//! and the exit code says which of three things happened — 0 it worked, 1 it
//! ran and failed, 2 it was not a command at all. `hook` always exits 0: it
//! answers a turn, not an operator.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(bin())
        .args(args)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run conbus");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const COMMANDS: &[&str] = &[
    "daemon",
    "hook",
    "log",
    "install",
    "uninstall",
    "backfill",
    "doctor",
    "quota",
    "hop",
    "go",
    "status",
    "run",
    "arena",
    "summary",
    "verdict",
    "usage",
    "progress",
    "sync",
    "policy",
];

#[test]
fn every_command_answers_help_with_the_usage_text() {
    for command in COMMANDS {
        let (code, stdout, _) = run(&[command, "--help"]);
        assert_eq!(code, 0, "conbus {command} --help must exit 0: {stdout}");
        assert!(
            stdout.contains("USAGE:"),
            "conbus {command} --help must print the usage text: {stdout}"
        );
    }
    for invocation in [&["help"][..], &["--help"][..], &[][..]] {
        let (code, stdout, _) = run(invocation);
        assert_eq!(code, 0, "bare help forms exit 0");
        assert!(stdout.contains("USAGE:"));
    }
}

#[test]
fn usage_errors_exit_two() {
    for (args, why) in [
        (&["frobnicate"][..], "unknown command"),
        (&["daemon", "--bogus"][..], "unknown flag"),
        (
            &["usage", "--days"][..],
            "a value flag missing its argument",
        ),
        (&["log", "--tail"][..], "a value flag missing its argument"),
        (
            &["doctor", "--shareable"][..],
            "a flag from another command",
        ),
    ] {
        let (code, _, stderr) = run(args);
        assert_eq!(
            code, 2,
            "conbus {args:?} ({why}) must exit 2, stderr: {stderr}"
        );
        assert!(
            stderr.contains("USAGE:"),
            "conbus {args:?} should print usage on stderr: {stderr}"
        );
    }
}

#[test]
fn the_hook_exits_zero_even_on_a_flag_it_does_not_know() {
    // The turn cannot wait on usage pedantry: a flag the hook does not know
    // is still just a payload to answer.
    let (code, _, _) = run(&["hook", "--bogus-flag"]);
    assert_eq!(code, 0, "conbus hook always exits 0");
}

/// A pipe nobody is reading: the first write to it fails with `Broken pipe`.
fn a_pipe_with_no_reader() -> Stdio {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    Stdio::from(writer)
}

#[test]
fn a_reader_that_went_away_is_not_a_crash() {
    // `conbus log | head -1` is ordinary use. `println!` panics on the closed
    // pipe, so this printed a panic and a backtrace hint and exited 101, where a
    // Unix tool exits as if SIGPIPE had ended it.
    let out = Command::new(bin())
        .arg("--version")
        .stdout(a_pipe_with_no_reader())
        .stderr(Stdio::piped())
        .output()
        .expect("run");

    assert_eq!(out.status.code(), Some(141));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

#[test]
fn a_hook_whose_reader_went_away_does_not_fail_the_turn() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(bin())
        .args(["hook", "--harness", "claude_code"])
        .env("CONBUS_HOME", home.path())
        .env("HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdin(Stdio::piped())
        .stdout(a_pipe_with_no_reader())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"x"}"#)
        .expect("write");

    let out = child.wait_with_output().expect("wait");

    assert_eq!(
        out.status.code(),
        Some(0),
        "a hook must never fail the turn"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("panicked"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
