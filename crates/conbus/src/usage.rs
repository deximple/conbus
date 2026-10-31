//! `conbus usage`: what the agents did, counted, from the log.
//!
//! Counts only. The fold is `conbus_core::usage`, which never holds a body;
//! this module prints it. Cost is named as absent rather than left out, because
//! a usage report that is silent about cost reads as one that has it.

use std::io;

use conbus_core::clock::{Timestamp, ago};
use conbus_core::paths::Paths;
use conbus_core::segment::SegmentLog;
use conbus_core::usage::{HarnessTotal, Ledger, SharedRow, first_day, next_day};

use crate::otlp;

/// How many names a top-N line prints.
const TOP: usize = 5;

const COST: &str = "cost      not in this log: no hook payload carries tokens or prices";

/// What `conbus usage` prints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Output {
    Text,
    /// One local row per line, folder included.
    Json,
    /// What a sync would send: RFD 0002, workspaces off.
    Shareable,
}

impl Output {
    /// `--shareable` wins: it is JSON too, with less in it.
    pub fn of(json: bool, shareable: bool) -> Self {
        match (json, shareable) {
            (_, true) => Output::Shareable,
            (true, false) => Output::Json,
            (false, false) => Output::Text,
        }
    }
}

/// Which records a report covers, and how its header names that.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Window {
    pub since: String,
    /// Exclusive; none means up to now.
    pub until: Option<String>,
    pub label: String,
}

/// `--day` wins, then `--since`, then the last `days` days.
pub fn window(
    days: u32,
    since: Option<&str>,
    day: Option<&str>,
    now: Timestamp,
) -> io::Result<Window> {
    if let Some(day) = day {
        check_since(day)?;
        let until = next_day(day).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("--day wants one UTC day like 2026-09-01, got {day:?}"),
            )
        })?;
        return Ok(Window {
            since: day.to_string(),
            until: Some(until),
            label: format!("on {day}"),
        });
    }
    if let Some(since) = since {
        check_since(since)?;
    }
    let since = since.map_or_else(|| first_day(now, days), str::to_string);
    Ok(Window {
        label: format!("from {since}"),
        since,
        until: None,
    })
}

/// Rows a sync may send. The log stays in this module; `sync` never opens it.
pub fn shareable_rows(paths: &Paths) -> io::Result<Vec<SharedRow>> {
    let scan = SegmentLog::scan(paths.log_dir())?;
    Ok(Ledger::fold(&scan.records, "").shareable())
}

/// Print the report; with a collector, send it the shareable rows after.
///
/// What is sent is printed first, whichever form was asked for, so the rows a
/// collector receives are rows their owner could read — `--shareable` prints
/// exactly them. What the send did goes to stderr, which keeps `--json` and
/// `--shareable` one row per line and nothing else.
pub fn run(
    paths: &Paths,
    window: Window,
    output: Output,
    collector: Option<&otlp::Endpoint>,
) -> io::Result<()> {
    let scan = SegmentLog::scan(paths.log_dir())?;
    let ledger = Ledger::fold_between(&scan.records, &window.since, window.until.as_deref());
    let lines = match output {
        Output::Text => render(&ledger, &window.label),
        Output::Json => render_json(&ledger)?,
        Output::Shareable => render_shareable(&ledger)?,
    };
    for line in lines {
        println!("{line}");
    }
    if !scan.defects.is_empty() {
        eprintln!(
            "unreadable: {} line(s) not counted (conbus log names them)",
            scan.defects.len()
        );
    }
    if let Some(collector) = collector {
        for line in otlp::export(collector, &ledger.shareable(), Timestamp::now())? {
            eprintln!("{line}");
        }
    }
    Ok(())
}

/// The window is compared as text, so a day that is not written the log's way
/// would select the wrong records without a word. Refused instead.
fn check_since(since: &str) -> io::Result<()> {
    let bytes = since.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    let number = |range: std::ops::Range<usize>| {
        since
            .get(range)
            .and_then(|part| part.parse::<u32>().ok())
            .unwrap_or(0)
    };
    let shaped = digits(0..4)
        && bytes.get(4) == Some(&b'-')
        && digits(5..7)
        && bytes.get(7) == Some(&b'-')
        && digits(8..10);
    if shaped && (1..=12).contains(&number(5..7)) && (1..=31).contains(&number(8..10)) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("--since wants a UTC day like 2026-09-01, got {since:?}"),
    ))
}

fn render(ledger: &Ledger, label: &str) -> Vec<String> {
    let recovered = if ledger.recovered == 0 {
        String::new()
    } else {
        format!(", {} recovered", ledger.recovered)
    };
    let mut lines = vec![format!(
        "usage     {label} (UTC) · {} record(s){recovered}{}",
        ledger.records,
        skipped(ledger)
    )];
    if ledger.harnesses.is_empty() {
        lines.push("          nothing recorded in this window".into());
    }
    lines.extend(ledger.harnesses.iter().flat_map(describe));
    if !ledger.failing_tools.is_empty() {
        lines.push(format!("failing   {}", top(&ledger.failing_tools)));
    }
    if !ledger.workspaces.is_empty() {
        lines.push(format!("busiest   {} (turns)", top(&ledger.workspaces)));
    }
    lines.push(COST.into());
    lines
}

