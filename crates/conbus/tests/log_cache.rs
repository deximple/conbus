//! `--tail` must never open a segment it does not need: with every older
//! segment unreadable, the last record still prints.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

/// A root with four closed segments and one open, records 1..=30 spread
/// across them.
fn root_with_five_segments() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let log = home.path().join("log");
    std::fs::create_dir(&log).expect("log dir");
    for segment in 0..5u32 {
        let mut file = std::fs::File::create(log.join(format!("segment-{segment:08}.jsonl")))
            .expect("segment");
        for seq in 0..6u64 {
            let line = serde_json::json!({
                "v": 3,
                "ts": "2026-11-01T00:00:00.000Z",
                "seq": segment as u64 * 6 + seq,
                "bus_id": "01a19463-cbd4-7b50-b819-1c9ecf85ef18",
                "harness": "claude_code",
                "event": "session_candidate"
            });
            writeln!(file, "{line}").expect("write");
        }
    }
    home
}

#[test]
fn tail_never_opens_the_segments_it_skips() {
    let home = root_with_five_segments();
    for entry in std::fs::read_dir(home.path().join("log")).expect("read dir") {
        let path = entry.expect("entry").path();
        if !path.ends_with("segment-00000004.jsonl") {
            let mut mode = std::fs::metadata(&path).expect("meta").permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                mode.set_mode(0o000);
            }
            std::fs::set_permissions(&path, mode).expect("chmod");
        }
    }

    let output = Command::new(bin())
        .arg("log")
        .arg("--tail")
        .arg("1")
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("log");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("session-start"),
        "the last record must print: {stdout}"
    );
}

const FIRST: &str = "01a19463-cbd4-7b50-b819-000000000001";
const SECOND: &str = "01a19463-cbd4-7b50-b819-000000000002";

fn turn(seq: u64, bus_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "v": 3, "ts": "2026-11-01T00:00:00.000Z", "seq": seq, "bus_id": bus_id,
        "harness": "claude_code", "vendor_session_id": "split-session",
        "event": "turn_started", "prompt": {"kind": "kept", "text": text}
    })
}

fn alias(seq: u64, bus_id: &str, also: &str) -> serde_json::Value {
    serde_json::json!({
        "v": 3, "ts": "2026-11-01T00:00:00.000Z", "seq": seq, "bus_id": bus_id,
        "harness": "claude_code", "vendor_session_id": "split-session",
        "event": "aliased", "also": also
    })
}

fn root_holding(segments: &[&[serde_json::Value]]) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let log = home.path().join("log");
    std::fs::create_dir(&log).expect("log dir");
    for (index, lines) in segments.iter().enumerate() {
        let mut file =
            std::fs::File::create(log.join(format!("segment-{index:08}.jsonl"))).expect("segment");
        for line in *lines {
            writeln!(file, "{line}").expect("write");
        }
    }
    home
}

/// Run `conbus log --json` with these arguments: what it printed, line by line,
/// and what it said on stderr.
fn log_lines(home: &std::path::Path, args: &[&str]) -> (Vec<serde_json::Value>, String) {
    let output = Command::new(bin())
        .args(["log", "--json"])
        .args(args)
        .env("CONBUS_HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .stdin(Stdio::null())
        .output()
        .expect("log");
    assert_eq!(output.status.code(), Some(0));
    let lines = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("a record"))
        .collect();
    (lines, String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn tail_and_a_full_read_agree_on_an_aliased_conversation() {
    let home = root_holding(&[&[
        turn(0, FIRST, "under the first id"),
        turn(1, SECOND, "under the second id"),
        alias(2, FIRST, SECOND),
        alias(3, SECOND, FIRST),
    ]]);

    let (full, _) = log_lines(home.path(), &["--bus-id", FIRST]);
    let (tailed, _) = log_lines(home.path(), &["--bus-id", FIRST, "--tail", "100"]);

    assert_eq!(full.len(), 4);
    assert_eq!(tailed, full);
}

#[test]
fn a_tail_follows_an_alias_written_long_before_its_window() {
    // `aliased` lines are written at the start after a split, so a conversation
    // that kept going has them far behind its last few records.
    let home = root_holding(&[
        &[
            turn(0, FIRST, "under the first id"),
            alias(1, FIRST, SECOND),
            alias(2, SECOND, FIRST),
        ],
        &[
            turn(3, FIRST, "first, later"),
            turn(4, SECOND, "second, last"),
        ],
    ]);

    let (tailed, _) = log_lines(home.path(), &["--bus-id", FIRST, "--tail", "2"]);

    let seqs: Vec<u64> = tailed
        .iter()
        .filter_map(|record| record["seq"].as_u64())
        .collect();
    assert_eq!(seqs, vec![3, 4], "the newest two of either id");
}

#[test]
fn a_tail_without_a_conversation_still_reads_only_what_it_needs() {
    // The cost of following aliases belongs to the read that named one.
    let home = root_with_five_segments();
    let oldest = home.path().join("log/segment-00000000.jsonl");
    let mut mode = std::fs::metadata(&oldest).expect("meta").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        mode.set_mode(0o000);
    }
    std::fs::set_permissions(&oldest, mode).expect("chmod");

    let (tailed, stderr) = log_lines(home.path(), &["--tail", "1"]);

    assert_eq!(tailed.len(), 1);
    assert!(!stderr.contains("aliases"), "{stderr}");
}

#[test]
fn a_tail_for_a_conversation_survives_a_segment_it_cannot_read_and_says_so() {
    let home = root_holding(&[
        &[alias(0, FIRST, SECOND), alias(1, SECOND, FIRST)],
        &[turn(2, FIRST, "first, last")],
    ]);
    let older = home.path().join("log/segment-00000000.jsonl");
    let mut mode = std::fs::metadata(&older).expect("meta").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        mode.set_mode(0o000);
    }
    std::fs::set_permissions(&older, mode).expect("chmod");

    let (tailed, stderr) = log_lines(home.path(), &["--bus-id", FIRST, "--tail", "1"]);

    assert_eq!(tailed.len(), 1, "the id it was given still matches");
    assert!(stderr.contains("aliases"), "{stderr}");
}
