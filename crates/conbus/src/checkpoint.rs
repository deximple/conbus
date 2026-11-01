//! `cache/checkpoint.json` — the folded derived state at a named log point,
//! so the next start does not fold ten gigabytes again.
//!
//! Written by the start-up child, which scans the log anyway: its fold and
//! this file are what the serving daemon loads instead of scanning itself.
//! Like every `cache/` entry it is derived — delete it and nothing is lost;
//! a load that cannot validate falls back to the full fold silently.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::path::Path;

use conbus_core::ids::BusId;
use conbus_core::pack::Chapter;
use conbus_core::record::{Record, SCHEMA_VERSION};
use conbus_core::registry::{Restoring, SessionKey};
use conbus_core::segment::{Covered, SegmentLog};
use serde::{Deserialize, Serialize};

const CHECKPOINT_VERSION: u32 = 1;

/// First line of the file — everything a reader needs to decide whether the
/// checkpoint answers for this log. The payload line is only parsed after
/// this validates, so a coverage check never pays for a full read.
#[derive(Serialize, Deserialize)]
struct Head {
    checkpoint: u32,
    conbus: String,
    schema: u32,
    covered: Covered,
}

/// The folded state plus the summaries a fast-path `prepare` would need to
/// keep gap fill and split repair honest without rescanning.
#[derive(Serialize, Deserialize)]
struct Payload {
    /// Duplicated from the head line so the payload is self-describing; a
    /// reader cross-checks them only after `head` validates.
    covered: Covered,
    /// Registry mid-fold: `seen_live` must come back too or a delta record's
    /// "live outranks reconstructed" rule would fold the tail differently.
    restoring: Restoring,
    chapters: HashMap<BusId, Chapter>,
    records: u64,
    unreadable: u64,
    /// Vendor session → the live conversations it was written under, for
    /// `repair_splits` without `live_splits` over the whole log.
    session_buses: Vec<(SessionKey, Vec<BusId>)>,
    /// Every `aliased` edge the log already carries, for `already_aliased`.
    aliased: Vec<(BusId, BusId)>,
    /// The window gap dedupe can write into: records newer than the log's
    /// newest minus the margin `append_gaps_at_start` uses.
    recent: Vec<Record>,
}

/// What `restore` needs from a checkpoint, unwrapped for folding the delta.
pub struct Loaded {
    pub covered: Covered,
    pub restoring: Restoring,
    pub chapters: HashMap<BusId, Chapter>,
    pub records: u64,
    pub unreadable: u64,
}

/// The pieces `prepare` needs to skip its scan when the log has not moved:
/// what the full scan would have told gap fill and split repair.
///
/// Only `covered` is read today — the skip is total. The rest stay in the
/// file so a later revision can run the fills on these summaries instead of
/// rescanning; `missing_from` wants a conversation's whole turn set, not a
/// window, which is why this version does not try.
pub struct PrepareState {
    pub covered: Covered,
    /// Vendor session → live conversations (>1 is a split to repair).
    #[allow(dead_code)]
    pub session_buses: Vec<(SessionKey, BTreeSet<BusId>)>,
    /// `aliased` pairs already written, for the `already_aliased` check.
    #[allow(dead_code)]
    pub aliased: HashSet<(BusId, BusId)>,
    /// The margin window's records — `held` for the gap fill.
    #[allow(dead_code)]
    pub recent: Vec<Record>,
}

fn checkpoint_path(paths: &conbus_core::paths::Paths) -> std::path::PathBuf {
    paths.cache_dir().join("checkpoint.json")
}

/// The `CONBUS_NO_CHECKPOINT=1` escape hatch: every consumer treats the file
/// as absent.
fn disabled() -> bool {
    std::env::var_os("CONBUS_NO_CHECKPOINT").is_some()
}

