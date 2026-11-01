//! Which vendor session belongs to which conversation.
//!
//! The rule this module exists to enforce: a `bus_id` is minted on the first
//! user prompt, never on a session start. Claude Code fires `SessionStart`
//! several times per project within a few hundred milliseconds and some of
//! those are ghosts that never get a transcript
//! (anthropics/claude-code#78455). Minting there would pile up ids that no
//! conversation ever uses, and can leave a real session aliased to a ghost.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::event::{Event, Harness, VendorAgentId, VendorTurnId};
use crate::ids::BusId;
use crate::record::Record;

/// serde_json map keys must be strings; `SessionKey` and `(String, String)`
/// are not. Encode those maps as a flat list of key/value pairs instead.
mod serde_pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;
    use std::hash::Hash;

    pub(super) fn serialize<K, V, S>(map: &HashMap<K, V>, s: S) -> Result<S::Ok, S::Error>
    where
        K: Serialize + Eq + Hash,
        V: Serialize,
        S: Serializer,
    {
        map.iter().collect::<Vec<_>>().serialize(s)
    }

    pub(super) fn deserialize<'de, K, V, D>(d: D) -> Result<HashMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(d)?.into_iter().collect())
    }
}

/// A vendor session, namespaced by the harness that named it.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SessionKey {
    pub harness: Harness,
    pub vendor_session_id: String,
}

impl SessionKey {
    pub fn new(harness: Harness, vendor_session_id: impl Into<String>) -> Self {
        SessionKey {
            harness,
            vendor_session_id: vendor_session_id.into(),
        }
    }
}

/// One observed event together with the correlators its own payload carried.
///
/// They travel with the event rather than with the session stamp because a
/// deferred session start and the prompt that flushes it are written in one
/// batch but did not carry the same correlator — the vendor sends none on a
/// session start. Sharing one stamp across the batch would invent that
/// correlation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Observed {
    pub vendor_turn_id: Option<VendorTurnId>,
    pub vendor_agent_id: Option<VendorAgentId>,
    /// The checkout the harness reported for this event. Travels per
    /// observation for the same reason the correlators do: a session start and
    /// the prompt that flushes it are written in one batch but were observed at
    /// different moments, and a checkout can change in between.
    pub workspace: Option<String>,
    pub event: Event,
}

impl Observed {
    /// An event that carried no correlators.
    pub fn bare(event: Event) -> Self {
        Observed {
            vendor_turn_id: None,
            vendor_agent_id: None,
            workspace: None,
            event,
        }
    }
}

/// What the daemon should do with an observed event.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Disposition {
    /// Remembered as a session candidate. Nothing reaches the log yet, because
    /// this session may turn out to be a ghost.
    Deferred,
    /// Write these, in order, under this conversation.
    Write {
        bus_id: BusId,
        observed: Vec<Observed>,
    },
    /// Belongs to no conversation, and this kind of event cannot start one.
    Orphaned,
}

/// Who a directed hop reserved a leg for, in one checkout.
///
/// Claimed is an identity claim, and this codebase only makes one where the
/// identity is known. Two conversations claiming the same leg in the same
/// checkout is exactly the case where it is not, so the seat holds nobody
/// rather than the last writer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum Seat {
    Claimed(BusId),
    Contested,
}

/// The vendor-session-to-conversation map. Lives in the daemon's memory.
#[derive(Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Registry {
    #[serde(with = "serde_pairs")]
    bound: HashMap<SessionKey, BusId>,
    /// Session starts seen but not yet proven real, kept so that a conversation
    /// can log where it began once its first prompt arrives.
    ///
    /// Bounded. Most of what lands here never mints — Claude Code fires several
    /// `SessionStart`s per project within a few hundred milliseconds and some
    /// are ghosts with no transcript — so an unbounded map grows for the life
    /// of the daemon, which is now supervised and meant to run for weeks.
    /// Ordered by an insertion counter. The map holds no clock value of its
    /// own, so a counter is the only ordering available without adding one —
    /// and adding a timestamp would not work anyway: they are truncated to
    /// milliseconds, and a burst of session starts lands inside a single one,
    /// which makes eviction order arbitrary. That was not a guess; it turned a
    /// test red.
    #[serde(with = "serde_pairs")]
    candidates: HashMap<SessionKey, (u64, Observed)>,
    remembered: u64,
    /// A hop to a catalog leg that cannot keep a warm session. The next
    /// unbound prompt in that checkout claims this conversation.
    #[serde(with = "serde_pairs")]
    pending: HashMap<(String, String), Seat>,
}

/// A registry being rebuilt from the log, one record at a time.
///
/// [`Registry::restore`] folds a collected slice with it. The daemon folds a
/// streaming scan instead, so that rebuilding its state at start does not cost
/// as much memory as the log itself.
#[derive(Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Restoring {
    registry: Registry,
    /// Sessions a record mirrored live has claimed; a reconstruction binds only
    /// the rest (see [`Registry::restore`]).
    seen_live: HashSet<SessionKey>,
}

