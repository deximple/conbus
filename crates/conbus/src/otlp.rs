//! `conbus usage --otlp URL`: the pipe.
//!
//! The rows `--shareable` prints, and nothing else, sent as OpenTelemetry
//! metrics to a collector on this machine — the OpenTelemetry Collector,
//! Grafana Alloy and the Datadog Agent all take OTLP/HTTP on `127.0.0.1:4318`
//! — which forwards them to whatever the team already runs. That collector is
//! the person's own, not a conbus service: there is no device id, member or
//! credential here, and what reaches the wire is still bound by
//! `docs/rfd/0002-metadata-sync.md`, whose "The pipe" section is this module.
//!
//! A spike, scoped like one: one HTTP/1.1 `POST` over a `TcpStream` to a
//! loopback address, and no retry. TLS and every host beyond this machine are
//! the collector's job, so `https://` and other hosts are refused before a
//! connection is made, and no HTTP or TLS crate is added for them.
//!
//! The protocol is OTLP 1.11.0, <https://opentelemetry.io/docs/specs/otlp/>:
//! `POST /v1/metrics` with `Content-Type: application/json`; `200` is
//! success, and a `200` whose body carries `partialSuccess` means some points
//! were rejected.

mod metrics;
mod post;
mod proto;

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use conbus_core::clock::Timestamp;
use conbus_core::usage::SharedRow;

/// The URL every refusal offers instead: where the OpenTelemetry Collector,
/// Grafana Alloy and the Datadog Agent take OTLP/HTTP by default.
const EXAMPLE: &str = "http://127.0.0.1:4318";

/// OTLP/HTTP's path for metrics, appended to a base URL that does not end in
/// it already — what an exporter does with `OTEL_EXPORTER_OTLP_ENDPOINT`.
const METRICS_PATH: &str = "/v1/metrics";

/// A URL without a port means HTTP's, here as anywhere.
const HTTP_PORT: u16 = 80;

/// Where the pipe sends: a collector on this machine, spoken to in plain HTTP.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Endpoint {
    /// Tried in order: one for an address, two for `localhost`.
    addrs: Vec<SocketAddr>,
    /// The URL's authority as written, which is what `Host` carries.
    authority: String,
    /// The request target: the URL's path, ending in `/v1/metrics`.
    path: String,
}

impl Endpoint {
    /// Read `--otlp`'s URL, or say why it is not one this spike speaks to.
    ///
    /// Only `http://` to a loopback address: `127.0.0.0/8`, `[::1]`, or
    /// `localhost`, which is taken to mean those two without asking a
    /// resolver. No connection is made here, so a refusal never reaches the
    /// network.
    pub fn parse(url: &str) -> io::Result<Self> {
        if url.contains('@') {
            // Checked first, anywhere in the URL, and never repeated back: the
            // part before an `@` may be a password, and one with a `/` in it
            // would otherwise read as a path.
            return Err(refused(format!(
                "--otlp takes no user name or password in the URL, and no `@` anywhere \
                 in it: it sends to a collector on this machine, such as {EXAMPLE}, and \
                 that collector holds whatever credential the backend needs"
            )));
        }
        let (scheme, rest) = url.split_once("://").ok_or_else(|| malformed(url))?;
        if scheme.eq_ignore_ascii_case("https") {
            return Err(refused(format!(
                "--otlp speaks plain HTTP to a collector on this machine, such as {EXAMPLE}, \
                 and leaves TLS onward to that collector; got {url:?}"
            )));
        }
        if !scheme.eq_ignore_ascii_case("http") {
            return Err(malformed(url));
        }
        let (authority, target) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
        if target.contains(['?', '#']) {
            return Err(refused(format!(
                "--otlp wants a collector's URL without a query or fragment, such as \
                 {EXAMPLE}; got {url:?}"
            )));
        }
        let (host, port) = split_authority(authority).ok_or_else(|| malformed(url))?;
        let addrs = loopback(host, port).ok_or_else(|| {
            refused(format!(
                "--otlp sends only to a collector on this machine (127.0.0.1, [::1] or \
                 localhost), which forwards onward; got {url:?}"
            ))
        })?;
        Ok(Endpoint {
            addrs,
            authority: authority.to_string(),
            path: metrics_path(target).ok_or_else(|| malformed(url))?,
        })
    }

