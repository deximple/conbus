//! The mapping: a shareable row as OTLP metrics, and nothing else of it.
//!
//! Every count in a row is one data point of the metric it is named as below.
//! The row's day is the point's time; its harness and attendance are the
//! point's attributes, and `conbus.stops` adds `kind` — what an operator would
//! do about the stop. The same table is in `docs/rfd/0002-metadata-sync.md`.
//!
//! | Row field | Metric | Type | Unit |
//! | --- | --- | --- | --- |
//! | `conversations` | `conbus.conversations` | gauge | `{conversation}` |
//! | `turns` | `conbus.turns` | sum | `{turn}` |
//! | `limit_stops` … `user_stops` | `conbus.stops`, `kind` = `limit` `account` `vendor` `request` `other` `person` | sum | `{stop}` |
//! | `limit_episodes` | `conbus.limit_episodes` | sum | `{episode}` |
//! | `subagents` | `conbus.subagents` | sum | `{run}` |
//! | `tool_calls` | `conbus.tool.calls` | sum | `{call}` |
//! | `tool_failures` | `conbus.tool.failures` | sum | `{call}` |
//! | `tool_interrupts` | `conbus.tool.interrupts` | sum | `{call}` |
//! | `tool_denials` | `conbus.tool.denials` | sum | `{call}` |
//! | `permission_asks` | `conbus.permission_asks` | sum | `{ask}` |
//! | `tool_ms` | `conbus.tool.duration` | sum | `ms` |
//! | `briefings` | `conbus.briefings` | sum | `{briefing}` |
//! | `hops` | `conbus.hops` | sum | `{hop}` |
//! | `repeated` | `conbus.repeated` | sum | `{record}` |
//! | `recovered` | `conbus.recovered` | sum | `{record}` |
//!
//! Sums are monotonic, with delta temporality: a point is what happened
//! between 00:00 UTC of its day (`startTimeUnixNano`) and the next 00:00
//! (`timeUnixNano`), so the next day's point starts where this one ends, as
//! the data model asks of deltas — and so a day sent twice is counted twice.
//! A day that has not ended is counted up to now rather than stamped with a
//! time that has not come. `conversations` is a gauge because a conversation
//! that runs past midnight is in two days' rows; a gauge point carries only
//! its time, since `metrics.proto` says a gauge's start "is ignored for all
//! data points". Units follow the semantic conventions: a count is the thing
//! counted, in braces, and a duration is `ms`.
//!
//! The encoding is OTLP 1.11.0's "JSON Protobuf Encoding": keys are the proto
//! field names in lowerCamelCase, enum values are integers, and 64-bit
//! integers — the `fixed64` timestamps and the `sfixed64` `asInt` — are
//! decimal strings. The resource says which program sent the points,
//! `service.name` and `service.version`, and nothing that names the machine
//! or the person.

use std::io;

use conbus_core::clock::Timestamp;
use conbus_core::usage::{Attendance, Counts, SharedRow};
use serde::Serialize;

/// `AGGREGATION_TEMPORALITY_DELTA` in `metrics.proto`: each point is what
/// changed in its own interval.
const DELTA: u8 = 1;

const DAY_SECONDS: i64 = 24 * 60 * 60;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// One metric the pipe sends.
struct Instrument {
    name: &'static str,
    description: &'static str,
    unit: &'static str,
    /// Does not add up across days, so it is sent as a gauge.
    gauge: bool,
}

const fn sum(name: &'static str, description: &'static str, unit: &'static str) -> Instrument {
    Instrument {
        name,
        description,
        unit,
        gauge: false,
    }
}

