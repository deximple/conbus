//! When one vendor session was recorded under more than one `bus_id`.
//!
//! The log is append-only, so the two conversations stay. An `aliased`
//! record on both sides is how a reader following either id sees the rest.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::event::Event;
use crate::ids::BusId;
use crate::record::Record;
use crate::registry::SessionKey;

/// Live (not backfilled) conversations a vendor session was written under.
pub fn live_splits(records: &[Record]) -> Vec<(SessionKey, BTreeSet<BusId>)> {
    let mut live: HashMap<SessionKey, BTreeSet<BusId>> = HashMap::new();
    for record in records {
        if record.source.backfilled_from.is_some() {
            continue;
        }
        let Some(vendor_session_id) = &record.source.vendor_session_id else {
            continue;
        };
        let key = SessionKey::new(record.source.harness.clone(), vendor_session_id.clone());
        live.entry(key).or_default().insert(record.bus_id);
    }
    live.into_iter()
        .filter(|(_, conversations)| conversations.len() > 1)
        .collect()
}

/// Every conversation reachable from `seed` by prefix match and `aliased` edges.
pub fn expand_aliases(records: &[Record], seed: &str) -> HashSet<BusId> {
    let mut seeds: HashSet<BusId> = HashSet::new();
    for record in records {
        if record.bus_id.to_string().starts_with(seed) {
            seeds.insert(record.bus_id);
        }
        if let Event::Aliased { also } = &record.event
            && also.to_string().starts_with(seed)
        {
            seeds.insert(*also);
            seeds.insert(record.bus_id);
        }
    }
    let mut edges: HashMap<BusId, HashSet<BusId>> = HashMap::new();
    for record in records {
        let Event::Aliased { also } = &record.event else {
            continue;
        };
        edges.entry(record.bus_id).or_default().insert(*also);
        edges.entry(*also).or_default().insert(record.bus_id);
    }
    let mut seen = HashSet::new();
    let mut stack: Vec<BusId> = seeds.into_iter().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(next) = edges.get(&id) {
            stack.extend(next.iter().copied());
        }
    }
    seen
}

/// Whether both sides already carry an `aliased` record pointing at the other.
pub fn already_aliased(records: &[Record], a: BusId, b: BusId) -> bool {
    let set = expand_aliases(records, &a.to_string());
    set.contains(&a) && set.contains(&b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Timestamp;
    use crate::event::{Body, Event, Harness, Source};
    use crate::record::SCHEMA_VERSION;

    fn rec(bus_id: BusId, session: &str, event: Event) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 1,
            ts: Timestamp::now(),
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some(session.into()),
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            },
            event,
        }
    }

    #[test]
    fn a_split_session_is_found_and_an_alias_joins_both_ids() {
        let a: BusId = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("a");
        let b: BusId = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3f".parse().expect("b");
        let prompt = Event::TurnStarted {
            author: None,
            prompt: Body::kept("hi"),
        };
        let records = vec![
            rec(a, "vendor-split", prompt.clone()),
            rec(b, "vendor-split", prompt),
            rec(a, "vendor-split", Event::Aliased { also: b }),
            rec(b, "vendor-split", Event::Aliased { also: a }),
        ];

        let splits = live_splits(&records);
        assert_eq!(splits.len(), 1);
        assert_eq!(splits[0].1.len(), 2);
        let following_a = expand_aliases(&records, "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e");
        assert!(following_a.contains(&a));
        assert!(following_a.contains(&b));
        assert!(already_aliased(&records, a, b));
    }
}