/// One row per line. The rows are the whole of it: a reader that wants totals
/// sums them, and nothing here is a body.
fn render_json(ledger: &Ledger) -> io::Result<Vec<String>> {
    ledger
        .rows
        .iter()
        .map(|row| serde_json::to_string(row).map_err(io::Error::other))
        .collect()
}

/// The rows as RFD 0002 lets a sync send them. Printed here first so that
/// what leaves a machine is something its owner has already read.
fn render_shareable(ledger: &Ledger) -> io::Result<Vec<String>> {
    ledger
        .shareable()
        .iter()
        .map(|row| serde_json::to_string(row).map_err(io::Error::other))
        .collect()
}

fn describe(total: &HarnessTotal) -> Vec<String> {
    let c = &total.counts;
    vec![
        format!(
            "harness   {} — {} conversation(s): attended {} · unattended {} · unknown {}",
            total.harness, total.conversations, total.attended, total.unattended, total.unknown
        ),
        format!("          {} turn(s) · subagents {}", c.turns, c.subagents),
        format!(
            "          stopped: limit {} in {} episode(s) · account {} · vendor {} · request {} · other {} · by a person {}",
            c.limit_stops,
            c.limit_episodes,
            c.account_stops,
            c.vendor_errors,
            c.request_faults,
            c.other_stops,
            c.user_stops
        ),
        format!(
            "          {} tool call(s): failed {} · interrupted {} · refused {} · asks {} · {} in tools",
            c.tool_calls,
            c.tool_failures,
            c.tool_interrupts,
            c.tool_denials,
            c.permission_asks,
            ago((c.tool_ms / 1_000) as i64)
        ),
    ]
}