/// The first line, validated. `None` when anything about the checkpoint
/// cannot be trusted — corrupt JSON, another build or schema, a covered
/// segment that shrank or vanished — which always means: fold from scratch.
fn head(paths: &conbus_core::paths::Paths) -> Option<Head> {
    if disabled() {
        return None;
    }
    let file = std::fs::File::open(checkpoint_path(paths)).ok()?;
    let mut first = String::new();
    io::BufRead::read_line(&mut io::BufReader::new(file), &mut first).ok()?;
    let head: Head = serde_json::from_str(&first).ok()?;
    if head.checkpoint != CHECKPOINT_VERSION
        || head.conbus != env!("CARGO_PKG_VERSION")
        || head.schema != SCHEMA_VERSION
    {
        return None;
    }
    // The covered segment must still exist with at least the covered length:
    // a shorter file means a healed torn tail or a rewritten log, and either
    // way the fold the checkpoint abbreviates no longer matches the bytes.
    segment_ok(paths.log_dir(), &head.covered)?;
    Some(head)
}

/// The payload line, validated by `head` first so a parse only runs when the
/// file answers for this log.
fn payload(paths: &conbus_core::paths::Paths) -> Option<Payload> {
    head(paths)?;
    let text = std::fs::read_to_string(checkpoint_path(paths)).ok()?;
    let mut lines = text.lines();
    let _: Head = serde_json::from_str(lines.next()?).ok()?;
    serde_json::from_str(lines.next()?).ok()
}

/// Load the checkpoint if it is present, readable, this build's own, and the
/// covered segment still exists at least as long as the checkpoint says.
/// Any failure is `None`, never a wrong answer.
pub fn load(paths: &conbus_core::paths::Paths) -> Option<Loaded> {
    let cp = payload(paths)?;
    Some(Loaded {
        covered: cp.covered,
        restoring: cp.restoring,
        chapters: cp.chapters,
        records: cp.records,
        unreadable: cp.unreadable,
    })
}

/// The prepare-side view, same validation.
pub fn load_prepare(paths: &conbus_core::paths::Paths) -> Option<PrepareState> {
    let cp = payload(paths)?;
    Some(PrepareState {
        covered: cp.covered,
        session_buses: cp
            .session_buses
            .into_iter()
            .map(|(key, ids)| (key, ids.into_iter().collect()))
            .collect(),
        aliased: cp.aliased.into_iter().collect(),
        recent: cp.recent,
    })
}

/// The covered segment, when it still exists and is at least as long as the
/// checkpoint claims. `None` is the caller's signal to fold from scratch.
fn segment_ok(log_dir: impl AsRef<Path>, covered: &Covered) -> Option<Covered> {
    let file = log_dir.as_ref().join(&covered.segment);
    let len = std::fs::metadata(&file).ok()?.len();
    if len < covered.bytes {
        return None;
    }
    Some(covered.clone())
}

/// Everything a checkpoint writer must know, folded and summarised.
pub struct Folded {
    pub restoring: Restoring,
    pub chapters: HashMap<BusId, Chapter>,
    pub records: u64,
    pub unreadable: u64,
    pub session_buses: Vec<(SessionKey, Vec<BusId>)>,
    pub aliased: Vec<(BusId, BusId)>,
    pub recent: Vec<Record>,
}

/// Fold `records` — the log order matters — into everything the checkpoint
/// stores. `defects` is the count the scan reported as unreadable.
pub fn fold(records: &[Record], defects: u64) -> Folded {
    let mut restoring = Restoring::default();
    let mut chapters = HashMap::new();
    for record in records {
        restoring.absorb(record);
        Chapter::apply(&mut chapters, record);
    }
    let newest = records.iter().map(|r| r.ts).max();
    let recent = match newest {
        Some(newest) => {
            let margin = crate::backfill::GAP_MARGIN_SECS;
            let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0)
                .expect("the unix epoch is a valid timestamp");
            let since = newest.seconds_since(epoch) - margin;
            records
                .iter()
                .filter(|r| r.ts.seconds_since(epoch) >= since)
                .cloned()
                .collect()
        }
        None => Vec::new(),
    };
    Folded {
        restoring,
        chapters,
        records: records.len() as u64,
        unreadable: defects,
        session_buses: session_buses(records),
        aliased: aliased_edges(records),
        recent,
    }
}

