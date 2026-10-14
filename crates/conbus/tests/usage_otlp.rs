//! End-to-end: `conbus usage --otlp`, from a real log on disk, through the
//! real binary, to a collector on 127.0.0.1 — a listener standing in for one,
//! which keeps what it was sent. Loopback only: nothing here leaves the machine.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::thread::{self, JoinHandle};

use conbus_core::event::{Body, Entrypoint, Event, Harness, Source, ToolOutcome, TurnState};
use conbus_core::record::Entry;
use conbus_core::retention::Retention;
use conbus_core::segment::SegmentLog;

const DAY: &str = "2026-09-22";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

/// A day of unattended Claude Code work, written through the log's own
/// writer: a prompt, a failed tool call, a limit stop. Every body is a canary.
fn a_day_of_work(home: &Path) {
    let mut log = SegmentLog::open(home.join("log"), Retention::Full).expect("log");
    let source = || Source {
        harness: Harness::new(Harness::CLAUDE_CODE),
        harness_version: None,
        vendor_session_id: Some("sess-canary".into()),
        workspace: Some("/Users/someone/secret-project".into()),
        entrypoint: Some(Entrypoint::new("sdk-cli")),
        backfilled_from: None,
        profile: None,
    };
    let events = [
        Event::TurnStarted {
            author: None,
            prompt: Body::kept("the secret plan"),
        },
        Event::ToolUse {
            tool: "Bash".into(),
            tool_use_id: None,
            duration_ms: Some(40),
            input: Body::kept("cargo test"),
            outcome: ToolOutcome::Failed {
                error: Body::kept("password=hunter2"),
                interrupted: false,
            },
        },
        Event::TurnEnded {
            state: TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            },
            assistant_message: None,
            tokens: None,
        },
    ];
    for (second, event) in events.into_iter().enumerate() {
        log.append(Entry {
            bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
            observed_at: Some(format!("{DAY}T09:00:0{second}.000Z").parse().expect("ts")),
            source: source(),
            vendor_turn_id: None,
            vendor_agent_id: None,
            event,
        })
        .expect("append");
    }
}

fn usage(home: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("usage")
        .args(args)
        .env("CONBUS_HOME", home)
        .output()
        .expect("run conbus usage")
}

/// A collector that answers one request with a 200 and keeps the request.
fn collector() -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().expect("addr").port()
    );
    let request = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).expect("read");
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
            let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if raw.len() >= end + 4 + length {
                break;
            }
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            )
            .expect("answer");
        raw
    });
    (url, request)
}

#[test]
fn the_rows_shareable_prints_are_what_the_collector_receives() {
    let home = tempfile::tempdir().expect("home");
    a_day_of_work(home.path());
    let (url, request) = collector();

    let out = usage(home.path(), &["--day", DAY, "--shareable", "--otlp", &url]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "otlp: sent 1 row(s) as 24 data point(s) to {url}/v1/metrics (200 OK)"
        )),
        "{stderr}"
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("a row"))
        .collect();
    assert_eq!(rows.len(), 1, "{stdout}");

    let raw = request.join().expect("collector");
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("head");
    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
    assert!(head.starts_with("post /v1/metrics http/1.1\r\n"), "{head}");
    assert!(
        head.contains("\r\ncontent-type: application/json"),
        "{head}"
    );
    let text = String::from_utf8(raw[end + 4..].to_vec()).expect("utf8");
    for leaked in [
        "secret plan",
        "Bash",
        "cargo test",
        "hunter2",
        "rate_limit",
        "secret-project",
        "sdk-cli",
        "sess-canary",
        "01996b1e",
    ] {
        assert!(
            !text.contains(leaked),
            "{leaked} reached the collector: {text}"
        );
    }
    let body: serde_json::Value = serde_json::from_str(&text).expect("a JSON body");
    let metrics = body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
        .as_array()
        .expect("metrics");
    let sum_of = |name: &str, kind: Option<&str>| -> u64 {
        metrics
            .iter()
            .filter(|metric| metric["name"] == name)
            .flat_map(|metric| {
                metric["sum"]["dataPoints"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|point| {
                let kind_of = point["attributes"].as_array().and_then(|kvs| {
                    kvs.iter()
                        .find(|kv| kv["key"] == "kind")
                        .and_then(|kv| kv["value"]["stringValue"].as_str())
                });
                kind_of == kind
            })
            .map(|point| {
                point["asInt"]
                    .as_str()
                    .expect("asInt")
                    .parse::<u64>()
                    .expect("int")
            })
            .sum()
    };
    let row = &rows[0];
    assert_eq!(
        sum_of("conbus.turns", None),
        row["turns"].as_u64().expect("turns")
    );
    assert_eq!(
        sum_of("conbus.stops", Some("limit")),
        row["limit_stops"].as_u64().expect("limit")
    );
    assert_eq!(
        sum_of("conbus.tool.failures", None),
        row["tool_failures"].as_u64().expect("failures")
    );
    assert_eq!(
        sum_of("conbus.tool.duration", None),
        row["tool_ms"].as_u64().expect("ms")
    );
    assert_eq!(
        (row["turns"].as_u64(), row["tool_ms"].as_u64()),
        (Some(1), Some(40))
    );
}

#[test]
fn a_url_the_pipe_does_not_speak_is_refused_before_anything_is_printed_or_sent() {
    let home = tempfile::tempdir().expect("home");
    a_day_of_work(home.path());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("addr").port();

    for (url, says) in [
        (format!("https://127.0.0.1:{port}"), "plain HTTP"),
        (
            format!("http://127.0.0.1:{port}/v1/metrics?x=1"),
            "http://127.0.0.1:4318",
        ),
        (
            "http://collector.example.com:4318".to_string(),
            "on this machine",
        ),
    ] {
        let out = usage(home.path(), &["--day", DAY, "--otlp", &url]);

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{url} was accepted: {stderr}");
        assert!(stderr.contains(says), "{url}: {stderr}");
        assert!(out.stdout.is_empty(), "{url} printed a report first");
    }
    let nobody = listener
        .accept()
        .map(|_| ())
        .expect_err("nothing connected");
    assert_eq!(nobody.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn a_collector_that_is_not_there_is_an_error_and_a_non_zero_exit() {
    let home = tempfile::tempdir().expect("home");
    a_day_of_work(home.path());
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };

    let out = usage(
        home.path(),
        &["--day", DAY, "--otlp", &format!("http://127.0.0.1:{port}")],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("no collector is listening at 127.0.0.1:{port}")),
        "{stderr}"
    );
}