static CONVERSATIONS: Instrument = Instrument {
    name: "conbus.conversations",
    description: "Conversations with a record that day; not additive across days",
    unit: "{conversation}",
    gauge: true,
};
static TURNS: Instrument = sum(
    "conbus.turns",
    "Prompts submitted to a main thread",
    "{turn}",
);
static STOPS: Instrument = sum(
    "conbus.stops",
    "Turns stopped early, by what an operator would do about it",
    "{stop}",
);
static LIMIT_EPISODES: Instrument = sum(
    "conbus.limit_episodes",
    "Runs of limit stops: a new one after an hour without any",
    "{episode}",
);
static SUBAGENTS: Instrument = sum("conbus.subagents", "Subagent runs that finished", "{run}");
static TOOL_CALLS: Instrument = sum("conbus.tool.calls", "Tool calls that ran", "{call}");
static TOOL_FAILURES: Instrument = sum(
    "conbus.tool.failures",
    "Tool calls that ran and failed",
    "{call}",
);
static TOOL_INTERRUPTS: Instrument = sum(
    "conbus.tool.interrupts",
    "Tool calls a person stopped",
    "{call}",
);
static TOOL_DENIALS: Instrument = sum(
    "conbus.tool.denials",
    "Tool calls the harness refused to run",
    "{call}",
);
static PERMISSION_ASKS: Instrument = sum(
    "conbus.permission_asks",
    "Times a person was asked whether a tool may run",
    "{ask}",
);
static TOOL_DURATION: Instrument = sum(
    "conbus.tool.duration",
    "Summed tool durations the vendor reported",
    "ms",
);
static BRIEFINGS: Instrument = sum(
    "conbus.briefings",
    "Briefings handed to a session",
    "{briefing}",
);
static HOPS: Instrument = sum(
    "conbus.hops",
    "Routed records, whoever decided them",
    "{hop}",
);
static REPEATED: Instrument = sum(
    "conbus.repeated",
    "Second deliveries of a report already counted",
    "{record}",
);
static RECOVERED: Instrument = sum(
    "conbus.recovered",
    "Records recovered by gap fill or backfill rather than seen by a hook",
    "{record}",
);

/// Every count of one row as (metric, `kind` of stop, value), in the order the
/// metrics are sent.
///
/// Destructured field by field with no `..`, the way `Counts::add` is: a count
/// added to the row stops this compiling until someone decides what it is sent
/// as — or that it is not sent — which is an amendment to RFD 0002 either way.
fn values(row: &SharedRow) -> [(&'static Instrument, Option<&'static str>, u64); 20] {
    let SharedRow {
        day: _,
        harness: _,
        attendance: _,
        conversations,
        counts,
    } = row;
    let Counts {
        turns,
        limit_stops,
        limit_episodes,
        account_stops,
        vendor_errors,
        request_faults,
        other_stops,
        user_stops,
        subagents,
        tool_calls,
        tool_failures,
        tool_interrupts,
        tool_denials,
        permission_asks,
        tool_ms,
        briefings,
        hops,
        repeated,
        recovered,
    } = counts;
    [
        (&CONVERSATIONS, None, *conversations),
        (&TURNS, None, *turns),
        (&STOPS, Some("limit"), *limit_stops),
        (&STOPS, Some("account"), *account_stops),
        (&STOPS, Some("vendor"), *vendor_errors),
        (&STOPS, Some("request"), *request_faults),
        (&STOPS, Some("other"), *other_stops),
        (&STOPS, Some("person"), *user_stops),
        (&LIMIT_EPISODES, None, *limit_episodes),
        (&SUBAGENTS, None, *subagents),
        (&TOOL_CALLS, None, *tool_calls),
        (&TOOL_FAILURES, None, *tool_failures),
        (&TOOL_INTERRUPTS, None, *tool_interrupts),
        (&TOOL_DENIALS, None, *tool_denials),
        (&PERMISSION_ASKS, None, *permission_asks),
        (&TOOL_DURATION, None, *tool_ms),
        (&BRIEFINGS, None, *briefings),
        (&HOPS, None, *hops),
        (&REPEATED, None, *repeated),
        (&RECOVERED, None, *recovered),
    ]
}

/// Attendance as `--shareable` spells it — pinned by a test against its serde
/// name, and exhaustive, so a new class is a decision here too.
fn attendance(attendance: Attendance) -> &'static str {
    match attendance {
        Attendance::Attended => "attended",
        Attendance::Unattended => "unattended",
        Attendance::Unknown => "unknown",
    }
}

