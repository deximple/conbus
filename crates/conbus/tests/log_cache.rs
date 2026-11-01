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