/// `live_splits` without the >1 filter — the checkpoint stores every session's
/// live conversation set so a later `repair_splits` recomputes the filter.
fn session_buses(records: &[Record]) -> Vec<(SessionKey, Vec<BusId>)> {
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
        .map(|(key, ids)| (key, ids.into_iter().collect()))
        .collect()
}

/// Every `aliased` edge the log carries, both directions stored once each.
fn aliased_edges(records: &[Record]) -> Vec<(BusId, BusId)> {
    records
        .iter()
        .filter_map(|record| match &record.event {
            conbus_core::event::Event::Aliased { also } => Some((record.bus_id, *also)),
            _ => None,
        })
        .collect()
}

/// Write the checkpoint beside the sidecars: temp file, rename, owner-only.
/// A failed write is a note, not a death — the next start just folds.
pub fn save(paths: &conbus_core::paths::Paths, folded: Folded) -> io::Result<()> {
    if disabled() {
        return Ok(());
    }
    let Some(covered) = SegmentLog::coverage(paths.log_dir())? else {
        return Ok(());
    };
    let head = Head {
        checkpoint: CHECKPOINT_VERSION,
        conbus: env!("CARGO_PKG_VERSION").to_string(),
        schema: SCHEMA_VERSION,
        covered: covered.clone(),
    };
    let cp = Payload {
        covered,
        restoring: folded.restoring,
        chapters: folded.chapters,
        records: folded.records,
        unreadable: folded.unreadable,
        session_buses: folded.session_buses,
        aliased: folded.aliased,
        recent: folded.recent,
    };
    let cache = paths.cache_dir();
    std::fs::create_dir_all(&cache)?;
    let file = checkpoint_path(paths);
    let tmp = cache.join(".checkpoint.json.tmp");
    std::fs::write(
        &tmp,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&head)?,
            serde_json::to_string(&cp)?
        ),
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, &file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::event::{Body, Event, Harness, Source, TurnState};
    use conbus_core::paths::Paths;
    use conbus_core::record::Entry;
    use conbus_core::retention::Retention;

    fn paths(home: &tempfile::TempDir) -> Paths {
        Paths::from_env(home.path().join("bus").to_str(), None, None).expect("paths")
    }

    fn source(session: &str) -> Source {
        Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: None,
            vendor_session_id: Some(session.to_string()),
            workspace: None,
            entrypoint: None,
            backfilled_from: None,
            profile: None,
        }
    }

    /// Appends one record through a real `SegmentLog`, because `save`'s
    /// coverage point must name bytes that exist.
    fn write(paths: &Paths, bus_id: BusId, session: &str, event: Event) -> Record {
        let mut log = SegmentLog::open(paths.log_dir(), Retention::default()).expect("open");
        log.append(Entry {
            bus_id,
            observed_at: None,
            source: source(session),
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event,
        })
        .expect("append")
        .expect("a record")
    }

    /// A checkpoint over a small live log: a turn under one conversation.
    fn checkpointed(home: &tempfile::TempDir) -> (Paths, Vec<Record>) {
        let paths = paths(home);
        let bus = BusId::mint();
        write(
            &paths,
            bus,
            "sess-a",
            Event::TurnStarted {
                author: None,
                prompt: Body::kept("hello"),
            },
        );
        write(
            &paths,
            bus,
            "sess-a",
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: None,
                tokens: None,
            },
        );
        let scan = SegmentLog::scan(paths.log_dir()).expect("scan");
        save(&paths, fold(&scan.records, 0)).expect("save");
        (paths, scan.records)
    }

    #[test]
    fn a_checkpoint_round_trips_the_fold_it_abbreviates() {
        let home = tempfile::tempdir().expect("tempdir");
        let (paths, records) = checkpointed(&home);
        let covered = SegmentLog::coverage(paths.log_dir())
            .expect("coverage")
            .expect("covered");

        let loaded = load(&paths).expect("the checkpoint just written loads");

        let mut restoring = Restoring::default();
        for record in &records {
            restoring.absorb(record);
        }
        assert_eq!(loaded.restoring, restoring);
        assert_eq!(loaded.records, records.len() as u64);
        assert_eq!(loaded.covered, covered);
        assert_eq!(loaded.chapters.len(), 1);
        let state = load_prepare(&paths).expect("the prepare view loads too");
        assert_eq!(state.covered, covered);
    }

    #[test]
    fn a_corrupt_checkpoint_loads_nothing_and_costs_a_full_fold() {
        let home = tempfile::tempdir().expect("tempdir");
        let (paths, _) = checkpointed(&home);
        let file = paths.cache_dir().join("checkpoint.json");
        std::fs::write(&file, "{not json").expect("corrupt");
        assert!(load(&paths).is_none());
        assert!(load_prepare(&paths).is_none());
    }

    #[test]
    fn a_checkpoint_from_another_build_or_schema_loads_nothing() {
        let home = tempfile::tempdir().expect("tempdir");
        let (paths, _) = checkpointed(&home);
        let file = paths.cache_dir().join("checkpoint.json");

        let text = std::fs::read_to_string(&file).expect("read");
        let mut lines = text.lines();
        let mut head: serde_json::Value =
            serde_json::from_str(lines.next().expect("head")).expect("parse head");
        let payload = lines.next().expect("payload").to_string();
        head["checkpoint"] = serde_json::json!(99);
        std::fs::write(&file, format!("{head}\n{payload}\n")).expect("rewrite");
        assert!(load(&paths).is_none(), "checkpoint version gates");

        let text = std::fs::read_to_string(&file).expect("read");
        let mut lines = text.lines();
        let mut head: serde_json::Value =
            serde_json::from_str(lines.next().expect("head")).expect("parse head");
        let payload = lines.next().expect("payload").to_string();
        head["checkpoint"] = serde_json::json!(CHECKPOINT_VERSION);
        head["schema"] = serde_json::json!(SCHEMA_VERSION + 1);
        std::fs::write(&file, format!("{head}\n{payload}\n")).expect("rewrite");
        assert!(load(&paths).is_none(), "schema version gates");
    }

    #[test]
    fn a_covered_segment_that_shrank_loads_nothing() {
        let home = tempfile::tempdir().expect("tempdir");
        let (paths, _) = checkpointed(&home);
        let covered = SegmentLog::coverage(paths.log_dir())
            .expect("coverage")
            .expect("covered");
        let file = paths.log_dir().join(&covered.segment);
        // A healed torn tail or a rewritten log leaves the file shorter than
        // the checkpoint's byte position; the fold it abbreviates no longer
        // matches the bytes.
        let truncated = std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open");
        truncated.set_len(covered.bytes - 1).expect("truncate");
        assert!(load(&paths).is_none());
    }

    #[test]
    fn no_log_and_no_checkpoint_means_no_checkpoint() {
        let home = tempfile::tempdir().expect("tempdir");
        let paths = paths(&home);
        assert!(load(&paths).is_none());
    }

    #[test]
    fn the_escape_hatch_ignores_a_valid_checkpoint() {
        let home = tempfile::tempdir().expect("tempdir");
        let (paths, _) = checkpointed(&home);
        unsafe { std::env::set_var("CONBUS_NO_CHECKPOINT", "1") };
        let loaded = load(&paths);
        unsafe { std::env::remove_var("CONBUS_NO_CHECKPOINT") };
        assert!(loaded.is_none());
    }
}