/// One export: the body, and what it holds.
#[derive(Debug)]
pub struct Payload {
    pub body: Vec<u8>,
    pub rows: usize,
    pub points: usize,
    /// Days whose points are counted up to now, because the day had not ended.
    pub open_days: Vec<String>,
}

/// The rows as one `ExportMetricsServiceRequest`, in OTLP's JSON encoding.
pub fn encode(rows: &[SharedRow], version: &str, now: Timestamp) -> io::Result<Payload> {
    let now = unix_seconds(now)?;
    let mut metrics: Vec<Metric> = Vec::new();
    let mut open_days: Vec<String> = Vec::new();
    let mut points = 0;
    for row in rows {
        let span = Span::of(&row.day, now)?;
        if span.open && !open_days.contains(&row.day) {
            open_days.push(row.day.clone());
        }
        for (instrument, kind, value) in values(row) {
            let point = span.point(instrument, attributes(row, kind), value)?;
            points += 1;
            match metrics
                .iter_mut()
                .find(|metric| metric.name == instrument.name)
            {
                Some(metric) => metric.data.points().push(point),
                None => metrics.push(Metric::new(instrument, point)),
            }
        }
    }
    let request = ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Resource {
                attributes: vec![
                    text("service.name", "conbus"),
                    text("service.version", version),
                ],
            },
            scope_metrics: vec![ScopeMetrics { metrics }],
        }],
    };
    Ok(Payload {
        body: serde_json::to_vec(&request).map_err(io::Error::other)?,
        rows: rows.len(),
        points,
        open_days,
    })
}