impl Restoring {
    /// Fold one record, in log order.
    pub fn absorb(&mut self, record: &Record) {
        let Some(vendor_session_id) = &record.source.vendor_session_id else {
            return;
        };
        let key = SessionKey::new(record.source.harness.clone(), vendor_session_id.clone());
        let reconstructed = record.source.backfilled_from.is_some();
        if reconstructed && self.seen_live.contains(&key) {
            return;
        }
        if !reconstructed {
            self.seen_live.insert(key.clone());
        }
        self.registry.bound.insert(key, record.bus_id);
        self.registry.alias_warm_leg(record.bus_id, &record.event);
        self.registry.note_route_record(record);
    }

    /// The registry the records folded so far describe.
    pub fn finish(self) -> Registry {
        self.registry
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the vendor-session bindings from the log.
    ///
    /// The registry is derived state, and derived state has to be recoverable
    /// from the segments or a daemon restart forks one conversation into two:
    /// the vendor session keeps its id, we forget which conversation it was,
    /// and its next prompt mints a second `bus_id`.
    ///
    /// Later records win, so a session that was rebound keeps its newest
    /// conversation — with one exception. A record the recorder mirrored live
    /// outranks one backfill reconstructed, whatever order they were written
    /// in. Backfill runs with the daemon stopped and appends its copy of a
    /// transcript *after* whatever the recorder had already written for the
    /// same session, so "later wins" alone bound every session alive across a
    /// backfill to its copy when the daemon came back, and their remaining
    /// turns were written under the wrong conversation. Two conversations on
    /// a real log were split that way. A reconstruction only binds a session
    /// nothing live has claimed, which is the case it was meant for: a
    /// session resuming after its history was recovered continues the
    /// recovered conversation instead of minting a second one.
    ///
    /// Cost is one pass over the log; the derived SQLite index the plan
    /// schedules is what replaces it once the log stops being small.
    pub fn restore(records: &[Record]) -> Self {
        let mut restoring = Restoring::default();
        for record in records {
            restoring.absorb(record);
        }
        restoring.finish()
    }

    /// The conversation a vendor session is bound to, if any.
    pub fn bus_id(&self, key: &SessionKey) -> Option<BusId> {
        self.bound.get(key).copied()
    }

    /// A `warm:` / `warm-claude:` lease names a session the bus opened. Bind
    /// that session to this conversation so hop-back keeps the predecessor's id.
    ///
    /// Does not steal: a session that already has a binding stays there.
    /// The strong identity claim is only for a session we started ourselves.
    fn alias_warm_leg(&mut self, bus_id: BusId, event: &Event) {
        if let Some(thread) = event.warm_thread() {
            let alias = SessionKey::new(Harness::new(Harness::CODEX), thread);
            self.bound.entry(alias).or_insert(bus_id);
        }
        if let Some(session) = event.claude_warm_session() {
            let alias = SessionKey::new(Harness::new(Harness::CLAUDE_CODE), session);
            self.bound.entry(alias).or_insert(bus_id);
        }
    }

    fn needs_seat(harness: &str) -> bool {
        !matches!(harness, Harness::CLAUDE_CODE | Harness::CODEX)
    }

    fn normalize_workspace(workspace: &str) -> Option<String> {
        let trimmed = workspace.trim().trim_end_matches('/');
        if trimmed.is_empty() || trimmed == "/" {
            return None;
        }
        Some(trimmed.to_string())
    }

    fn clear_pending_bus(&mut self, bus_id: BusId) {
        self.pending
            .retain(|_, seat| !matches!(seat, Seat::Claimed(id) if *id == bus_id));
    }

    fn note_route(&mut self, bus_id: BusId, observed: &Observed) {
        let Event::Routed { next, .. } = &observed.event else {
            return;
        };
        self.clear_pending_bus(bus_id);
        if !Self::needs_seat(next) {
            return;
        }
        let Some(workspace) = observed
            .workspace
            .as_deref()
            .and_then(Self::normalize_workspace)
        else {
            return;
        };
        // Two conversations in one checkout can be directed at the same
        // attach leg before either has said a word there — parallel sessions
        // or worktrees in one repo make that ordinary, not exotic. A plain
        // insert let the second silently take the first's seat, and the next
        // prompt in that checkout then bound to the wrong conversation and
        // the first was split a second time. A seat two conversations claim
        // belongs to neither: minting a fresh id is a recoverable
        // disappointment, and binding to the wrong one is not.
        match self.pending.entry((next.clone(), workspace)) {
            Entry::Vacant(seat) => {
                seat.insert(Seat::Claimed(bus_id));
            }
            Entry::Occupied(mut seat) => {
                if !matches!(seat.get(), Seat::Claimed(id) if *id == bus_id) {
                    seat.insert(Seat::Contested);
                }
            }
        }
    }

    fn note_route_record(&mut self, record: &Record) {
        self.note_route(
            record.bus_id,
            &Observed {
                vendor_turn_id: record.vendor_turn_id.clone(),
                vendor_agent_id: record.vendor_agent_id.clone(),
                workspace: record.source.workspace.clone(),
                event: record.event.clone(),
            },
        );
        if matches!(
            record.event,
            Event::TurnStarted { .. } | Event::TurnEnded { .. }
        ) {
            self.pending.retain(|(harness, _), seat| {
                !(matches!(seat, Seat::Claimed(id) if *id == record.bus_id)
                    && harness == record.source.harness.as_str())
            });
        }
    }

    fn take_seat(&mut self, harness: &Harness, workspace: Option<&str>) -> Option<BusId> {
        let workspace = Self::normalize_workspace(workspace?)?;
        match self
            .pending
            .remove(&(harness.as_str().to_string(), workspace))
        {
            Some(Seat::Claimed(bus_id)) => Some(bus_id),
            Some(Seat::Contested) | None => None,
        }
    }

    /// The most session starts held at once before the oldest is forgotten.
    ///
    /// Forgetting one costs the retroactive `session_candidate` record for that
    /// session, not the conversation: a prompt mints whether or not its start
    /// was remembered.
    pub const REMEMBERED_STARTS: usize = 1024;

    fn remember(&mut self, key: SessionKey, observed: Observed) {
        let full =
            self.candidates.len() >= Self::REMEMBERED_STARTS && !self.candidates.contains_key(&key);
        if let Some(oldest) = full
            .then(|| {
                self.candidates
                    .iter()
                    .min_by_key(|(_, (seen, _))| *seen)
                    .map(|(key, _)| key.clone())
            })
            .flatten()
        {
            self.candidates.remove(&oldest);
        }
        self.remembered += 1;
        self.candidates.insert(key, (self.remembered, observed));
    }

    /// How many session starts are being held. Ghosts never leave on their own.
    pub fn remembered_starts(&self) -> usize {
        self.candidates.len()
    }

    /// Record an event and say what should reach the log.
    pub fn observe(&mut self, key: &SessionKey, observed: Observed) -> Disposition {
        // A bound session is already proven real, so nothing about it is
        // deferred — including the session starts that fire again on resume,
        // compact, and fork.
        if let Some(bus_id) = self.bound.get(key).copied() {
            self.alias_warm_leg(bus_id, &observed.event);
            self.note_route(bus_id, &observed);
            return Disposition::Write {
                bus_id,
                observed: vec![observed],
            };
        }
        if matches!(observed.event, Event::SessionCandidate { .. }) {
            self.remember(key.clone(), observed);
            return Disposition::Deferred;
        }
        // Unbound. A prompt proves a session is real enough to mint.
        //
        // Cursor also mints on work it can be seen doing — a completed turn
        // or a tool call. Its prompt hook lives in Cursor's own hooks file
        // while its tool hooks arrive through Claude Code's settings file,
        // and the two do not always both fire: on 2026-09-08, 42 Cursor
        // conversations existed only as 4,229 orphaned `preToolUse` /
        // `postToolUse` drops, present nowhere in the log under any harness,
        // because no `beforeSubmitPrompt` ever reached the bus for them. A
        // tool call is proof of a live session in exactly the way a ghost
        // `SessionStart` is not, which is why this exception is Cursor's
        // alone and why a session that only *ends* still orphans.
        let cursor_proof = key.harness.as_str() == Harness::CURSOR
            && matches!(
                observed.event,
                Event::TurnEnded { .. } | Event::ToolRequested { .. } | Event::ToolUse { .. }
            );
        if !matches!(observed.event, Event::TurnStarted { .. }) && !cursor_proof {
            return Disposition::Orphaned;
        }
        // A directed hop to a leg without a bus-owned warm has no vendor
        // session yet. The next real prompt in that checkout sits on the
        // conversation that was handed off, instead of minting a twin.
        let bus_id = match self.take_seat(&key.harness, observed.workspace.as_deref()) {
            Some(bus_id) => bus_id,
            None => BusId::mint(),
        };
        self.bound.insert(key.clone(), bus_id);
        let observed = self
            .candidates
            .remove(key)
            .map(|(_, remembered)| remembered)
            .into_iter()
            .chain(std::iter::once(observed))
            .collect();
        Disposition::Write { bus_id, observed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backfill::CLAUDE_TRANSCRIPT;
    use crate::clock::Timestamp;
    use crate::event::{Body, Source, TurnState};
    use crate::record::SCHEMA_VERSION;

    fn key() -> SessionKey {
        SessionKey::new(Harness::new(Harness::CLAUDE_CODE), "vendor-abc")
    }

    /// A record as the log would hold it for the session `key()` names, either
    /// mirrored live or reconstructed by backfill.
    fn logged(seq: u64, bus_id: BusId, backfilled_from: Option<&str>, event: Event) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq,
            ts: Timestamp::now(),
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some("vendor-abc".into()),
                workspace: None,
                entrypoint: None,
                backfilled_from: backfilled_from.map(str::to_string),
                profile: None,
            },
            event,
        }
    }

