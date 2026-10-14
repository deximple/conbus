//! Survival counters. The kill criteria live here as numbers, not vibes.
//!
//! Routing hops and failover hops are separate. Injected units are a cost,
//! counted on every briefing. Turn authorship is three buckets: human,
//! machine, absent — never fold absent into human.
//!
//! `routing_hops` and `failover_hops` count **organic** hops only: the ones a
//! vendor signal caused. A hop an operator typed and a hop a probe staged are
//! real events and are counted, in buckets of their own, because a dashboard
//! that hides them lies in the other direction. On 2026-09-08 all eleven hops
//! on the live machine were operator-directed or staged by a probe, and
//! `conbus verdict` reported them as "routing 4 (3.35/week)" — the number the
//! kill criteria test against.

use std::collections::HashMap;

use crate::event::{Event, Hop, HopProvenance};
use crate::ids::BusId;
use crate::record::Record;

/// What the log says about hops, injections, and who typed.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Meter {
    pub routing_hops: u64,
    pub failover_hops: u64,
    /// Hops a person asked for by name.
    pub directed_hops: u64,
    /// Hops a run declared as staged.
    pub synthetic_hops: u64,
    /// Hops written before provenance was recorded. Their own bucket, never
    /// folded into the organic count.
    pub unattributed_hops: u64,
    /// A second `routed` for a transition already open on that conversation.
    /// `hop --apply` refuses to write one, so this counts what a retry or an
    /// older build left behind.
    pub duplicate_hops: u64,
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
        let mut open: HashMap<BusId, Option<(String, Hop)>> = HashMap::new();
        for record in records {
            match &record.event {
                Event::Routed {
                    next,
                    hop,
                    provenance,
                    ..
                } => {
                    let transition = (next.clone(), *hop);
                    if open.get(&record.bus_id) == Some(&Some(transition.clone())) {
                        meter.duplicate_hops += 1;
                        continue;
                    }
                    open.insert(record.bus_id, Some(transition));
                    match provenance {
                        Some(HopProvenance::VendorSignal) => match hop {
                            Hop::HandedOff => meter.routing_hops += 1,
                            Hop::Displaced => meter.failover_hops += 1,
                        },
                        Some(HopProvenance::Directed) => meter.directed_hops += 1,
                        Some(HopProvenance::DeclaredSynthetic) => meter.synthetic_hops += 1,
                        None => meter.unattributed_hops += 1,
                    }
                }
                Event::Continued { hop, .. } => {
                    open.insert(record.bus_id, None);
                    match hop {
                        Hop::HandedOff => meter.routing_edges += 1,
                        Hop::Displaced => meter.failover_edges += 1,
                    }
                }
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

    /// What was counted and left out of the organic figures. Printed beside
    /// `describe`, never instead of it: a hidden hop is the same lie as an
    /// overcounted one.
    pub fn describe_provenance(&self) -> String {
        format!(
            "excluded  directed {} · declared synthetic {} · unattributed {} · duplicate {}",
            self.directed_hops, self.synthetic_hops, self.unattributed_hops, self.duplicate_hops
        )
    }

    pub fn describe(&self) -> String {
        format!(
            "hops      routing {} · failover {} · edges routing {} · failover {} [organic]",
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
                profile: None,
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
                code_freshness: None,
                provenance: Some(HopProvenance::VendorSignal),
            }),
            record(Event::Routed {
                next: "codex".into(),
                hop: Hop::Displaced,
                reason: "rate_limit".into(),
                code_freshness: None,
                provenance: Some(HopProvenance::VendorSignal),
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

    fn routed(next: &str, hop: Hop, provenance: Option<HopProvenance>) -> Record {
        record(Event::Routed {
            next: next.into(),
            hop,
            reason: "reason".into(),
            code_freshness: None,
            provenance,
        })
    }

    #[test]
    fn only_a_vendor_signal_counts_as_a_hop_the_router_made() {
        // On 2026-09-08 every hop on the live machine was one an operator
        // typed or a probe staged, and the dashboard reported them as
        // "routing 4 (3.35/week)" — the number the kill criteria test
        // against. They are still counted, in buckets of their own, because
        // hiding them is the same lie in the other direction.
        let meter = Meter::fold(&[
            routed("codex", Hop::HandedOff, Some(HopProvenance::VendorSignal)),
            routed("cursor", Hop::HandedOff, Some(HopProvenance::Directed)),
            routed(
                "grok",
                Hop::Displaced,
                Some(HopProvenance::DeclaredSynthetic),
            ),
            routed("agy", Hop::Displaced, None),
        ]);

        assert_eq!(meter.routing_hops, 1, "the vendor signal, and only it");
        assert_eq!(meter.failover_hops, 0);
        assert_eq!(meter.directed_hops, 1);
        assert_eq!(meter.synthetic_hops, 1);
        assert_eq!(meter.unattributed_hops, 1);
        assert!(
            meter.describe_provenance().contains("unattributed 1"),
            "{}",
            meter.describe_provenance()
        );
    }

    #[test]
    fn a_second_routed_for_one_transition_is_not_a_second_hop() {
        // `hop --apply` refuses to write one, so this counts what a retry or
        // an older build left behind. A conversation that hops, comes back,
        // and hops again is two.
        let meter = Meter::fold(&[
            routed("codex", Hop::Displaced, Some(HopProvenance::VendorSignal)),
            routed("codex", Hop::Displaced, Some(HopProvenance::VendorSignal)),
        ]);

        assert_eq!(meter.failover_hops, 1);
        assert_eq!(meter.duplicate_hops, 1);
    }

    #[test]
    fn a_conversation_that_comes_back_can_hop_again() {
        let meter = Meter::fold(&[
            routed("codex", Hop::Displaced, Some(HopProvenance::VendorSignal)),
            record(Event::Continued {
                predecessor: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
                hop: Hop::Displaced,
            }),
            routed("codex", Hop::Displaced, Some(HopProvenance::VendorSignal)),
        ]);

        assert_eq!(meter.failover_hops, 2, "the edge closed the first one");
        assert_eq!(meter.duplicate_hops, 0);
    }
}