    /// The URL the request goes to, as a person would write it.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.authority, self.path)
    }
}

/// `host[:port]` or `[v6][:port]`, the port defaulting to HTTP's. The host
/// comes back with a `[` still on it when it was bracketed, so `loopback`
/// knows to read it as IPv6 and nothing else.
fn split_authority(authority: &str) -> Option<(&str, u16)> {
    let (host, port) = match authority.strip_prefix('[') {
        Some(inner) => {
            let (v6, after) = inner.split_once(']')?;
            (&authority[..v6.len() + 1], after)
        }
        None => match authority.find(':') {
            Some(colon) => authority.split_at(colon),
            None => (authority, ""),
        },
    };
    let port = match port.strip_prefix(':') {
        None if port.is_empty() => HTTP_PORT,
        None => return None,
        Some(digits) => digits.parse::<u16>().ok().filter(|port| *port != 0)?,
    };
    (!host.is_empty()).then_some((host, port))
}

/// The addresses a host names, when every one of them is this machine.
fn loopback(host: &str, port: u16) -> Option<Vec<SocketAddr>> {
    let ips: Vec<IpAddr> = if host.eq_ignore_ascii_case("localhost") {
        vec![Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()]
    } else if let Some(v6) = host.strip_prefix('[') {
        vec![v6.parse::<Ipv6Addr>().ok()?.into()]
    } else {
        vec![host.parse::<Ipv4Addr>().ok()?.into()]
    };
    ips.iter().all(IpAddr::is_loopback).then(|| {
        ips.into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect()
    })
}

/// The request target: the URL's path with `/v1/metrics` appended unless it
/// already ends in it. None when the path is not one a request line can carry
/// as it is.
fn metrics_path(path: &str) -> Option<String> {
    if !path.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let base = path.trim_end_matches('/');
    Some(if base.ends_with(METRICS_PATH) {
        base.to_string()
    } else {
        format!("{base}{METRICS_PATH}")
    })
}

fn malformed(url: &str) -> io::Error {
    refused(format!(
        "--otlp wants a collector's URL such as {EXAMPLE}, got {url:?}"
    ))
}

