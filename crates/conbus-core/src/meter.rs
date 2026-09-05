//! Survival counters. The kill criteria live here as numbers, not vibes.
//!
//! Routing hops and failover hops are separate. Injected units are a cost,
//! counted on every briefing. Turn authorship is three buckets: human,
//! machine, absent — never fold absent into human.

use crate::event::{Event, Hop};
use crate::record::Record;

/// What the log says about hops, injections, and who typed.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Meter {
    pub routing_hops: u64,
    pub failover_hops: u64,
    /// `continued` edges, counted the same way. A decision without an edge
    /// is a hop that did not happen; an edge without a decision is ambient.
    pub routing_edges: u64,
    pub failover_edges: u64,
    pub injected_units: u64,
    pub briefings: u64,
    pub human_turns: u64,
    pub machine_turns: u64,
    pub unlabelled_turns: u64,
    pub summaries: u64,
}

impl Meter {
    pub fn fold(records: &[Record]) -> Self {
        let mut meter = Meter::default();
        for record in records {
            match &record.event {
                Event::Routed { hop, .. } => match hop {
                    Hop::HandedOff => meter.routing_hops += 1,
                    Hop::Displaced => meter.failover_hops += 1,
                },
                Event::Continued { hop, .. } => match hop {
                    Hop::HandedOff => meter.routing_edges += 1,
                    Hop::Displaced => meter.failover_edges += 1,
                },
                Event::Briefed { units, .. } => {
                    meter.briefings += 1;
                    meter.injected_units += *units as u64;
                }
                Event::TurnStarted { author, .. } => match author {
                    Some(author) if author.is_human() => meter.human_turns += 1,
                    Some(author) if author.is_machine() => meter.machine_turns += 1,
                    _ => meter.unlabelled_turns += 1,
                },
                Event::Summarised { .. } => meter.summaries += 1,
                _ => {}
            }
        }
        meter
    }

    pub fn describe(&self) -> String {
        format!(
            "hops      routing {} · failover {} · edges routing {} · failover {}",
            self.routing_hops, self.failover_hops, self.routing_edges, self.failover_edges
        )
    }

    pub fn describe_cost(&self) -> String {
        format!(
            "inject    {} briefing(s) · {} units (cost) · {} summar{}",
            self.briefings,
            self.injected_units,
            self.summaries,
            if self.summaries == 1 { "y" } else { "ies" }
        )
    }

    pub fn describe_turns(&self) -> String {
        format!(
            "turns     human {} · machine {} · unlabelled {}",
            self.human_turns, self.machine_turns, self.unlabelled_turns
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Author, Body, Harness, Hop, Source};
    use crate::record::{Record, SCHEMA_VERSION};

    fn record(event: Event) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 1,
            ts: "2026-09-06T00:00:00.000Z".parse().expect("ts"),
            bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
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
    fn hops_split_routing_from_failover() {
        let meter = Meter::fold(&[
            record(Event::Routed {
                next: "codex".into(),
                hop: Hop::HandedOff,
                reason: "80%".into(),
            }),
            record(Event::Routed {
                next: "codex".into(),
                hop: Hop::Displaced,
                reason: "rate_limit".into(),
            }),
            record(Event::Continued {
                predecessor: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
                hop: Hop::Displaced,
            }),
        ]);
        assert_eq!(meter.routing_hops, 1);
        assert_eq!(meter.failover_hops, 1);
        assert_eq!(meter.failover_edges, 1);
        assert_eq!(meter.routing_edges, 0);
    }

    #[test]
    fn injected_units_count_every_briefing() {
        let meter = Meter::fold(&[record(Event::Briefed {
            about: Vec::new(),
            units: 400,
            dropped: 0,
        })]);
        assert_eq!(meter.briefings, 1);
        assert_eq!(meter.injected_units, 400);
    }

    #[test]
    fn absent_author_is_its_own_bucket() {
        let human = Event::TurnStarted {
            author: Some(Author::new(Author::TYPED)),
            prompt: Body::kept("hi"),
        };
        let machine = Event::TurnStarted {
            author: Some(Author::new(Author::SDK)),
            prompt: Body::kept("hi"),
        };
        let absent = Event::TurnStarted {
            author: None,
            prompt: Body::kept("hi"),
        };
        let meter = Meter::fold(&[record(human), record(machine), record(absent)]);
        assert_eq!(meter.human_turns, 1);
        assert_eq!(meter.machine_turns, 1);
        assert_eq!(meter.unlabelled_turns, 1);
    }
}
