//! Opt-in: `conbus usage --otlp` against a real OpenTelemetry Collector.
//!
//! `usage_otlp.rs` checks the request against a listener that stands in for a
//! collector. This checks that a real one accepts it and keeps every point as
//! `--shareable` printed it. It needs a collector binary, so it is ignored
//! unless asked for:
//!
//! ```sh
//! CONBUS_OTELCOL=/path/to/otelcol cargo test -p conbus --test otlp_collector -- --ignored
//! ```
//!
//! The core `otelcol` distribution has everything it uses: the OTLP receiver
//! and the file exporter. First run on 2026-09-24 with otelcol 0.161.0
//! (darwin arm64): 2 rows, 40 points, all equal. The collector listens on a
//! loopback port of its own and nothing leaves the machine.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use conbus_core::event::{Body, Entrypoint, Event, Harness, Source, ToolOutcome, TurnState};
use conbus_core::record::Entry;
use conbus_core::retention::Retention;
use conbus_core::segment::SegmentLog;
use serde_json::Value;

const DAY: &str = "2026-09-22";
/// 2026-09-22T00:00:00Z and the next midnight, in nanoseconds.
const DAY_START: u64 = 1_790_035_200_000_000_000;
const DAY_END: u64 = 1_790_121_600_000_000_000;

/// Each row field, and the metric RFD 0002's "The pipe" says it becomes:
/// name, whether it is a sum, its unit, and the `kind` of a stop.
const METRICS: [(&str, &str, bool, &str, Option<&str>); 20] = [
    (
        "conversations",
        "conbus.conversations",
        false,
        "{conversation}",
        None,
    ),
    ("turns", "conbus.turns", true, "{turn}", None),
    ("limit_stops", "conbus.stops", true, "{stop}", Some("limit")),
    (
        "account_stops",
        "conbus.stops",
        true,
        "{stop}",
        Some("account"),
    ),
    (
        "vendor_errors",
        "conbus.stops",
        true,
        "{stop}",
        Some("vendor"),
    ),
    (
        "request_faults",
        "conbus.stops",
        true,
        "{stop}",
        Some("request"),
    ),
    ("other_stops", "conbus.stops", true, "{stop}", Some("other")),
    ("user_stops", "conbus.stops", true, "{stop}", Some("person")),
    (
        "limit_episodes",
        "conbus.limit_episodes",
        true,
        "{episode}",
        None,
    ),
    ("subagents", "conbus.subagents", true, "{run}", None),
    ("tool_calls", "conbus.tool.calls", true, "{call}", None),
    (
        "tool_failures",
        "conbus.tool.failures",
        true,
        "{call}",
        None,
    ),
    (
        "tool_interrupts",
        "conbus.tool.interrupts",
        true,
        "{call}",
        None,
    ),
    ("tool_denials", "conbus.tool.denials", true, "{call}", None),
    (
        "permission_asks",
        "conbus.permission_asks",
        true,
        "{ask}",
        None,
    ),
    ("tool_ms", "conbus.tool.duration", true, "ms", None),
    ("briefings", "conbus.briefings", true, "{briefing}", None),
    ("hops", "conbus.hops", true, "{hop}", None),
    ("repeated", "conbus.repeated", true, "{record}", None),
    ("recovered", "conbus.recovered", true, "{record}", None),
];

/// A collector run from a config of this test's own, stopped however the
/// test ends.
struct Collector {
    child: Child,
    received: PathBuf,
    port: u16,
}

impl Collector {
    /// `port` can only be probed by freeing it first; when another process
    /// takes it before the collector binds, start over with a fresh one.
    fn start(binary: &Path, dir: &Path) -> Collector {
        for attempt in 0..5 {
            if let Some(collector) = Self::try_start(binary, dir, free_port()) {
                return collector;
            }
            assert!(attempt < 4, "the port kept being taken between probes");
        }
        unreachable!()
    }