fn refused(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Send the rows, and say in a line or two what was sent.
///
/// No rows is no request: there would be no harness to label a zero with.
/// Nothing is retried, because a delta the collector took without saying so
/// would be counted twice; whoever runs this sees the error and decides.
pub fn export(endpoint: &Endpoint, rows: &[SharedRow], now: Timestamp) -> io::Result<Vec<String>> {
    if rows.is_empty() {
        return Ok(vec!["otlp: nothing sent: no rows in this window".into()]);
    }
    let payload = metrics::encode(rows, crate::build_info::VERSION, now)?;
    let answer = post::post(endpoint, &payload.body, &post::BOUNDS)?;
    if let Some((rejected, said)) = answer.rejected {
        let said = if said.is_empty() {
            String::new()
        } else {
            format!(": {said}")
        };
        return Err(io::Error::other(format!(
            "the collector at {} took the request but rejected {rejected} of {} data point(s){said}",
            endpoint.url(),
            payload.points
        )));
    }
    let status = format!("{} {}", answer.status, answer.reason);
    let sent = format!(
        "otlp: sent {} row(s) as {} data point(s) to {} ({})",
        payload.rows,
        payload.points,
        endpoint.url(),
        status.trim_end()
    );
    let growing = (!payload.open_days.is_empty()).then(|| {
        format!(
            "otlp: {} (UTC) has not ended, so it was counted up to now and will grow; \
             send each day once, after it ends (--day)",
            payload.open_days.join(", ")
        )
    });
    Ok(std::iter::once(sent).chain(growing).collect())
}

#[cfg(test)]
mod tests {
    use super::post::fake::{Collector, answer};
    use super::*;
    use conbus_core::event::{Body, Entrypoint, Event, Harness, Source, ToolOutcome, TurnState};
    use conbus_core::record::{Record, SCHEMA_VERSION};
    use conbus_core::usage::{Attendance, Counts, Ledger};
    use serde_json::Value;

    const OK: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";

    fn later() -> Timestamp {
        "2026-09-24T10:30:00.000Z".parse().expect("ts")
    }

    fn row(day: &str, harness: &str, turns: u64) -> SharedRow {
        SharedRow {
            day: day.into(),
            harness: harness.into(),
            attendance: Attendance::Unattended,
            conversations: 1,
            counts: Counts {
                turns,
                tool_failures: 2,
                ..Counts::default()
            },
        }
    }

    fn path_of(url: &str) -> String {
        Endpoint::parse(url).expect(url).path
    }

    #[test]
    fn a_collectors_url_is_read_the_way_otlp_exporters_read_it() {
        // A base URL gets the signal's path appended; a URL that already
        // names it is used as it is (the OTLP exporter configuration, for
        // OTEL_EXPORTER_OTLP_ENDPOINT and ..._METRICS_ENDPOINT).
        assert_eq!(path_of("http://127.0.0.1:4318"), "/v1/metrics");
        assert_eq!(path_of("http://127.0.0.1:4318/"), "/v1/metrics");
        assert_eq!(path_of("http://localhost:4318/v1/metrics"), "/v1/metrics");
        assert_eq!(path_of("http://localhost:4318/v1/metrics/"), "/v1/metrics");
        assert_eq!(path_of("http://127.0.0.1:4318/otlp"), "/otlp/v1/metrics");
        assert_eq!(path_of("http://127.0.0.1:4318/otlp/"), "/otlp/v1/metrics");

        let local = Endpoint::parse("http://127.0.0.1:4318").expect("local");
        assert_eq!(local.url(), "http://127.0.0.1:4318/v1/metrics");
        assert_eq!(local.authority, "127.0.0.1:4318");
        assert_eq!(local.addrs, vec!["127.0.0.1:4318".parse().expect("addr")]);

        let named = Endpoint::parse("HTTP://LocalHost:4318").expect("localhost");
        assert_eq!(
            named.addrs,
            vec![
                "127.0.0.1:4318".parse().expect("v4"),
                "[::1]:4318".parse().expect("v6")
            ],
            "localhost is this machine without asking a resolver"
        );
        let six = Endpoint::parse("http://[::1]:4318").expect("v6");
        assert_eq!(six.url(), "http://[::1]:4318/v1/metrics");
        let bare = Endpoint::parse("http://127.0.0.1").expect("no port");
        assert_eq!(
            bare.addrs,
            vec!["127.0.0.1:80".parse().expect("addr")],
            "a URL without a port means HTTP's, as it would anywhere else"
        );
    }

    #[test]
    fn https_is_refused_before_any_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();

        for url in [
            format!("https://127.0.0.1:{port}"),
            format!("HTTPS://localhost:{port}/v1/metrics"),
            "https://otlp.example.com/v1/metrics".to_string(),
        ] {
            let err = Endpoint::parse(&url).expect_err(&url);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{url}");
            let text = err.to_string();
            assert!(text.contains("plain HTTP"), "{text}");
            assert!(text.contains("collector on this machine"), "{text}");
        }
        let nobody = listener
            .accept()
            .map(|_| ())
            .expect_err("nothing connected");
        assert_eq!(nobody.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn a_url_that_is_not_a_local_collector_is_refused_before_any_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();

        let malformed = [
            String::new(),
            format!("127.0.0.1:{port}"),
            format!("grpc://127.0.0.1:{port}"),
            "http://".to_string(),
            format!("http://:{port}"),
            "http://127.0.0.1:0".to_string(),
            "http://127.0.0.1:65536".to_string(),
            "http://127.0.0.1:43a8".to_string(),
            "http://127.0.0.1:".to_string(),
            "http://[::1".to_string(),
            format!("http://127.0.0.1:{port}/v1 metrics"),
            format!("http://127.0.0.1:{port}/v1/metrics?tenant=a"),
            format!("http://127.0.0.1:{port}#metrics"),
        ];
        for url in &malformed {
            let err = Endpoint::parse(url).expect_err(url);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{url}");
            assert!(err.to_string().contains("http://127.0.0.1:4318"), "{err}");
        }
        for url in [
            "http://collector.example.com:4318",
            "http://10.0.0.5:4318",
            "http://0.0.0.0:4318",
            "http://[2001:db8::1]:4318",
        ] {
            let err = Endpoint::parse(url).expect_err(url);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{url}");
            assert!(err.to_string().contains("on this machine"), "{err}");
        }
        let nobody = listener
            .accept()
            .map(|_| ())
            .expect_err("nothing connected");
        assert_eq!(nobody.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn a_password_in_the_url_is_refused_without_being_repeated() {
        let err = Endpoint::parse("http://me:hunter2@127.0.0.1:4318").expect_err("userinfo");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let text = err.to_string();
        assert!(text.contains("no user name or password"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
        // Refused wherever it is, since a password with a `/` in it would
        // otherwise be read as a path and repeated back; the message says so.
        let path = Endpoint::parse("http://127.0.0.1:4318/team@a").expect_err("an @ in a path");
        assert!(path.to_string().contains("no `@` anywhere"), "{path}");
        assert!(!path.to_string().contains("team"), "{path}");
    }

    #[test]
    fn export_posts_the_shareable_rows_to_the_collector_as_otlp_json() {
        let collector = Collector::answering(OK);
        let rows = [
            row("2026-09-22", "claude_code", 3),
            row("2026-09-22", "codex", 1),
        ];

        let said = export(&collector.endpoint, &rows, later()).expect("sent");

        let port = collector.endpoint.addrs[0].port();
        let request = collector.request();
        let end = request
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a head");
        let head = String::from_utf8(request[..end].to_vec()).expect("utf8");
        let mut lines = head.split("\r\n");
        assert_eq!(lines.next(), Some("POST /v1/metrics HTTP/1.1"), "{head}");
        let headers: Vec<String> = lines.map(str::to_ascii_lowercase).collect();
        assert!(
            headers.contains(&"content-type: application/json".to_string()),
            "{head}"
        );
        assert!(
            headers.contains(&format!("host: 127.0.0.1:{port}")),
            "{head}"
        );
        let body = &request[end + 4..];
        assert!(
            headers.contains(&format!("content-length: {}", body.len())),
            "{head}"
        );
        let sent: serde_json::Value = serde_json::from_slice(body).expect("a JSON body");
        let expected: serde_json::Value = serde_json::from_slice(
            &metrics::encode(&rows, crate::build_info::VERSION, later())
                .expect("encode")
                .body,
        )
        .expect("json");
        assert_eq!(sent, expected);
        let turns = sent["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][1].clone();
        assert_eq!(turns["name"], "conbus.turns");
        assert_eq!(turns["sum"]["dataPoints"][0]["asInt"], "3");
        assert_eq!(turns["sum"]["dataPoints"][1]["asInt"], "1");

        assert_eq!(
            said,
            vec![format!(
                "otlp: sent 2 row(s) as 40 data point(s) to http://127.0.0.1:{port}/v1/metrics (200 OK)"
            )]
        );
    }

    #[test]
    fn a_day_that_has_not_ended_is_sent_and_said_to_be_growing() {
        let collector = Collector::answering(OK);

        let said = export(
            &collector.endpoint,
            &[row("2026-09-24", "claude_code", 1)],
            later(),
        )
        .expect("sent");

        collector.request();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[1].contains("2026-09-24"), "{said:?}");
        assert!(said[1].contains("--day"), "{said:?}");
    }

    #[test]
    fn an_empty_window_sends_nothing() {
        // No rows is not a request: there is no harness to label a zero with.
        let endpoint = Endpoint::parse("http://127.0.0.1:9").expect("endpoint");
        let said = export(&endpoint, &[], later()).expect("nothing to send");
        assert_eq!(said, vec!["otlp: nothing sent: no rows in this window"]);
    }

    #[test]
    fn a_partial_success_is_an_error_that_counts_what_was_rejected() {
        // OTLP answers 200 when it keeps some points and drops others, and
        // says how many in `partialSuccess`. "Sent" would be a lie.
        let collector = Collector::answering(&answer(
            "200 OK",
            r#"{"partialSuccess":{"rejectedDataPoints":"20","errorMessage":"points older than an hour are dropped"}}"#,
        ));

        let err = export(
            &collector.endpoint,
            &[row("2026-09-22", "claude_code", 1)],
            later(),
        )
        .expect_err("rejected points are not a success");

        collector.request();
        let text = err.to_string();
        assert!(text.contains("rejected 20 of 20 data point(s)"), "{text}");
        assert!(text.contains("older than an hour"), "{text}");
    }

    const BUS: &str = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e";

    fn record(ts: &str, source: Source, event: Event) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 1,
            ts: ts.parse().expect("ts"),
            bus_id: BUS.parse().expect("id"),
            vendor_turn_id: None,
            vendor_agent_id: None,
            source,
            event,
        }
    }

    /// The value the collector was sent for one metric and `kind` of it.
    fn sent(body: &Value, name: &str, kind: Option<&str>) -> Option<String> {
        body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()?
            .iter()
            .filter(|metric| metric["name"] == name)
            .filter_map(|metric| metric.get("sum").or_else(|| metric.get("gauge")))
            .filter_map(|data| data["dataPoints"].as_array())
            .flatten()
            .find(|point| {
                let kind_of = point["attributes"].as_array().and_then(|kvs| {
                    kvs.iter()
                        .find(|kv| kv["key"] == "kind")
                        .and_then(|kv| kv["value"]["stringValue"].as_str())
                });
                kind_of == kind
            })
            .and_then(|point| point["asInt"].as_str().map(str::to_string))
    }

    #[test]
    fn nothing_but_counts_reaches_the_collector() {
        // RFD 0002's never-sent list, with a canary for each kind of thing a
        // log holds: a prompt, a reply, a tool, its input and its error, a
        // stop reason, a folder, a raw entrypoint, a harness version, a
        // session id and a bus id — looked for in the whole request, head and
        // body, as the collector received it.
        let source = || Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: Some("9.9.9-canary".into()),
            vendor_session_id: Some("sess-canary".into()),
            workspace: Some("/Users/someone/secret-project".into()),
            entrypoint: Some(Entrypoint::new("sdk-cli")),
            backfilled_from: None,
        };
        let ledger = Ledger::fold(
            &[
                record(
                    "2026-09-22T09:00:00.000Z",
                    source(),
                    Event::TurnStarted {
                        author: None,
                        prompt: Body::kept("the secret plan"),
                    },
                ),
                record(
                    "2026-09-22T09:00:01.000Z",
                    source(),
                    Event::ToolUse {
                        tool: "Bash".into(),
                        tool_use_id: None,
                        duration_ms: Some(1_500),
                        input: Body::kept("cargo test"),
                        outcome: ToolOutcome::Failed {
                            error: Body::kept("password=hunter2"),
                            interrupted: false,
                        },
                    },
                ),
                record(
                    "2026-09-22T09:00:02.000Z",
                    source(),
                    Event::TurnEnded {
                        state: TurnState::InterruptedQuota {
                            reason: "rate_limit: canary".into(),
                        },
                        assistant_message: Some(Body::kept("here is the plan")),
                    },
                ),
            ],
            "2026-09-22",
        );
        let collector = Collector::answering(OK);

        export(&collector.endpoint, &ledger.shareable(), later()).expect("sent");

        let request = String::from_utf8(collector.request()).expect("utf8");
        for leaked in [
            "secret plan",
            "here is the plan",
            "Bash",
            "cargo test",
            "hunter2",
            "rate_limit",
            "canary",
            "secret-project",
            "/Users",
            "workspace",
            "sdk-cli",
            BUS,
        ] {
            assert!(
                !request.contains(leaked),
                "{leaked} reached the collector: {request}"
            );
        }
        let (_, body) = request.split_once("\r\n\r\n").expect("a head");
        let body: Value = serde_json::from_str(body).expect("json");
        assert_eq!(sent(&body, "conbus.turns", None).as_deref(), Some("1"));
        assert_eq!(
            sent(&body, "conbus.stops", Some("limit")).as_deref(),
            Some("1")
        );
        assert_eq!(
            sent(&body, "conbus.tool.failures", None).as_deref(),
            Some("1")
        );
        assert_eq!(
            sent(&body, "conbus.tool.duration", None).as_deref(),
            Some("1500")
        );
        let turns = &body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][1];
        assert_eq!(turns["name"], "conbus.turns");
        assert_eq!(
            turns["sum"]["dataPoints"][0]["attributes"][1]["value"]["stringValue"], "unattended",
            "the class, never the entrypoint it came from"
        );
    }
}