/// What the window left out, so a small number is not mistaken for a quiet
/// week.
fn skipped(ledger: &Ledger) -> String {
    let parts: Vec<String> = [
        ("earlier", ledger.before_window),
        ("later", ledger.after_window),
        ("repeated", ledger.repeated),
        ("recorded twice", ledger.recorded_twice),
        ("probe", ledger.probe),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .map(|(what, n)| format!("{what} {n}"))
    .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(" · not counted: {}", parts.join(" · "))
}

fn top(counts: &[(String, u64)]) -> String {
    counts
        .iter()
        .take(TOP)
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::event::{Body, Entrypoint, Event, Harness, Source, ToolOutcome, TurnState};
    use conbus_core::record::{Record, SCHEMA_VERSION};

    const ONE: &str = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e";
    const TWO: &str = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3f";

    fn record(
        ts: &str,
        bus_id: &str,
        harness: &str,
        entrypoint: Option<&str>,
        event: Event,
    ) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 1,
            ts: ts.parse().expect("ts"),
            bus_id: bus_id.parse().expect("id"),
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: Source {
                harness: Harness::new(harness),
                harness_version: None,
                vendor_session_id: None,
                workspace: Some("/w".into()),
                entrypoint: entrypoint.map(Entrypoint::new),
                backfilled_from: None,
            },
            event,
        }
    }

    fn week() -> Ledger {
        let claude = |ts, event| record(ts, ONE, Harness::CLAUDE_CODE, Some("sdk-cli"), event);
        Ledger::fold(
            &[
                claude(
                    "2026-09-22T09:00:00.000Z",
                    Event::TurnStarted {
                        author: None,
                        prompt: Body::kept("the secret plan"),
                    },
                ),
                claude(
                    "2026-09-22T09:00:01.000Z",
                    Event::ToolUse {
                        tool: "Bash".into(),
                        tool_use_id: None,
                        duration_ms: Some(7_200_000),
                        input: Body::kept("cargo test"),
                        outcome: ToolOutcome::Failed {
                            error: Body::kept("exit 101"),
                            interrupted: false,
                        },
                    },
                ),
                claude(
                    "2026-09-22T09:00:02.000Z",
                    Event::TurnEnded {
                        state: TurnState::InterruptedQuota {
                            reason: "rate_limit".into(),
                        },
                        assistant_message: None,
                    },
                ),
                record(
                    "2026-09-22T10:00:00.000Z",
                    TWO,
                    Harness::CODEX,
                    None,
                    Event::TurnStarted {
                        author: None,
                        prompt: Body::kept("hi"),
                    },
                ),
            ],
            "2026-09-17",
        )
    }

    #[test]
    fn the_report_names_each_harness_with_who_was_there_and_why_it_stopped() {
        let text = render(&week(), "from 2026-09-17").join("\n");

        assert!(
            text.contains("from 2026-09-17 (UTC) · 4 record(s)"),
            "{text}"
        );
        assert!(
            text.contains("claude_code — 1 conversation(s): attended 0 · unattended 1 · unknown 0"),
            "{text}"
        );
        assert!(text.contains("1 turn(s) · subagents 0"), "{text}");
        assert!(
            text.contains(
                "stopped: limit 1 in 1 episode(s) · account 0 · vendor 0 · request 0 · other 0 · by a person 0"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "1 tool call(s): failed 1 · interrupted 0 · refused 0 · asks 0 · 2h in tools"
            ),
            "{text}"
        );
        assert!(
            text.contains("codex — 1 conversation(s): attended 0 · unattended 0 · unknown 1"),
            "{text}"
        );
        assert!(text.contains("failing   Bash 1"), "{text}");
        assert!(text.contains("busiest   /w 2 (turns)"), "{text}");
        for leaked in ["secret plan", "cargo test", "exit 101"] {
            assert!(
                !text.contains(leaked),
                "{leaked} reached the report: {text}"
            );
        }
    }

    #[test]
    fn cost_is_named_as_absent_rather_than_left_out() {
        // A usage report silent about cost reads as one that has it.
        let text = render(&week(), "from 2026-09-17").join("\n");
        assert!(text.contains("cost      not in this log"), "{text}");
    }

    #[test]
    fn an_empty_window_says_so() {
        let text = render(&Ledger::default(), "from 2026-09-17").join("\n");
        assert!(text.contains("nothing recorded"), "{text}");
    }

    #[test]
    fn skipped_records_are_counted_in_the_header() {
        let ledger = Ledger {
            records: 9,
            recovered: 5,
            before_window: 3,
            repeated: 4,
            recorded_twice: 2,
            probe: 1,
            ..Ledger::default()
        };
        let text = render(&ledger, "from 2026-09-17").join("\n");
        assert!(
            text.contains("from 2026-09-17 (UTC) · 9 record(s), 5 recovered"),
            "{text}"
        );
        assert!(
            text.contains("not counted: earlier 3 · repeated 4 · recorded twice 2 · probe 1"),
            "{text}"
        );
        assert!(!text.contains("later"), "only what was set aside: {text}");
    }

    #[test]
    fn a_since_that_is_not_a_day_is_refused_rather_than_read_as_an_empty_week() {
        // The window is a text comparison, so `2026-9-1` would silently
        // count nothing from September onwards instead of saying so.
        for good in ["2026-09-01", "2026-09-01T12:00", "2026-09-01T12:00:00.000Z"] {
            assert!(check_since(good).is_ok(), "{good}");
        }
        for bad in [
            "2026-9-1",
            "yesterday",
            "2026-13-01",
            "2026-09-32",
            "",
            "2026-09",
        ] {
            let err = check_since(bad).expect_err(bad);
            assert!(err.to_string().contains("2026-09-01"), "{err}");
        }
    }

    #[test]
    fn shareable_prints_the_rows_a_sync_would_send_and_no_folder() {
        let lines = render_shareable(&week()).expect("json");

        assert_eq!(lines.len(), 2, "{lines:?}");
        for line in &lines {
            let row: serde_json::Value = serde_json::from_str(line).expect("a row");
            assert_eq!(row["day"], "2026-09-22");
            assert!(row.get("workspace").is_none(), "{line}");
            assert!(!line.contains("/w"), "{line}");
        }
    }

    #[test]
    fn shareable_wins_over_json() {
        assert_eq!(Output::of(false, false), Output::Text);
        assert_eq!(Output::of(true, false), Output::Json);
        assert_eq!(Output::of(true, true), Output::Shareable);
        assert_eq!(Output::of(false, true), Output::Shareable);
    }

    #[test]
    fn a_day_is_that_day_and_says_so() {
        let now: Timestamp = "2026-09-23T10:00:00.000Z".parse().expect("ts");
        let day = window(7, None, Some("2026-09-22"), now).expect("a day");
        assert_eq!(day.since, "2026-09-22");
        assert_eq!(day.until.as_deref(), Some("2026-09-23"));
        assert_eq!(day.label, "on 2026-09-22");
        let week = window(7, None, None, now).expect("a week");
        assert_eq!(
            (week.since.as_str(), week.until, week.label.as_str()),
            ("2026-09-17", None, "from 2026-09-17")
        );
        for bad in ["2026-9-22", "2026-09-22T12:00", "yesterday"] {
            assert!(window(7, None, Some(bad), now).is_err(), "{bad}");
        }
    }

    #[test]
    fn json_is_one_row_per_line_and_nothing_else() {
        let lines = render_json(&week()).expect("json");

        assert_eq!(lines.len(), 2, "{lines:?}");
        for line in &lines {
            let row: serde_json::Value = serde_json::from_str(line).expect("a row");
            assert_eq!(row["day"], "2026-09-22");
            assert!(!line.contains("secret plan"), "{line}");
        }
    }
}