    fn session_start() -> Event {
        Event::SessionCandidate {
            trigger: Some("startup".into()),
        }
    }

    fn prompt(text: &str) -> Event {
        Event::TurnStarted {
            author: None,
            prompt: Body::kept(text),
        }
    }

    #[test]
    fn restoring_from_the_log_keeps_a_vendor_session_on_its_conversation() {
        let bus_id = BusId::mint();
        let logged = logged(0, bus_id, None, prompt("before the restart"));

        let mut registry = Registry::restore(std::slice::from_ref(&logged));

        assert_eq!(registry.bus_id(&key()), Some(bus_id));
        assert_eq!(
            registry.observe(&key(), Observed::bare(prompt("after the restart"))),
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(prompt("after the restart"))],
            },
            "a restored session must not mint a second conversation"
        );
    }

    #[test]
    fn a_reconstructed_record_does_not_displace_a_live_binding() {
        // The twin defect, as it happened on a real log. A session was minted
        // live; then `backfill --apply`, which runs with the daemon stopped,
        // recovered the same transcript under the file's own UUID and appended
        // that copy *after* the live records. When the daemon came back,
        // "later record wins" bound the session to the copy, and everything
        // the session said next was written under the wrong conversation.
        // Two conversations on this machine were split that way before anyone
        // noticed. A reconstruction is the weaker fact, and it must never
        // outrank what the recorder saw for itself.
        let live = BusId::mint();
        let copy = BusId::mint();
        let records = vec![
            logged(0, live, None, prompt("seen live")),
            logged(
                1,
                copy,
                Some(CLAUDE_TRANSCRIPT),
                Event::SessionCandidate { trigger: None },
            ),
            logged(2, copy, Some(CLAUDE_TRANSCRIPT), prompt("seen live")),
        ];

        let registry = Registry::restore(&records);

        assert_eq!(registry.bus_id(&key()), Some(live));
    }

    #[test]
    fn restoring_one_record_at_a_time_binds_what_restore_binds() {
        // The daemon folds its registry from a streaming scan, not a collected
        // one. The same records handed over one at a time must bind the same
        // way, the rule that a live record outranks a reconstruction included.
        let live = BusId::mint();
        let copy = BusId::mint();
        let records = vec![
            logged(0, live, None, prompt("seen live")),
            logged(1, copy, Some(CLAUDE_TRANSCRIPT), prompt("seen live")),
        ];

        let mut restoring = Restoring::default();
        for record in &records {
            restoring.absorb(record);
        }
        let streamed = restoring.finish();

        assert_eq!(streamed.bus_id(&key()), Some(live));
        assert_eq!(
            streamed.bus_id(&key()),
            Registry::restore(&records).bus_id(&key())
        );
    }

    #[test]
    fn a_session_nothing_live_has_claimed_is_bound_to_its_reconstruction() {
        // The other half of the same rule, and the design working: backfill
        // adopts the transcript's own UUID as the `bus_id`, so a session that
        // resumes after its history was recovered continues the recovered
        // conversation instead of minting a second one.
        let copy = BusId::mint();
        let records = vec![logged(
            0,
            copy,
            Some(CLAUDE_TRANSCRIPT),
            prompt("recovered"),
        )];

        let registry = Registry::restore(&records);

        assert_eq!(registry.bus_id(&key()), Some(copy));
    }

    #[test]
    fn among_live_records_the_later_binding_still_wins() {
        // Precedence is about provenance, not position. Between two live
        // records the newer one keeps winning, because that is what a session
        // rebound while the daemon was up actually did.
        let first = BusId::mint();
        let second = BusId::mint();
        let records = vec![
            logged(0, first, None, prompt("first")),
            logged(1, second, None, prompt("second")),
        ];

        let registry = Registry::restore(&records);

        assert_eq!(registry.bus_id(&key()), Some(second));
    }

    #[test]
    fn a_session_that_kept_going_under_its_copy_stays_there() {
        // The shape the two split conversations on a real log have: minted
        // live as A, reconstructed as B by a backfill, then — after a restart
        // that predates the precedence rule — live again under B. The newest
        // live fact is B, and a restart must keep it there rather than send
        // the next turn back to A and fork the session a second time.
        let live = BusId::mint();
        let copy = BusId::mint();
        let records = vec![
            logged(0, live, None, prompt("first")),
            logged(1, copy, Some(CLAUDE_TRANSCRIPT), prompt("first")),
            logged(2, copy, None, prompt("second, after the restart")),
        ];

        let registry = Registry::restore(&records);

        assert_eq!(registry.bus_id(&key()), Some(copy));
    }

    #[test]
    fn a_record_with_no_vendor_session_binds_nothing() {
        let mut anonymous = logged(0, BusId::mint(), None, prompt("who?"));
        anonymous.source.vendor_session_id = None;

        let registry = Registry::restore(&[anonymous]);

        assert_eq!(registry.bus_id(&key()), None);
    }

    #[test]
    fn the_bound_is_large_enough_to_be_useless_to_hit_by_accident() {
        // Asserted as a literal because every other test here is written
        // against the constant and would pass at any value, including two.
        // Claude Code fires several session starts per project within a few
        // hundred milliseconds; a thousand is far past a real burst and still
        // a few hundred kilobytes at worst.
        assert_eq!(Registry::REMEMBERED_STARTS, 1024);
    }

    #[test]
    fn ghost_session_starts_do_not_accumulate_without_bound() {
        // Most session starts never mint — several fire per project within a
        // few hundred milliseconds and some have no transcript at all. The
        // daemon is supervised now and meant to run for weeks.
        let mut registry = Registry::new();

        for n in 0..(Registry::REMEMBERED_STARTS + 500) {
            let key = SessionKey::new(Harness::new(Harness::CLAUDE_CODE), format!("ghost-{n}"));
            registry.observe(&key, Observed::bare(session_start()));
        }

        assert_eq!(registry.remembered_starts(), Registry::REMEMBERED_STARTS);
    }

    #[test]
    fn forgetting_a_session_start_does_not_cost_its_conversation() {
        let mut registry = Registry::new();
        registry.observe(&key(), Observed::bare(session_start()));
        for n in 0..(Registry::REMEMBERED_STARTS + 1) {
            let other = SessionKey::new(Harness::new(Harness::CLAUDE_CODE), format!("ghost-{n}"));
            registry.observe(&other, Observed::bare(session_start()));
        }

        // The remembered start is gone, but the prompt still mints and is still
        // written — only the retroactive session_candidate record is lost.
        let disposition = registry.observe(&key(), Observed::bare(prompt("late")));

        let Disposition::Write { observed, .. } = disposition else {
            panic!("a prompt must still mint, got {disposition:?}");
        };
        assert_eq!(observed, vec![Observed::bare(prompt("late"))]);
    }

    #[test]
    fn a_session_start_alone_mints_nothing() {
        let mut registry = Registry::new();

        assert_eq!(
            registry.observe(&key(), Observed::bare(session_start())),
            Disposition::Deferred
        );
        assert_eq!(registry.bus_id(&key()), None, "a ghost must not get an id");
    }

    #[test]
    fn a_cursor_tool_call_mints_the_conversation_its_prompt_never_reached() {
        // Measured 2026-09-08: 4,229 orphan drops since 09-05 carried
        // `preToolUse` / `postToolUse` for 42 Cursor conversation ids that
        // appear nowhere in the log, under any harness. Cursor's tool hooks
        // arrive through Claude Code's settings file; `beforeSubmitPrompt`
        // and `stop` come from Cursor's own hooks file and never fired for
        // these — so every one of those conversations was thrown away whole.
        // A tool call proves a session is real for the same reason a
        // completed turn does, which this module already accepts.
        let key = SessionKey::new(Harness::new(Harness::CURSOR), "worker-conv");
        let mut registry = Registry::new();
        let requested = Event::ToolRequested {
            tool: "Read".into(),
            tool_use_id: None,
            input: Body::kept("{}"),
        };

        let Disposition::Write { bus_id, .. } =
            registry.observe(&key, Observed::bare(requested.clone()))
        else {
            panic!("a live Cursor tool call must mint");
        };

        assert_eq!(registry.bus_id(&key), Some(bus_id));
        assert_eq!(
            registry.observe(&key, Observed::bare(requested)),
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(Event::ToolRequested {
                    tool: "Read".into(),
                    tool_use_id: None,
                    input: Body::kept("{}"),
                })],
            },
            "the second call joins the conversation the first one minted"
        );
    }

    #[test]
    fn only_cursor_mints_on_a_tool_call() {
        // The rule is Cursor's alone. Claude Code fires ghost session starts
        // and a tool call under an unbound Claude session is the ordinary
        // shape of a daemon restart mid-turn, which must stay an orphan.
        let mut registry = Registry::new();
        let requested = Event::ToolRequested {
            tool: "Read".into(),
            tool_use_id: None,
            input: Body::kept("{}"),
        };

        assert_eq!(
            registry.observe(&key(), Observed::bare(requested)),
            Disposition::Orphaned
        );
        assert_eq!(registry.bus_id(&key()), None);
    }

    #[test]
    fn a_cursor_session_that_only_ends_is_still_an_orphan() {
        // A `sessionEnd` with nothing before it proves a session existed, not
        // that a conversation happened. Minting there would put ghosts in the
        // log, which is what the deferral rule exists to prevent.
        let key = SessionKey::new(Harness::new(Harness::CURSOR), "closed-empty");
        let mut registry = Registry::new();

        assert_eq!(
            registry.observe(&key, Observed::bare(Event::SessionEnded { reason: None })),
            Disposition::Orphaned
        );
    }

    #[test]
    fn a_cursor_completed_turn_mints_when_the_prompt_was_never_seen() {
        let key = SessionKey::new(Harness::new(Harness::CURSOR), "cursor-conv");
        let mut registry = Registry::new();
        let ended = Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: None,
            tokens: None,
        };

        let Disposition::Write { bus_id, .. } =
            registry.observe(&key, Observed::bare(ended.clone()))
        else {
            panic!("a live Cursor turn must mint");
        };
        assert_eq!(registry.bus_id(&key), Some(bus_id));
        assert_eq!(
            registry.observe(&key, Observed::bare(ended)),
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                    tokens: None,
                })],
            }
        );
    }

    #[test]
    fn the_first_prompt_mints_the_id_and_flushes_the_remembered_session_start() {
        let mut registry = Registry::new();
        registry.observe(&key(), Observed::bare(session_start()));

        let disposition = registry.observe(&key(), Observed::bare(prompt("ship the recorder")));

        let Disposition::Write { bus_id, observed } = disposition else {
            panic!("the first prompt must mint, got {disposition:?}");
        };
        assert_eq!(registry.bus_id(&key()), Some(bus_id));
        assert_eq!(
            observed,
            vec![
                Observed::bare(session_start()),
                Observed::bare(prompt("ship the recorder")),
            ],
            "the session start must reach the log behind the prompt that proved it real"
        );
    }

    #[test]
    fn later_events_in_the_same_session_reuse_the_id_and_flush_nothing_extra() {
        let mut registry = Registry::new();
        registry.observe(&key(), Observed::bare(session_start()));
        let Disposition::Write { bus_id, .. } =
            registry.observe(&key(), Observed::bare(prompt("first")))
        else {
            panic!("the first prompt must mint");
        };

        let second = registry.observe(&key(), Observed::bare(prompt("second")));
        let ended = registry.observe(
            &key(),
            Observed::bare(Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: None,
                tokens: None,
            }),
        );

        assert_eq!(
            second,
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(prompt("second"))]
            }
        );
        assert_eq!(
            ended,
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                    tokens: None,
                })],
            }
        );
    }

    #[test]
    fn a_resume_on_a_bound_session_is_logged_rather_than_held_as_a_candidate() {
        // `SessionStart` fires again on `--resume`, `compact`, and `fork`. That
        // session is already proven real, so there is nothing to defer: holding
        // it as a candidate loses the record and leaks the entry.
        let mut registry = Registry::new();
        let Disposition::Write { bus_id, .. } =
            registry.observe(&key(), Observed::bare(prompt("first")))
        else {
            panic!("the first prompt must mint");
        };
        let resumed = Event::SessionCandidate {
            trigger: Some("resume".into()),
        };

        let disposition = registry.observe(&key(), Observed::bare(resumed.clone()));

        assert_eq!(
            disposition,
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(resumed)]
            }
        );
    }

    #[test]
    fn a_prompt_mints_even_when_the_session_start_was_never_seen() {
        // Claude Code Desktop's `/clear` omits `source` (#76704), and a hook can
        // be registered mid-session. A missing session start must not cost us
        // the conversation.
        let mut registry = Registry::new();

        let disposition =
            registry.observe(&key(), Observed::bare(prompt("no session start arrived")));

        let Disposition::Write { bus_id, observed } = disposition else {
            panic!("a prompt must mint on its own, got {disposition:?}");
        };
        assert_eq!(registry.bus_id(&key()), Some(bus_id));
        assert_eq!(
            observed,
            vec![Observed::bare(prompt("no session start arrived"))]
        );
    }

    #[test]
    fn an_event_that_cannot_start_a_conversation_is_reported_not_dropped() {
        let mut registry = Registry::new();

        let disposition = registry.observe(
            &key(),
            Observed::bare(Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: None,
                tokens: None,
            }),
        );

        assert_eq!(disposition, Disposition::Orphaned);
        assert_eq!(registry.bus_id(&key()), None);
    }

    #[test]
    fn a_warm_thread_the_bus_opened_keeps_the_predecessors_id() {
        let mut registry = Registry::new();
        let claude = key();
        let Disposition::Write { bus_id, .. } =
            registry.observe(&claude, Observed::bare(prompt("work")))
        else {
            panic!("mint");
        };

        registry.observe(
            &claude,
            Observed::bare(Event::Leased {
                holder: "warm:thr_bus".into(),
            }),
        );

        let codex = SessionKey::new(Harness::new(Harness::CODEX), "thr_bus");
        assert_eq!(
            registry.observe(&codex, Observed::bare(prompt("continue"))),
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(prompt("continue"))],
            }
        );
    }

    #[test]
    fn restoring_a_warm_lease_binds_the_codex_thread() {
        let bus_id = BusId::mint();
        let leased = logged(
            0,
            bus_id,
            None,
            Event::Leased {
                holder: "warm:thr_bus".into(),
            },
        );

        let mut registry = Registry::restore(std::slice::from_ref(&leased));

        let codex = SessionKey::new(Harness::new(Harness::CODEX), "thr_bus");
        assert_eq!(registry.bus_id(&codex), Some(bus_id));
        assert_eq!(
            registry.observe(&codex, Observed::bare(prompt("after restart"))),
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(prompt("after restart"))],
            }
        );
    }

    #[test]
    fn a_warm_alias_does_not_steal_a_live_codex_session() {
        let own = BusId::mint();
        let other = BusId::mint();
        let mut claimed = logged(0, own, None, prompt("already this conversation"));
        claimed.source.harness = Harness::new(Harness::CODEX);
        claimed.source.vendor_session_id = Some("thr_bus".into());
        let mut leased = logged(
            1,
            other,
            None,
            Event::Leased {
                holder: "warm:thr_bus".into(),
            },
        );
        leased.source.vendor_session_id = Some("claude-1".into());

        let registry = Registry::restore(&[claimed, leased]);

        assert_eq!(
            registry.bus_id(&SessionKey::new(Harness::new(Harness::CODEX), "thr_bus")),
            Some(own)
        );
    }

    #[test]
    fn a_claude_warm_session_the_bus_opened_keeps_the_predecessors_id() {
        let mut registry = Registry::new();
        let claude = key();
        let Disposition::Write { bus_id, .. } =
            registry.observe(&claude, Observed::bare(prompt("work")))
        else {
            panic!("mint");
        };

        registry.observe(
            &claude,
            Observed::bare(Event::Leased {
                holder: "warm-claude:sess_bus".into(),
            }),
        );

        let resumed = SessionKey::new(Harness::new(Harness::CLAUDE_CODE), "sess_bus");
        assert_eq!(
            registry.observe(&resumed, Observed::bare(prompt("continue"))),
            Disposition::Write {
                bus_id,
                observed: vec![Observed::bare(prompt("continue"))],
            }
        );
        assert_eq!(
            Event::Leased {
                holder: "warm-claude:sess_bus".into(),
            }
            .warm_thread(),
            None,
            "warm-claude: must not look like a Codex thread"
        );
    }

    #[test]
    fn a_holder_that_is_not_a_warm_thread_aliases_nothing() {
        let mut registry = Registry::new();
        let Disposition::Write { bus_id, .. } =
            registry.observe(&key(), Observed::bare(prompt("work")))
        else {
            panic!("mint");
        };
        registry.observe(
            &key(),
            Observed::bare(Event::Leased {
                holder: "conbus-hop".into(),
            }),
        );

        let stray = SessionKey::new(Harness::new(Harness::CODEX), "conbus-hop");
        let Disposition::Write { bus_id: minted, .. } =
            registry.observe(&stray, Observed::bare(prompt("unrelated")))
        else {
            panic!("must mint");
        };
        assert_ne!(minted, bus_id);
    }

    #[test]
    fn two_vendor_sessions_get_two_conversations() {
        let mut registry = Registry::new();
        let other = SessionKey::new(Harness::new(Harness::CLAUDE_CODE), "vendor-xyz");

        let Disposition::Write { bus_id: first, .. } =
            registry.observe(&key(), Observed::bare(prompt("a")))
        else {
            panic!("mint");
        };
        let Disposition::Write { bus_id: second, .. } =
            registry.observe(&other, Observed::bare(prompt("b")))
        else {
            panic!("mint");
        };

        assert_ne!(first, second);
    }

    #[test]
    fn the_same_session_id_under_two_harnesses_is_two_conversations() {
        let mut registry = Registry::new();
        let codex = SessionKey::new(Harness::new(Harness::CODEX), "vendor-abc");

        let Disposition::Write { bus_id: claude, .. } =
            registry.observe(&key(), Observed::bare(prompt("a")))
        else {
            panic!("mint");
        };
        let Disposition::Write {
            bus_id: codex_id, ..
        } = registry.observe(&codex, Observed::bare(prompt("b")))
        else {
            panic!("mint");
        };

        assert_ne!(
            claude, codex_id,
            "vendor session ids are only unique per vendor"
        );
    }

    #[test]
    fn two_conversations_directed_at_one_leg_in_one_checkout_seat_neither() {
        // Parallel sessions or worktrees in one repo make this ordinary. A
        // plain insert let the second hop take the first's seat with no
        // trace, and the next Cursor prompt then bound to the wrong
        // conversation — splitting the first a second time, in a log that
        // cannot be rewritten. A fresh id is a recoverable disappointment;
        // the wrong id is not.
        let mut registry = Registry::new();
        let first = key();
        let second = SessionKey::new(Harness::new(Harness::CLAUDE_CODE), "vendor-def");
        let Disposition::Write { bus_id: a, .. } =
            registry.observe(&first, Observed::bare(prompt("work")))
        else {
            panic!("mint");
        };
        let Disposition::Write { bus_id: b, .. } =
            registry.observe(&second, Observed::bare(prompt("other work")))
        else {
            panic!("mint");
        };
        for who in [&first, &second] {
            let mut routed = Observed::bare(Event::Routed {
                next: Harness::CURSOR.into(),
                hop: crate::event::Hop::HandedOff,
                reason: "directed".into(),
                code_freshness: None,
                provenance: None,
            });
            routed.workspace = Some("/repo".into());
            registry.observe(who, routed);
        }

        let cursor = SessionKey::new(Harness::new(Harness::CURSOR), "composer-1");
        let mut arriving = Observed::bare(prompt("continue"));
        arriving.workspace = Some("/repo".into());
        let Disposition::Write { bus_id: seated, .. } = registry.observe(&cursor, arriving) else {
            panic!("mint");
        };

        assert_ne!(seated, a, "a contested seat must not hand out the first");
        assert_ne!(seated, b, "nor the second");
    }

    #[test]
    fn one_conversation_hopping_twice_keeps_its_seat() {
        // The companion guard: the contested rule must not fire on a
        // conversation that simply hops to the same leg again, which would
        // silently disable directed hops for everybody.
        let mut registry = Registry::new();
        let claude = key();
        let Disposition::Write { bus_id, .. } =
            registry.observe(&claude, Observed::bare(prompt("work")))
        else {
            panic!("mint");
        };
        for _ in 0..2 {
            let mut routed = Observed::bare(Event::Routed {
                next: Harness::CURSOR.into(),
                hop: crate::event::Hop::HandedOff,
                reason: "directed".into(),
                code_freshness: None,
                provenance: None,
            });
            routed.workspace = Some("/repo".into());
            registry.observe(&claude, routed);
        }

        let cursor = SessionKey::new(Harness::new(Harness::CURSOR), "composer-2");
        let mut arriving = Observed::bare(prompt("continue"));
        arriving.workspace = Some("/repo".into());
        let Disposition::Write { bus_id: seated, .. } = registry.observe(&cursor, arriving) else {
            panic!("mint");
        };

        assert_eq!(seated, bus_id, "its own second hop is not a contest");
    }

    #[test]
    fn a_directed_hop_seat_is_claimed_by_the_next_prompt_in_that_checkout() {
        let mut registry = Registry::new();
        let claude = key();
        let Disposition::Write { bus_id, .. } =
            registry.observe(&claude, Observed::bare(prompt("work")))
        else {
            panic!("mint");
        };

        let mut routed = Observed::bare(Event::Routed {
            next: Harness::CURSOR.into(),
            hop: crate::event::Hop::HandedOff,
            reason: "directed".into(),
            code_freshness: None,
            provenance: None,
        });
        routed.workspace = Some("/repo".into());
        registry.observe(&claude, routed);

        let cursor = SessionKey::new(Harness::new(Harness::CURSOR), "new-composer");
        let mut first = Observed::bare(prompt("continue"));
        first.workspace = Some("/repo/".into());
        let Disposition::Write {
            bus_id: claimed, ..
        } = registry.observe(&cursor, first)
        else {
            panic!("claim");
        };
        assert_eq!(
            claimed, bus_id,
            "the new Cursor session is this conversation"
        );

        let other = SessionKey::new(Harness::new(Harness::CURSOR), "other-folder");
        let mut elsewhere = Observed::bare(prompt("unrelated"));
        elsewhere.workspace = Some("/else".into());
        let Disposition::Write { bus_id: minted, .. } = registry.observe(&other, elsewhere) else {
            panic!("mint");
        };
        assert_ne!(
            minted, bus_id,
            "a different checkout is a different conversation"
        );
    }

    #[test]
    fn a_restored_hop_seat_survives_a_daemon_restart() {
        let bus_id = BusId::mint();
        let mut routed = logged(
            1,
            bus_id,
            None,
            Event::Routed {
                next: Harness::CURSOR.into(),
                hop: crate::event::Hop::HandedOff,
                reason: "directed".into(),
                code_freshness: None,
                provenance: None,
            },
        );
        routed.source.workspace = Some("/repo".into());
        let registry = Registry::restore(std::slice::from_ref(&routed));

        let cursor = SessionKey::new(Harness::new(Harness::CURSOR), "after-restart");
        let mut first = Observed::bare(prompt("continue"));
        first.workspace = Some("/repo".into());
        let mut registry = registry;
        let Disposition::Write {
            bus_id: claimed, ..
        } = registry.observe(&cursor, first)
        else {
            panic!("claim");
        };
        assert_eq!(claimed, bus_id);
    }
}
