//! When one vendor session was recorded under more than one `bus_id`.
//!
//! The log is append-only, so the two conversations stay. An `aliased`
//! record on both sides is how a reader following either id sees the rest.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::path::Path;

use crate::event::Event;
use crate::ids::BusId;
use crate::record::Record;
use crate::registry::SessionKey;
use crate::segment::SegmentLog;

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

/// What following aliases needs from the log: where to start, and the edges.
#[derive(Default)]
struct AliasFold {
    seeds: HashSet<BusId>,
    edges: HashMap<BusId, HashSet<BusId>>,
}

impl AliasFold {
    fn absorb(&mut self, record: &Record, seed: &str) {
        if record.bus_id.to_string().starts_with(seed) {
            self.seeds.insert(record.bus_id);
        }
        if let Event::Aliased { also } = &record.event {
            if also.to_string().starts_with(seed) {
                self.seeds.insert(*also);
                self.seeds.insert(record.bus_id);
            }
            self.edges.entry(record.bus_id).or_default().insert(*also);
            self.edges.entry(*also).or_default().insert(record.bus_id);
        }
    }

    fn reach(self) -> HashSet<BusId> {
        let mut seen = HashSet::new();
        let mut stack: Vec<BusId> = self.seeds.into_iter().collect();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(next) = self.edges.get(&id) {
                stack.extend(next.iter().copied());
            }
        }
        seen
    }
}

/// Every conversation reachable from `seed` by prefix match and `aliased` edges.
pub fn expand_aliases(records: &[Record], seed: &str) -> HashSet<BusId> {
    let mut fold = AliasFold::default();
    for record in records {
        fold.absorb(record, seed);
    }
    fold.reach()
}

/// As [`expand_aliases`], reading the log rather than a slice of it: an
/// `aliased` record can sit anywhere, so a reader of the last few records cannot
/// find them in what it holds. Keeps the edges and not the records.
pub fn expand_aliases_in_log(
    dir: impl AsRef<Path>,
    cache: Option<&Path>,
    seed: &str,
) -> io::Result<HashSet<BusId>> {
    let mut fold = AliasFold::default();
    SegmentLog::scan_meta_each(dir, cache, |record| fold.absorb(&record, seed))?;
    Ok(fold.reach())
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
            hook_id: None,
            spooled_at: None,
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
