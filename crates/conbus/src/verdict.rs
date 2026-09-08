//! Four-week dogfood score. The kill criteria are numbers, not a vibe.
//!
//! The clock cannot be closed by hoping. A window shorter than 28 days
//! prints `open` and refuses the "no wedge" / "failover is not core"
//! sentences. Re-explain tokens are not in the log; this command says so.

use std::io;

use conbus_core::clock::Timestamp;
use conbus_core::event::Event;
use conbus_core::meter::Meter;
use conbus_core::paths::Paths;
use conbus_core::record::Record;
use conbus_core::segment::SegmentLog;

/// Plan: 4 weeks of dogfood before a kill-criteria verdict.
pub const WINDOW_SECS: i64 = 28 * 24 * 60 * 60;
const WEEK_SECS: f64 = 7.0 * 24.0 * 60.0 * 60.0;
const ROUTING_PER_WEEK: f64 = 3.0;
const FAILOVER_PER_WEEK: f64 = 1.0;

#[derive(Debug, PartialEq)]
pub struct Verdict {
    pub records: usize,
    pub elapsed_secs: i64,
    pub window_closed: bool,
    pub routing_hops: u64,
    pub failover_hops: u64,
    pub routing_per_week: f64,
    pub failover_per_week: f64,
    pub reasks_in_3: u64,
    pub injected_units: u64,
}

impl Verdict {
    pub fn score(records: &[Record], now: Timestamp) -> Self {
        // Backfill is recovered history, not dogfood. The kill clock starts
        // on the first live record.
        let live: Vec<Record> = records
            .iter()
            .filter(|record| record.source.backfilled_from.is_none())
            .cloned()
            .collect();
        let meter = Meter::fold(&live);
        let elapsed_secs = live
            .first()
            .map(|record| now.seconds_since(record.ts).max(0))
            .unwrap_or(0);
        let weeks = (elapsed_secs as f64 / WEEK_SECS).max(1.0 / 7.0);
        Verdict {
            records: live.len(),
            elapsed_secs,
            window_closed: elapsed_secs >= WINDOW_SECS,
            routing_hops: meter.routing_hops,
            failover_hops: meter.failover_hops,
            routing_per_week: meter.routing_hops as f64 / weeks,
            failover_per_week: meter.failover_hops as f64 / weeks,
            reasks_in_3: reasks_within_three(&live),
            injected_units: meter.injected_units,
        }
    }
}

/// Human prompts in the three `TurnStarted` that follow a `Continued`.
fn reasks_within_three(records: &[Record]) -> u64 {
    let mut n = 0;
    for (i, record) in records.iter().enumerate() {
        if !matches!(record.event, Event::Continued { .. }) {
            continue;
        }
        let mut seen = 0;
        for later in &records[i + 1..] {
            if later.bus_id != record.bus_id {
                continue;
            }
            let Event::TurnStarted { author, .. } = &later.event else {
                continue;
            };
            seen += 1;
            if author.as_ref().is_some_and(|a| a.is_human()) {
                n += 1;
            }
            if seen == 3 {
                break;
            }
        }
    }
    n
}

pub fn run(paths: &Paths) -> io::Result<()> {
    let scan = SegmentLog::scan(paths.log_dir())?;
    let verdict = Verdict::score(&scan.records, Timestamp::now());
    print_verdict(&verdict);
    Ok(())
}

fn print_verdict(v: &Verdict) {
    let days = v.elapsed_secs as f64 / 86_400.0;
    println!(
        "window    {} · {:.1} day(s) of {} · {} record(s)",
        if v.window_closed { "closed" } else { "open" },
        days,
        WINDOW_SECS / 86_400,
        v.records
    );
    println!(
        "hops      routing {} ({:.2}/week) · failover {} ({:.2}/week)",
        v.routing_hops, v.routing_per_week, v.failover_hops, v.failover_per_week
    );
    println!(
        "reask     {} human prompt(s) in the 3 turns after a continued edge",
        v.reasks_in_3
    );
    println!("reexplain unmeasured — the log has no post-hop token delta");
    println!(
        "inject    {} units (cost, not a kill signal)",
        v.injected_units
    );
    if !v.window_closed {
        println!("verdict   window still open — do not apply the kill criteria");
        return;
    }
    if v.routing_per_week < ROUTING_PER_WEEK {
        println!("verdict   routing hop < 3/week — no routing wedge");
    } else if v.failover_hops > 0 && v.failover_per_week < FAILOVER_PER_WEEK {
        println!("verdict   failover < 1/week — failover is not core; treat as routing");
    } else if v.reasks_in_3 == 0 {
        println!("verdict   window closed · reask 0 · hop is carrying the conversation");
    } else {
        println!(
            "verdict   window closed · {} reask(s) after hop — pack quality is the gap",
            v.reasks_in_3
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::event::{Author, Body, Event, Harness, Hop, Source};
    use conbus_core::ids::BusId;
    use conbus_core::record::{Record, SCHEMA_VERSION};

    fn rec(ts: &str, event: Event) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            ts: ts.parse().expect("ts"),
            seq: 1,
            bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e"
                .parse::<BusId>()
                .expect("id"),
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: None,
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
            },
            event,
        }
    }

    #[test]
    fn a_week_of_log_is_an_open_window() {
        let records = [rec(
            "2026-09-01T00:00:00.000Z",
            Event::Routed {
                next: "codex".into(),
                hop: Hop::HandedOff,
                reason: "directed".into(),
                code_freshness: None,
            },
        )];
        let now = "2026-09-07T00:00:00.000Z".parse().expect("now");
        let v = Verdict::score(&records, now);
        assert!(!v.window_closed);
        assert_eq!(v.routing_hops, 1);
    }

    #[test]
    fn twenty_eight_days_closes_the_window() {
        let records = [rec(
            "2026-08-01T00:00:00.000Z",
            Event::Routed {
                next: "codex".into(),
                hop: Hop::Displaced,
                reason: "rate_limit".into(),
                code_freshness: None,
            },
        )];
        let now = "2026-08-29T00:00:00.000Z".parse().expect("now");
        let v = Verdict::score(&records, now);
        assert!(v.window_closed);
        assert_eq!(v.failover_hops, 1);
    }

    #[test]
    fn a_human_prompt_after_continue_counts_as_a_reask() {
        let bus_event = |event| rec("2026-09-01T00:00:00.000Z", event);
        let records = [
            bus_event(Event::Continued {
                predecessor: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
                hop: Hop::Displaced,
            }),
            bus_event(Event::TurnStarted {
                author: Some(Author::new(Author::TYPED)),
                prompt: Body::kept("what were we doing"),
            }),
        ];
        assert_eq!(reasks_within_three(&records), 1);
    }

    #[test]
    fn backfill_does_not_start_the_dogfood_clock() {
        let mut old = rec(
            "2026-07-01T00:00:00.000Z",
            Event::TurnStarted {
                author: Some(Author::new(Author::TYPED)),
                prompt: Body::kept("old"),
            },
        );
        old.source.backfilled_from = Some("claude_transcript".into());
        let live = rec(
            "2026-09-01T00:00:00.000Z",
            Event::Routed {
                next: "codex".into(),
                hop: Hop::HandedOff,
                reason: "directed".into(),
                code_freshness: None,
            },
        );
        let now = "2026-09-07T00:00:00.000Z".parse().expect("now");
        let v = Verdict::score(&[old, live], now);
        assert!(!v.window_closed);
        assert_eq!(v.routing_hops, 1);
        assert!(v.elapsed_secs < WINDOW_SECS);
    }
}