fn attributes(row: &SharedRow, kind: Option<&'static str>) -> Vec<KeyValue> {
    [
        Some(text("harness", &row.harness)),
        Some(text("attendance", attendance(row.attendance))),
        kind.map(|kind| text("kind", kind)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn text(key: &'static str, value: &str) -> KeyValue {
    KeyValue {
        key,
        value: AnyValue {
            string_value: value.to_string(),
        },
    }
}

/// The interval one row's points cover, written the way OTLP wants it.
struct Span {
    start: String,
    time: String,
    /// The day had not ended when the payload was built.
    open: bool,
}

impl Span {
    /// 00:00 UTC of `day` to the next 00:00 — or to now, when that has not
    /// come yet.
    fn of(day: &str, now: i64) -> io::Result<Self> {
        let start = day_start(day)?;
        let end = start + DAY_SECONDS;
        let open = now < end;
        let time = if open { now.max(start) } else { end };
        Ok(Span {
            start: nanos(start)?,
            time: nanos(time)?,
            open,
        })
    }

    fn point(
        &self,
        instrument: &Instrument,
        attributes: Vec<KeyValue>,
        value: u64,
    ) -> io::Result<NumberDataPoint> {
        Ok(NumberDataPoint {
            attributes,
            start_time_unix_nano: (!instrument.gauge).then(|| self.start.clone()),
            time_unix_nano: self.time.clone(),
            as_int: i64::try_from(value)
                .map(|value| value.to_string())
                .map_err(|_| invalid(format!("a count of {value} does not fit OTLP's int64")))?,
        })
    }
}

fn day_start(day: &str) -> io::Result<i64> {
    let start: Timestamp = format!("{day}T00:00:00.000Z")
        .parse()
        .map_err(|_| invalid(format!("a row's day is not a UTC day: {day:?}")))?;
    unix_seconds(start)
}

fn unix_seconds(ts: Timestamp) -> io::Result<i64> {
    Timestamp::from_unix_seconds(0)
        .map(|epoch| ts.seconds_since(epoch))
        .ok_or_else(|| io::Error::other("the unix epoch is not a timestamp"))
}

/// Unix seconds as OTLP's `fixed64` nanoseconds, a decimal string.
fn nanos(seconds: i64) -> io::Result<String> {
    u64::try_from(seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(NANOS_PER_SECOND))
        .map(|nanos| nanos.to_string())
        .ok_or_else(|| invalid(format!("{seconds}s after 1970 is not an OTLP timestamp")))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// The wire, one type per proto message it stands for, each field named as the
// proto names it; `rename_all` makes the lowerCamelCase that OTLP/JSON wants.

/// `opentelemetry.proto.collector.metrics.v1.ExportMetricsServiceRequest`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportMetricsServiceRequest {
    resource_metrics: Vec<ResourceMetrics>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceMetrics {
    resource: Resource,
    scope_metrics: Vec<ScopeMetrics>,
}

#[derive(Serialize)]
struct Resource {
    attributes: Vec<KeyValue>,
}

/// With no `scope`: which program sent the points is the resource's to say.
#[derive(Serialize)]
struct ScopeMetrics {
    metrics: Vec<Metric>,
}

#[derive(Serialize)]
struct Metric {
    name: &'static str,
    description: &'static str,
    unit: &'static str,
    /// The proto's `oneof data`: `"sum": {…}` or `"gauge": {…}`.
    #[serde(flatten)]
    data: Data,
}

impl Metric {
    fn new(instrument: &'static Instrument, first: NumberDataPoint) -> Self {
        let data_points = vec![first];
        Metric {
            name: instrument.name,
            description: instrument.description,
            unit: instrument.unit,
            data: if instrument.gauge {
                Data::Gauge(Gauge { data_points })
            } else {
                Data::Sum(Sum {
                    aggregation_temporality: DELTA,
                    is_monotonic: true,
                    data_points,
                })
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum Data {
    Sum(Sum),
    Gauge(Gauge),
}

impl Data {
    fn points(&mut self) -> &mut Vec<NumberDataPoint> {
        match self {
            Data::Sum(sum) => &mut sum.data_points,
            Data::Gauge(gauge) => &mut gauge.data_points,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sum {
    aggregation_temporality: u8,
    is_monotonic: bool,
    data_points: Vec<NumberDataPoint>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Gauge {
    data_points: Vec<NumberDataPoint>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NumberDataPoint {
    attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_time_unix_nano: Option<String>,
    time_unix_nano: String,
    as_int: String,
}

#[derive(Serialize)]
struct KeyValue {
    key: &'static str,
    value: AnyValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AnyValue {
    string_value: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::event::{Body, Entrypoint, Event, Harness, Source, ToolOutcome, TurnState};
    use conbus_core::record::{Record, SCHEMA_VERSION};
    use conbus_core::usage::{Counts, Ledger};
    use serde_json::Value;

    const VERSION: &str = "0.2.4";
    /// 2026-09-22T00:00:00Z and the next midnight, in nanoseconds.
    const MIDNIGHT_22: &str = "1790035200000000000";
    const MIDNIGHT_23: &str = "1790121600000000000";
    const MIDNIGHT_24: &str = "1790208000000000000";
    /// 2026-09-24T10:30:00Z.
    const HALF_TEN_24: &str = "1790245800000000000";

    fn later() -> Timestamp {
        "2026-09-24T10:30:00.000Z".parse().expect("ts")
    }

    fn body_of(rows: &[SharedRow], now: Timestamp) -> Value {
        let payload = encode(rows, VERSION, now).expect("encode");
        serde_json::from_slice(&payload.body).expect("the body is JSON")
    }

    fn metrics_of(body: &Value) -> &Vec<Value> {
        body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()
            .expect("metrics")
    }

    fn points_of(metric: &Value) -> &Vec<Value> {
        metric
            .get("sum")
            .or_else(|| metric.get("gauge"))
            .and_then(|data| data["dataPoints"].as_array())
            .expect("data points")
    }

    fn attribute(point: &Value, key: &str) -> Option<String> {
        point["attributes"]
            .as_array()?
            .iter()
            .find(|kv| kv["key"] == key)
            .and_then(|kv| kv["value"]["stringValue"].as_str())
            .map(str::to_string)
    }

    fn shared(day: &str, harness: &str, attendance: Attendance, counts: Counts) -> SharedRow {
        SharedRow {
            day: day.into(),
            harness: harness.into(),
            attendance,
            conversations: 1,
            counts,
        }
    }

    #[test]
    fn every_count_of_a_row_is_one_point_of_the_metric_it_is_named_as() {
        // Each field a different number, so a swapped pair cannot pass.
        let row = SharedRow {
            day: "2026-09-22".into(),
            harness: "claude_code".into(),
            attendance: Attendance::Unattended,
            conversations: 20,
            counts: Counts {
                turns: 1,
                limit_stops: 2,
                limit_episodes: 3,
                account_stops: 4,
                vendor_errors: 5,
                request_faults: 6,
                other_stops: 7,
                user_stops: 8,
                subagents: 9,
                tool_calls: 10,
                tool_failures: 11,
                tool_interrupts: 12,
                tool_denials: 13,
                permission_asks: 14,
                tool_ms: 15,
                briefings: 16,
                hops: 17,
                repeated: 18,
                recovered: 19,
            },
        };
        let body = body_of(&[row], later());

        let mut sent = Vec::new();
        for metric in metrics_of(&body) {
            for point in points_of(metric) {
                sent.push((
                    metric["name"].as_str().expect("name").to_string(),
                    attribute(point, "kind"),
                    point["asInt"].as_str().expect("asInt").to_string(),
                ));
            }
        }
        let expected: Vec<(String, Option<String>, String)> = [
            ("conbus.conversations", None, "20"),
            ("conbus.turns", None, "1"),
            ("conbus.stops", Some("limit"), "2"),
            ("conbus.stops", Some("account"), "4"),
            ("conbus.stops", Some("vendor"), "5"),
            ("conbus.stops", Some("request"), "6"),
            ("conbus.stops", Some("other"), "7"),
            ("conbus.stops", Some("person"), "8"),
            ("conbus.limit_episodes", None, "3"),
            ("conbus.subagents", None, "9"),
            ("conbus.tool.calls", None, "10"),
            ("conbus.tool.failures", None, "11"),
            ("conbus.tool.interrupts", None, "12"),
            ("conbus.tool.denials", None, "13"),
            ("conbus.permission_asks", None, "14"),
            ("conbus.tool.duration", None, "15"),
            ("conbus.briefings", None, "16"),
            ("conbus.hops", None, "17"),
            ("conbus.repeated", None, "18"),
            ("conbus.recovered", None, "19"),
        ]
        .into_iter()
        .map(|(name, kind, value)| {
            (
                name.to_string(),
                kind.map(str::to_string),
                value.to_string(),
            )
        })
        .collect();
        assert_eq!(sent, expected);

        let units: Vec<(&str, &str)> = metrics_of(&body)
            .iter()
            .map(|m| {
                (
                    m["name"].as_str().expect("name"),
                    m["unit"].as_str().expect("unit"),
                )
            })
            .collect();
        assert_eq!(
            units,
            vec![
                ("conbus.conversations", "{conversation}"),
                ("conbus.turns", "{turn}"),
                ("conbus.stops", "{stop}"),
                ("conbus.limit_episodes", "{episode}"),
                ("conbus.subagents", "{run}"),
                ("conbus.tool.calls", "{call}"),
                ("conbus.tool.failures", "{call}"),
                ("conbus.tool.interrupts", "{call}"),
                ("conbus.tool.denials", "{call}"),
                ("conbus.permission_asks", "{ask}"),
                ("conbus.tool.duration", "ms"),
                ("conbus.briefings", "{briefing}"),
                ("conbus.hops", "{hop}"),
                ("conbus.repeated", "{record}"),
                ("conbus.recovered", "{record}"),
            ]
        );
    }

    #[test]
    fn counts_are_monotonic_deltas_and_conversations_a_gauge() {
        // A conversation that runs past midnight is in two days' rows, so
        // conversations do not add up across days; everything else does.
        let body = body_of(
            &[shared(
                "2026-09-22",
                "codex",
                Attendance::Unknown,
                Counts::default(),
            )],
            later(),
        );

        for metric in metrics_of(&body) {
            let name = metric["name"].as_str().expect("name");
            if name == "conbus.conversations" {
                assert!(metric.get("sum").is_none(), "{metric}");
                assert!(metric["gauge"]["dataPoints"].is_array(), "{metric}");
                continue;
            }
            assert!(metric.get("gauge").is_none(), "{metric}");
            // Enums go as integers in OTLP/JSON, never by name: 1 is DELTA.
            assert_eq!(metric["sum"]["aggregationTemporality"], 1, "{name}");
            assert_eq!(metric["sum"]["isMonotonic"], true, "{name}");
            assert!(
                !metric["description"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_row_is_its_utc_day_from_midnight_to_midnight_with_its_harness_and_attendance() {
        let rows = [
            shared(
                "2026-09-22",
                "claude_code",
                Attendance::Attended,
                Counts::default(),
            ),
            shared(
                "2026-09-23",
                "cursor",
                Attendance::Unknown,
                Counts::default(),
            ),
        ];
        let payload = encode(&rows, VERSION, later()).expect("encode");
        assert!(payload.open_days.is_empty(), "{:?}", payload.open_days);
        assert_eq!((payload.rows, payload.points), (2, 40));
        let body: Value = serde_json::from_slice(&payload.body).expect("json");

        let metrics = metrics_of(&body);
        let turns = points_of(&metrics[1]);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["startTimeUnixNano"], MIDNIGHT_22);
        assert_eq!(turns[0]["timeUnixNano"], MIDNIGHT_23);
        assert_eq!(
            attribute(&turns[0], "harness").as_deref(),
            Some("claude_code")
        );
        assert_eq!(
            attribute(&turns[0], "attendance").as_deref(),
            Some("attended")
        );
        assert_eq!(turns[1]["startTimeUnixNano"], MIDNIGHT_23);
        assert_eq!(turns[1]["timeUnixNano"], MIDNIGHT_24);
        assert_eq!(attribute(&turns[1], "harness").as_deref(), Some("cursor"));
        assert_eq!(
            attribute(&turns[1], "attendance").as_deref(),
            Some("unknown")
        );
        // `metrics.proto`: a gauge's start "is ignored for all data points".
        let conversations = points_of(&metrics[0]);
        assert!(conversations[0].get("startTimeUnixNano").is_none());
        assert_eq!(conversations[0]["timeUnixNano"], MIDNIGHT_23);
        for metric in metrics {
            for point in points_of(metric) {
                let keys: Vec<String> = point["attributes"]
                    .as_array()
                    .expect("attributes")
                    .iter()
                    .map(|kv| kv["key"].as_str().expect("key").to_string())
                    .collect();
                let expected = if metric["name"] == "conbus.stops" {
                    vec!["harness", "attendance", "kind"]
                } else {
                    vec!["harness", "attendance"]
                };
                assert_eq!(keys, expected, "{metric}");
            }
        }
    }

    #[test]
    fn a_day_that_has_not_ended_is_counted_up_to_now_never_to_a_time_that_has_not_come() {
        let rows = [
            shared(
                "2026-09-23",
                "codex",
                Attendance::Unknown,
                Counts::default(),
            ),
            shared(
                "2026-09-24",
                "codex",
                Attendance::Unknown,
                Counts::default(),
            ),
        ];
        let payload = encode(&rows, VERSION, later()).expect("encode");
        assert_eq!(payload.open_days, vec!["2026-09-24".to_string()]);
        let body: Value = serde_json::from_slice(&payload.body).expect("json");

        let turns = points_of(&metrics_of(&body)[1]);
        assert_eq!(turns[0]["timeUnixNano"], MIDNIGHT_24, "yesterday is closed");
        assert_eq!(turns[1]["startTimeUnixNano"], MIDNIGHT_24);
        assert_eq!(turns[1]["timeUnixNano"], HALF_TEN_24, "today stops at now");
    }

    /// Every object key anywhere in `value`.
    fn keys(value: &Value, into: &mut Vec<String>) {
        match value {
            Value::Object(fields) => {
                for (key, inner) in fields {
                    into.push(key.clone());
                    keys(inner, into);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| keys(item, into)),
            _ => {}
        }
    }

    #[test]
    fn the_body_is_otlp_json_and_says_only_which_program_sent_it() {
        // OTLP 1.11.0, "JSON Protobuf Encoding": field names in lowerCamelCase,
        // 64-bit integers as decimal strings. The vocabulary below is every key
        // the pipe may use; a new one has to be named in RFD 0002 first.
        let body = body_of(
            &[shared(
                "2026-09-22",
                "grok",
                Attendance::Unknown,
                Counts::default(),
            )],
            later(),
        );

        let mut seen = Vec::new();
        keys(&body, &mut seen);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen,
            vec![
                "aggregationTemporality",
                "asInt",
                "attributes",
                "dataPoints",
                "description",
                "gauge",
                "isMonotonic",
                "key",
                "metrics",
                "name",
                "resource",
                "resourceMetrics",
                "scopeMetrics",
                "startTimeUnixNano",
                "stringValue",
                "sum",
                "timeUnixNano",
                "unit",
                "value",
            ]
        );
        assert_eq!(body["resourceMetrics"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            body["resourceMetrics"][0]["resource"],
            serde_json::json!({"attributes": [
                {"key": "service.name", "value": {"stringValue": "conbus"}},
                {"key": "service.version", "value": {"stringValue": VERSION}},
            ]}),
            "which program, and nothing that names the machine or the person"
        );
        for metric in metrics_of(&body) {
            for point in points_of(metric) {
                for field in ["asInt", "timeUnixNano"] {
                    let text = point[field].as_str().expect("a decimal string");
                    assert!(text.bytes().all(|b| b.is_ascii_digit()), "{field}: {text}");
                }
            }
        }
    }

    fn record(ts: &str, bus_id: &str, source: Source, event: Event) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 1,
            ts: ts.parse().expect("ts"),
            bus_id: bus_id.parse().expect("id"),
            vendor_turn_id: None,
            vendor_agent_id: None,
            source,
            event,
        }
    }

    #[test]
    fn nothing_but_counts_reaches_the_body() {
        // RFD 0002's never-sent list, with a canary for each kind of thing a
        // log holds: a prompt, a reply, a tool, its input and its error, a
        // stop reason, a folder, a raw entrypoint, a session id and a bus id.
        const BUS: &str = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e";
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
                    BUS,
                    source(),
                    Event::TurnStarted {
                        author: None,
                        prompt: Body::kept("the secret plan"),
                    },
                ),
                record(
                    "2026-09-22T09:00:01.000Z",
                    BUS,
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
                    BUS,
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
        let rows = ledger.shareable();
        let payload = encode(&rows, VERSION, later()).expect("encode");
        let text = String::from_utf8(payload.body.clone()).expect("utf8");

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
            assert!(!text.contains(leaked), "{leaked} reached the body: {text}");
        }
        let body: Value = serde_json::from_str(&text).expect("json");
        let metrics = metrics_of(&body);
        let value_of = |name: &str, kind: Option<&str>| {
            metrics
                .iter()
                .filter(|m| m["name"] == name)
                .flat_map(points_of)
                .find(|p| attribute(p, "kind").as_deref() == kind)
                .and_then(|p| p["asInt"].as_str().map(str::to_string))
        };
        assert_eq!(value_of("conbus.turns", None).as_deref(), Some("1"));
        assert_eq!(
            value_of("conbus.stops", Some("limit")).as_deref(),
            Some("1")
        );
        assert_eq!(value_of("conbus.tool.failures", None).as_deref(), Some("1"));
        assert_eq!(
            value_of("conbus.tool.duration", None).as_deref(),
            Some("1500")
        );
        assert_eq!(
            attribute(&points_of(&metrics[1])[0], "attendance").as_deref(),
            Some("unattended"),
            "the class, never the entrypoint it came from"
        );
    }

    #[test]
    fn attendance_is_spelled_the_way_shareable_prints_it() {
        for class in [
            Attendance::Attended,
            Attendance::Unattended,
            Attendance::Unknown,
        ] {
            assert_eq!(
                serde_json::to_value(class).expect("json"),
                Value::String(attendance(class).to_string())
            );
        }
    }
}