    fn try_start(binary: &Path, dir: &Path, port: u16) -> Option<Collector> {
        let received = dir.join("received.jsonl");
        let config = dir.join("config.yaml");
        std::fs::write(
            &config,
            format!(
                "receivers:\n  otlp:\n    protocols:\n      http:\n        endpoint: 127.0.0.1:{port}\n\
                 exporters:\n  file:\n    path: {}\n\
                 service:\n  telemetry:\n    metrics:\n      level: none\n\
                 \x20 pipelines:\n    metrics:\n      receivers: [otlp]\n      exporters: [file]\n",
                received.display()
            ),
        )
        .expect("config");
        let log = std::fs::File::create(dir.join("collector.log")).expect("collector log");
        let child = Command::new(binary)
            .arg("--config")
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("start the collector");
        let mut collector = Collector {
            child,
            received,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            if Instant::now() >= deadline {
                let _ = collector.child.kill();
                let _ = collector.child.wait();
                eprintln!(
                    "the collector did not listen on {port}; see {}",
                    dir.join("collector.log").display()
                );
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Some(collector)
    }

    /// Stop it the way an operator would, so the file exporter writes out
    /// what it holds, and return what it received.
    fn stop(mut self) -> String {
        let pid = libc::pid_t::try_from(self.child.id()).expect("pid");
        // SAFETY: `pid` is this test's own child, which has not been reaped.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.child.try_wait().expect("wait").is_none() {
            assert!(Instant::now() < deadline, "the collector did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::fs::read_to_string(&self.received).expect("the collector wrote what it received")
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A port nothing listens on yet.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port()
}

/// Two rows on `DAY`: unattended Claude Code with a failed call and a limit
/// stop, and Codex — which says nothing about attendance — with a call that
/// ran.
fn a_day_of_work(home: &Path) {
    let mut log = SegmentLog::open(home.join("log"), Retention::Full).expect("log");
    let source = |harness: &str, session: &str, entrypoint: &str| Source {
        harness: Harness::new(harness),
        harness_version: None,
        vendor_session_id: Some(session.into()),
        workspace: Some("/Users/someone/project".into()),
        entrypoint: Some(Entrypoint::new(entrypoint)),
        backfilled_from: None,
        profile: None,
    };
    let claude = [
        Event::TurnStarted {
            author: None,
            prompt: Body::kept("a prompt"),
        },
        Event::ToolUse {
            tool: "Bash".into(),
            tool_use_id: None,
            duration_ms: Some(40),
            input: Body::kept("cargo test"),
            outcome: ToolOutcome::Failed {
                error: Body::kept("an error"),
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
    let codex = [
        Event::TurnStarted {
            author: None,
            prompt: Body::kept("another prompt"),
        },
        Event::ToolUse {
            tool: "shell".into(),
            tool_use_id: None,
            duration_ms: Some(1200),
            input: Body::kept("ls"),
            outcome: ToolOutcome::Succeeded {
                output: Body::kept("files"),
            },
        },
    ];
    let conversations = [
        (
            "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e",
            source(Harness::CLAUDE_CODE, "sess-a", "sdk-cli"),
            &claude[..],
        ),
        (
            "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3f",
            source(Harness::CODEX, "sess-b", "cli"),
            &codex[..],
        ),
    ];
    for (hour, (bus_id, source, events)) in conversations.into_iter().enumerate() {
        for (second, event) in events.iter().enumerate() {
            log.append(Entry {
                bus_id: bus_id.parse().expect("id"),
                observed_at: Some(
                    format!("{DAY}T1{hour}:00:0{second}.000Z")
                        .parse()
                        .expect("ts"),
                ),
                source: source.clone(),
                vendor_turn_id: None,
                vendor_agent_id: None,
                hook_id: None,
                spooled_at: None,
                event: event.clone(),
            })
            .expect("append");
        }
    }
}

/// Every point the collector kept, by metric, harness, attendance and kind.
fn points(received: &str) -> Vec<(String, String, String, Option<String>, Value, Value)> {
    let mut found = Vec::new();
    for line in received.lines().filter(|line| !line.trim().is_empty()) {
        let request: Value = serde_json::from_str(line).expect("the file exporter writes JSON");
        for resource in request["resourceMetrics"]
            .as_array()
            .expect("resourceMetrics")
        {
            let attributes: Vec<(String, String)> = resource["resource"]["attributes"]
                .as_array()
                .expect("resource attributes")
                .iter()
                .map(|a| {
                    let value = a["value"]["stringValue"].as_str().expect("a string");
                    (a["key"].as_str().expect("key").into(), value.into())
                })
                .collect();
            assert_eq!(
                attributes,
                vec![
                    ("service.name".to_string(), "conbus".to_string()),
                    (
                        "service.version".to_string(),
                        env!("CARGO_PKG_VERSION").to_string()
                    ),
                ],
                "the resource says what sent it and nothing about the machine"
            );
            for scope in resource["scopeMetrics"].as_array().expect("scopeMetrics") {
                for metric in scope["metrics"].as_array().expect("metrics") {
                    let name = metric["name"].as_str().expect("name").to_string();
                    let (shape, body) = if metric.get("sum").is_some() {
                        ("sum", &metric["sum"])
                    } else {
                        ("gauge", &metric["gauge"])
                    };
                    for point in body["dataPoints"].as_array().expect("dataPoints") {
                        let attr = |key: &str| {
                            point["attributes"]
                                .as_array()
                                .expect("point attributes")
                                .iter()
                                .find(|a| a["key"] == key)
                                .and_then(|a| a["value"]["stringValue"].as_str())
                                .map(String::from)
                        };
                        let keys: Vec<&str> = point["attributes"]
                            .as_array()
                            .expect("point attributes")
                            .iter()
                            .map(|a| a["key"].as_str().expect("key"))
                            .collect();
                        assert!(
                            keys.iter()
                                .all(|key| ["harness", "attendance", "kind"].contains(key)),
                            "{name} carries {keys:?}"
                        );
                        found.push((
                            name.clone(),
                            attr("harness").expect("harness"),
                            attr("attendance").expect("attendance"),
                            attr("kind"),
                            serde_json::json!({
                                "shape": shape,
                                "unit": metric["unit"],
                                "temporality": body["aggregationTemporality"],
                                "monotonic": body["isMonotonic"],
                            }),
                            point.clone(),
                        ));
                    }
                }
            }
        }
    }
    found
}

#[test]
#[ignore = "needs a real collector: set CONBUS_OTELCOL and run with --ignored"]
fn a_real_collector_keeps_every_point_as_shareable_printed_it() {
    let binary = std::env::var_os("CONBUS_OTELCOL")
        .map(PathBuf::from)
        .expect("set CONBUS_OTELCOL to an OpenTelemetry Collector binary");
    let home = tempfile::tempdir().expect("tempdir");
    a_day_of_work(home.path());
    let collector = Collector::start(&binary, home.path());

    let output = Command::new(env!("CARGO_BIN_EXE_conbus"))
        .args(["usage", "--json", "--shareable", "--day", DAY, "--otlp"])
        .arg(format!("http://127.0.0.1:{}", collector.port))
        .env("CONBUS_HOME", home.path())
        .output()
        .expect("run conbus usage");
    let received = collector.stop();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("(200 OK)"), "{stderr}");
    let rows: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("a row"))
        .collect();
    assert_eq!(rows.len(), 2, "one row per harness: {rows:?}");

    let mut kept = points(&received);
    assert_eq!(kept.len(), rows.len() * METRICS.len(), "{received}");
    for row in &rows {
        for (field, name, sum, unit, kind) in METRICS {
            let harness = row["harness"].as_str().expect("harness");
            let attendance = row["attendance"].as_str().expect("attendance");
            let at = kept
                .iter()
                .position(|(n, h, a, k, _, _)| {
                    n == name && h == harness && a == attendance && k.as_deref() == kind
                })
                .unwrap_or_else(|| panic!("no {name} {kind:?} for {harness}/{attendance}"));
            let (_, _, _, _, shape, point) = kept.swap_remove(at);
            let what = format!("{field} → {name} for {harness}/{attendance}");
            assert_eq!(
                point["asInt"].as_str().and_then(|v| v.parse::<u64>().ok()),
                row[field].as_u64(),
                "{what}"
            );
            assert_eq!(shape["unit"], unit, "{what}");
            assert_eq!(
                point["timeUnixNano"].as_str(),
                Some(DAY_END.to_string().as_str()),
                "{what}"
            );
            if sum {
                assert_eq!(shape["shape"], "sum", "{what}");
                assert_eq!(shape["temporality"], 1, "{what}: DELTA");
                assert_eq!(shape["monotonic"], true, "{what}");
                assert_eq!(
                    point["startTimeUnixNano"].as_str(),
                    Some(DAY_START.to_string().as_str()),
                    "{what}"
                );
            } else {
                assert_eq!(shape["shape"], "gauge", "{what}");
            }
        }
    }
    assert!(kept.is_empty(), "points no row accounts for: {kept:?}");
}
