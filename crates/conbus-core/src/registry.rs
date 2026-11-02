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

use crate::clock::Timestamp;
use crate::event::{Event, Harness, VendorAgentId, VendorTurnId};
use crate::ids::BusId;
use crate::record::Record;

/// A vendor session, namespaced by the harness that named it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
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
#[derive(Clone, PartialEq, Eq, Debug)]
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Seat {
    Claimed(BusId),
    Contested,
}

/// How long a seat waits for its prompt, from the hop that reserved it: the limit
/// a spooled event gets while it waits for the same thing. A hop nobody follows up
/// must not bind an unrelated prompt days later, and that binding would be for good.
const SEAT_LIFE_SECS: i64 = 24 * 60 * 60;

/// A seat, and when the hop that reserved it happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Waiting {
    seat: Seat,
    since: Timestamp,
}

impl Waiting {
    fn is_over(&self, at: Timestamp) -> bool {
        at.seconds_since(self.since) >= SEAT_LIFE_SECS
    }
}

/// The vendor-session-to-conversation map. Lives in the daemon's memory.
#[derive(Default)]
pub struct Registry {
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
    candidates: HashMap<SessionKey, (u64, Observed)>,
    remembered: u64,
    /// A hop to a catalog leg that cannot keep a warm session. The next
    /// unbound prompt in that checkout claims this conversation.
    pending: HashMap<(String, String), Waiting>,
    /// Sessions the log holds more for than a flushed session start. What comes
    /// first after that start is what minted them.
    spoken: HashSet<SessionKey>,
}

/// A registry being rebuilt from the log, one record at a time.
///
/// [`Registry::restore`] folds a collected slice with it. The daemon folds a
/// streaming scan instead, so that rebuilding its state at start does not cost
/// as much memory as the log itself.
#[derive(Default)]
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
        self.registry.note_written(record);
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
            .retain(|_, waiting| !matches!(waiting.seat, Seat::Claimed(id) if id == bus_id));
    }

    /// What a record does to the seats, once it is in the log.
    ///
    /// The one definition, called for each record a daemon writes and for each one
    /// a restart reads back, so the two cannot disagree. A hop the log refused never
    /// gets here, so it costs nobody a seat, and a seat's day runs from the same
    /// `ts` either way, so a restart gives it no new one.
    pub fn note_written(&mut self, record: &Record) {
        let Some(session) = &record.source.vendor_session_id else {
            return;
        };
        self.note_route(record);
        // The leg has arrived, so this conversation is waiting for nobody there.
        if matches!(
            record.event,
            Event::TurnStarted { .. } | Event::TurnEnded { .. }
        ) {
            self.pending.retain(|(harness, _), waiting| {
                !(matches!(waiting.seat, Seat::Claimed(id) if id == record.bus_id)
                    && harness == record.source.harness.as_str())
            });
        }
        if matches!(record.event, Event::SessionCandidate { .. }) {
            return;
        }
        // A session's first record past any start it flushed is what minted it,
        // and a mint uses up the seat in its checkout: see `observe`.
        let key = SessionKey::new(record.source.harness.clone(), session.clone());
        let minted = matches!(record.event, Event::TurnStarted { .. })
            || Self::cursor_proof(&key, &record.event);
        if self.spoken.insert(key) && minted && record.source.backfilled_from.is_none() {
            self.release_seat(&record.source.harness, record.source.workspace.as_deref());
        }
    }

    fn note_route(&mut self, record: &Record) {
        let Event::Routed { next, .. } = &record.event else {
            return;
        };
        let (bus_id, at) = (record.bus_id, record.ts);
        self.clear_pending_bus(bus_id);
        if !Self::needs_seat(next) {
            return;
        }
        let Some(workspace) = record
            .source
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
        let claimed = Waiting {
            seat: Seat::Claimed(bus_id),
            since: at,
        };
        match self.pending.entry((next.clone(), workspace)) {
            Entry::Vacant(seat) => {
                seat.insert(claimed);
            }
            // A seat whose day is over holds nobody, and contests nothing.
            Entry::Occupied(mut seat) if seat.get().is_over(at) => {
                seat.insert(claimed);
            }
            Entry::Occupied(mut seat) => {
                if !matches!(seat.get().seat, Seat::Claimed(id) if id == bus_id) {
                    seat.insert(Waiting {
                        seat: Seat::Contested,
                        since: at,
                    });
                }
            }
        }
    }

    fn release_seat(&mut self, harness: &Harness, workspace: Option<&str>) {
        let _ = self.take_seat(harness, workspace);
    }

    /// Work a Cursor session can be seen doing, which proves it real enough to
    /// mint without a prompt. See [`Registry::observe`].
    fn cursor_proof(key: &SessionKey, event: &Event) -> bool {
        key.harness.as_str() == Harness::CURSOR
            && matches!(
                event,
                Event::TurnEnded { .. } | Event::ToolRequested { .. } | Event::ToolUse { .. }
            )
    }

    fn take_seat(&mut self, harness: &Harness, workspace: Option<&str>) -> Option<BusId> {
        let workspace = Self::normalize_workspace(workspace?)?;
        let waiting = self
            .pending
            .remove(&(harness.as_str().to_string(), workspace))?;
        match waiting.seat {
            Seat::Claimed(bus_id) if !waiting.is_over(Timestamp::now()) => Some(bus_id),
            Seat::Claimed(_) | Seat::Contested => None,
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
        let prompted = matches!(observed.event, Event::TurnStarted { .. });
        if !prompted && !Self::cursor_proof(key, &observed.event) {
            return Disposition::Orphaned;
        }
        // A directed hop to a leg without a bus-owned warm has no vendor
        // session yet. The next real prompt in that checkout sits on the
        // conversation that was handed off, instead of minting a twin.
        //
        // A prompt, and nothing else. Cursor's proof above says a session is
        // real, not that it is the one the person sent here: a tool call from
        // a session already at work in that checkout would take the seat, and
        // the binding would outlive every restart.
        //
        // It does not leave the seat waiting either. If this is the session the
        // hop was for, its prompt hook did not fire, and whoever prompts next in
        // this checkout is a stranger who would sit on the handed-off conversation
        // for good. So the seat goes, and both hold fresh ids. A fresh id is a
        // recoverable disappointment; the wrong one is not.
        let seat = if prompted {
            self.take_seat(&key.harness, observed.workspace.as_deref())
        } else {
            self.release_seat(&key.harness, observed.workspace.as_deref());
            None
        };
        let bus_id = seat.unwrap_or_else(BusId::mint);
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
            landed(&mut registry, who, routed);
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

    /// A hop to Cursor in `/repo`, seen from the conversation it leaves, and a
    /// Cursor session nobody has bound yet.
    /// A record as the log holds what `session` said under this conversation.
    fn as_written(session: &SessionKey, bus_id: BusId, one: Observed) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 0,
            ts: Timestamp::now(),
            bus_id,
            vendor_turn_id: one.vendor_turn_id,
            vendor_agent_id: one.vendor_agent_id,
            hook_id: None,
            spooled_at: None,
            source: Source {
                harness: session.harness.clone(),
                harness_version: None,
                vendor_session_id: Some(session.vendor_session_id.clone()),
                workspace: one.workspace,
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            },
            event: one.event,
        }
    }

    /// What the daemon does with an event: observe it, write what that says, and
    /// tell the registry what reached the log. Returns what was written.
    fn landed_records(
        registry: &mut Registry,
        session: &SessionKey,
        observed: Observed,
    ) -> Vec<Record> {
        let Disposition::Write { bus_id, observed } = registry.observe(session, observed) else {
            return Vec::new();
        };
        let written: Vec<Record> = observed
            .into_iter()
            .map(|one| as_written(session, bus_id, one))
            .collect();
        for record in &written {
            registry.note_written(record);
        }
        written
    }

    fn landed(registry: &mut Registry, session: &SessionKey, observed: Observed) -> BusId {
        landed_records(registry, session, observed)
            .last()
            .expect("this one writes")
            .bus_id
    }

    fn a_seat_waiting_in_the_checkout(registry: &mut Registry) -> (BusId, SessionKey) {
        let Disposition::Write { bus_id, .. } =
            registry.observe(&key(), Observed::bare(prompt("work")))
        else {
            panic!("mint");
        };
        let mut routed = Observed::bare(Event::Routed {
            next: Harness::CURSOR.into(),
            hop: crate::event::Hop::HandedOff,
            reason: "ordered".into(),
            code_freshness: None,
            provenance: None,
        });
        routed.workspace = Some("/repo".into());
        landed(registry, &key(), routed);
        (
            bus_id,
            SessionKey::new(Harness::new(Harness::CURSOR), "someone-else"),
        )
    }

    #[test]
    fn a_strangers_tool_call_does_not_take_a_seat_meant_for_the_next_prompt() {
        // A directed hop reserves a seat for the next prompt in a checkout. A
        // Cursor session already working there is not that prompt, and the
        // binding it took would survive every restart after it.
        let mut registry = Registry::new();
        let (handed_off, stranger) = a_seat_waiting_in_the_checkout(&mut registry);
        let mut tool = Observed::bare(Event::ToolUse {
            tool: "Read".into(),
            tool_use_id: None,
            duration_ms: None,
            input: Body::kept("i"),
            outcome: crate::event::ToolOutcome::Succeeded {
                output: Body::kept("o"),
            },
        });
        tool.workspace = Some("/repo".into());

        let Disposition::Write { bus_id, .. } = registry.observe(&stranger, tool) else {
            panic!("a Cursor tool call mints");
        };

        assert_ne!(bus_id, handed_off, "a tool call took the seat");
    }

    #[test]
    fn a_strangers_ended_turn_does_not_take_a_seat_either() {
        let mut registry = Registry::new();
        let (handed_off, stranger) = a_seat_waiting_in_the_checkout(&mut registry);
        let mut ended = Observed::bare(Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: None,
            tokens: None,
        });
        ended.workspace = Some("/repo".into());

        let Disposition::Write { bus_id, .. } = registry.observe(&stranger, ended) else {
            panic!("a Cursor completed turn mints");
        };

        assert_ne!(bus_id, handed_off, "a completed turn took the seat");
    }

    /// The next prompt from a Cursor session nobody has seen in this checkout.
    fn the_next_prompt_in_the_checkout(registry: &mut Registry, session: &str) -> BusId {
        let key = SessionKey::new(Harness::new(Harness::CURSOR), session);
        let mut first = Observed::bare(prompt("unrelated work"));
        first.workspace = Some("/repo".into());
        landed(registry, &key, first)
    }

    fn a_tool_call_in_the_checkout() -> Observed {
        let mut tool = Observed::bare(Event::ToolRequested {
            tool: "Read".into(),
            tool_use_id: None,
            input: Body::kept("{}"),
        });
        tool.workspace = Some("/repo".into());
        tool
    }

    #[test]
    fn a_seat_the_handed_off_sessions_own_tool_call_passed_over_is_not_taken_by_a_stranger() {
        // The session the hop was for opens and its prompt hook does not fire, as
        // Cursor's sometimes does not. Its first tool call mints. If the seat stayed,
        // the next prompt in this checkout, from anyone at any time, would sit on
        // the handed-off conversation for good.
        let mut registry = Registry::new();
        let (handed_off, arrived) = a_seat_waiting_in_the_checkout(&mut registry);
        let Disposition::Write { bus_id: minted, .. } =
            registry.observe(&arrived, a_tool_call_in_the_checkout())
        else {
            panic!("a tool call mints");
        };

        let stranger = the_next_prompt_in_the_checkout(&mut registry, "a-stranger-days-later");

        assert_ne!(minted, handed_off, "a tool call took the seat");
        assert_ne!(stranger, handed_off, "a stranger took the seat");
        assert_ne!(stranger, minted, "and they are not each other");
    }

    #[test]
    fn a_completed_turn_gives_up_the_seat_the_same_way() {
        let mut registry = Registry::new();
        let (handed_off, arrived) = a_seat_waiting_in_the_checkout(&mut registry);
        let mut ended = Observed::bare(Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: None,
            tokens: None,
        });
        ended.workspace = Some("/repo".into());
        registry.observe(&arrived, ended);

        let stranger = the_next_prompt_in_the_checkout(&mut registry, "a-stranger-days-later");

        assert_ne!(stranger, handed_off);
    }

    #[test]
    fn a_tool_call_from_a_session_already_bound_leaves_the_seat_alone() {
        // It minted nothing, and says nothing about who the hop was for.
        let mut registry = Registry::new();
        let at_work = SessionKey::new(Harness::new(Harness::CURSOR), "already-at-work");
        let mut before = Observed::bare(prompt("earlier"));
        before.workspace = Some("/repo".into());
        registry.observe(&at_work, before);
        let (handed_off, _) = a_seat_waiting_in_the_checkout(&mut registry);

        registry.observe(&at_work, a_tool_call_in_the_checkout());

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut registry, "the-handed-off-one"),
            handed_off
        );
    }

    #[test]
    fn a_tool_call_in_another_checkout_leaves_the_seat_alone() {
        let mut registry = Registry::new();
        let (handed_off, elsewhere) = a_seat_waiting_in_the_checkout(&mut registry);
        let mut tool = a_tool_call_in_the_checkout();
        tool.workspace = Some("/another-repo".into());

        registry.observe(&elsewhere, tool);

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut registry, "the-handed-off-one"),
            handed_off
        );
    }

    /// A record of a Cursor session, as the log holds it.
    fn cursor_logged(seq: u64, bus_id: BusId, session: &str, event: Event) -> Record {
        let mut record = logged(seq, bus_id, None, event);
        record.source.harness = Harness::new(Harness::CURSOR);
        record.source.vendor_session_id = Some(session.into());
        record.source.workspace = Some("/repo".into());
        record
    }

    fn a_routed_record(bus_id: BusId) -> Record {
        let mut routed = logged(
            0,
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
        routed
    }

    fn a_tool_event() -> Event {
        Event::ToolRequested {
            tool: "Read".into(),
            tool_use_id: None,
            input: Body::kept("{}"),
        }
    }

    #[test]
    fn a_seat_given_up_stays_given_up_after_a_restart() {
        // The seat comes back from its `routed` record, so the mint that gave it
        // up has to be read back too, or a restart hands the stranger the seat.
        let handed_off = BusId::mint();
        let minted = BusId::mint();
        let records = [
            a_routed_record(handed_off),
            cursor_logged(1, minted, "arrived", a_tool_event()),
        ];

        let mut registry = Registry::restore(&records);

        assert_ne!(
            the_next_prompt_in_the_checkout(&mut registry, "a-stranger"),
            handed_off
        );
    }

    #[test]
    fn a_seat_given_up_behind_a_flushed_session_start_stays_given_up_too() {
        // What the log really holds when Cursor's start hook did fire: the mint
        // writes that start first, then the proof.
        let handed_off = BusId::mint();
        let minted = BusId::mint();
        let records = [
            a_routed_record(handed_off),
            cursor_logged(1, minted, "arrived", session_start()),
            cursor_logged(2, minted, "arrived", a_tool_event()),
        ];

        let mut registry = Registry::restore(&records);

        assert_ne!(
            the_next_prompt_in_the_checkout(&mut registry, "a-stranger"),
            handed_off
        );
    }

    #[test]
    fn a_restart_leaves_the_seat_a_bound_sessions_tool_call_left() {
        // Replaying the release has to mean the same mint the live path saw, and a
        // tool call from a session a prompt already bound is not one.
        let handed_off = BusId::mint();
        let at_work = BusId::mint();
        let records = [
            cursor_logged(0, at_work, "at-work", prompt("earlier")),
            a_routed_record(handed_off),
            cursor_logged(2, at_work, "at-work", a_tool_event()),
        ];

        let mut registry = Registry::restore(&records);

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut registry, "the-handed-off-one"),
            handed_off
        );
    }

    #[test]
    fn a_restart_leaves_the_seat_a_recovered_tool_call_left() {
        // Gap fill wrote this later. It says nothing about who came after the hop.
        let handed_off = BusId::mint();
        let recovered = BusId::mint();
        let mut filled = cursor_logged(1, recovered, "recovered", a_tool_event());
        filled.source.backfilled_from = Some(CLAUDE_TRANSCRIPT.into());
        let records = [a_routed_record(handed_off), filled];

        let mut registry = Registry::restore(&records);

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut registry, "the-handed-off-one"),
            handed_off
        );
    }

    #[test]
    fn the_live_path_and_a_restart_agree_on_a_seat_given_up() {
        // The records a live registry's own dispositions would write, read back.
        let mut live = Registry::new();
        let (handed_off, arrived) = a_seat_waiting_in_the_checkout(&mut live);
        let mut start = Observed::bare(session_start());
        start.workspace = Some("/repo".into());
        assert_eq!(live.observe(&arrived, start), Disposition::Deferred);
        let written = landed_records(&mut live, &arrived, a_tool_call_in_the_checkout());
        assert_eq!(written.len(), 2, "the start it flushed, then the proof");
        let mut records = vec![a_routed_record(handed_off)];
        records.extend(written);

        let mut restored = Registry::restore(&records);

        assert_ne!(
            the_next_prompt_in_the_checkout(&mut restored, "a-stranger"),
            handed_off
        );
        assert_ne!(
            the_next_prompt_in_the_checkout(&mut live, "a-stranger"),
            handed_off
        );
    }

    fn hours_ago(hours: i64) -> Timestamp {
        let epoch = Timestamp::from_unix_seconds(0).expect("the epoch");
        Timestamp::from_unix_seconds(Timestamp::now().seconds_since(epoch) - hours * 3600)
            .expect("a time")
    }

    #[test]
    fn a_seat_nobody_takes_within_a_day_is_gone() {
        // On `main` a hop nobody follows up held its seat for ever, and an unrelated
        // prompt days later bound to it for good.
        let mut registry = Registry::new();
        let (handed_off, _) = a_seat_waiting_in_the_checkout(&mut registry);
        for waiting in registry.pending.values_mut() {
            waiting.since = hours_ago(25);
        }

        assert_ne!(
            the_next_prompt_in_the_checkout(&mut registry, "a-stranger-days-later"),
            handed_off
        );
    }

    #[test]
    fn a_seat_inside_its_day_is_still_there_for_the_prompt() {
        let mut registry = Registry::new();
        let (handed_off, _) = a_seat_waiting_in_the_checkout(&mut registry);
        for waiting in registry.pending.values_mut() {
            waiting.since = hours_ago(23);
        }

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut registry, "the-handed-off-one"),
            handed_off
        );
    }

    #[test]
    fn a_restart_does_not_give_a_seat_a_new_day() {
        // Its life runs from the `routed` record, not from when the log was read.
        let handed_off = BusId::mint();
        let mut routed = a_routed_record(handed_off);
        routed.ts = hours_ago(25);

        let mut registry = Registry::restore(std::slice::from_ref(&routed));

        assert_ne!(
            the_next_prompt_in_the_checkout(&mut registry, "a-stranger-days-later"),
            handed_off
        );
    }

    #[test]
    fn a_restart_keeps_what_is_left_of_a_seats_day() {
        let handed_off = BusId::mint();
        let mut routed = a_routed_record(handed_off);
        routed.ts = hours_ago(23);

        let mut registry = Registry::restore(std::slice::from_ref(&routed));

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut registry, "the-handed-off-one"),
            handed_off
        );
    }

    #[test]
    fn a_seat_whose_day_is_over_does_not_contest_the_next_hop() {
        // Left there it would turn the new hop's seat into one nobody can take.
        let old = BusId::mint();
        let new = BusId::mint();
        let mut abandoned = a_routed_record(old);
        abandoned.ts = hours_ago(25);
        let records = [abandoned, a_routed_record(new)];

        let mut registry = Registry::restore(&records);

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut registry, "the-handed-off-one"),
            new
        );
    }

    #[test]
    fn a_proof_behind_a_start_that_landed_alone_releases_the_seat_live_too() {
        // A flushed start landed, its proof did not, and the daemon restarted. The
        // drained proof finds the session bound, so it mints nothing, and the next
        // replay would still call it the mint. What was written decides, both ways.
        let handed_off = BusId::mint();
        let minted = BusId::mint();
        let started = [
            a_routed_record(handed_off),
            cursor_logged(1, minted, "arrived", session_start()),
        ];
        let mut live = Registry::restore(&started);
        let arrived = SessionKey::new(Harness::new(Harness::CURSOR), "arrived");
        landed(&mut live, &arrived, a_tool_call_in_the_checkout());
        let mut replayed = Registry::restore(&[
            a_routed_record(handed_off),
            cursor_logged(1, minted, "arrived", session_start()),
            cursor_logged(2, minted, "arrived", a_tool_event()),
        ]);

        assert_ne!(
            the_next_prompt_in_the_checkout(&mut replayed, "a-stranger"),
            handed_off,
            "replay"
        );
        assert_ne!(
            the_next_prompt_in_the_checkout(&mut live, "a-stranger"),
            handed_off,
            "live: a stranger took the seat"
        );
    }

    #[test]
    fn a_contested_seat_a_prompt_used_up_is_used_up_after_a_restart_too() {
        // Live, the prompt clears the contest and the next hop's seat works. A
        // replay that kept the contest lost that handoff at every restart.
        let (a, b, c, minted) = (BusId::mint(), BusId::mint(), BusId::mint(), BusId::mint());
        let mut replayed = Registry::restore(&[
            a_routed_record(a),
            a_routed_record(b),
            cursor_logged(
                2,
                minted,
                "somebody",
                prompt("mints: the seat is contested"),
            ),
            a_routed_record(c),
        ]);

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut replayed, "the-one-c-sent"),
            c
        );
    }

    #[test]
    fn a_leg_already_here_ends_a_second_hop_live_as_in_replay() {
        // Live, a stranger would sit on the conversation, and that binding is for
        // good. Only a replay used to see that the leg had already arrived.
        let mut live = Registry::new();
        let (a, arrived) = a_seat_waiting_in_the_checkout(&mut live);
        let in_repo = |event: Event| {
            let mut one = Observed::bare(event);
            one.workspace = Some("/repo".into());
            one
        };
        assert_eq!(landed(&mut live, &arrived, in_repo(prompt("go on"))), a);
        landed(
            &mut live,
            &key(),
            in_repo(Event::Routed {
                next: Harness::CURSOR.into(),
                hop: crate::event::Hop::HandedOff,
                reason: "ordered again".into(),
                code_freshness: None,
                provenance: None,
            }),
        );
        landed(&mut live, &arrived, in_repo(prompt("still here")));

        assert_ne!(
            the_next_prompt_in_the_checkout(&mut live, "a-stranger"),
            a,
            "a stranger sits on a conversation whose Cursor leg is already here"
        );
    }

    #[test]
    fn an_observed_hop_that_never_reached_the_log_changes_no_seat() {
        // What a refusal leaves: observed, and never written. It costs nobody a
        // seat, and a restart would not have heard of it anyway.
        let mut registry = Registry::new();
        let (handed_off, _) = a_seat_waiting_in_the_checkout(&mut registry);
        let other = SessionKey::new(Harness::new(Harness::CLAUDE_CODE), "vendor-def");
        registry.observe(&other, Observed::bare(prompt("other work")));
        let mut refused = Observed::bare(Event::Routed {
            next: Harness::CURSOR.into(),
            hop: crate::event::Hop::HandedOff,
            reason: "the log refused this".into(),
            code_freshness: None,
            provenance: None,
        });
        refused.workspace = Some("/repo".into());
        registry.observe(&other, refused);

        assert_eq!(
            the_next_prompt_in_the_checkout(&mut registry, "the-handed-off-one"),
            handed_off
        );
    }

    /// A small generator, so a failure names a seed and runs the same way again.
    struct Dice(u64);

    impl Dice {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 16) % n
        }
    }

    #[test]
    fn the_live_path_and_a_restart_agree_on_the_seats() {
        // An earlier form of this, run 40,000 times by hand, found every class of
        // disagreement the two paths had. A refused hop is in here, a mint whose
        // record never lands is not: that session is bound in memory and nowhere
        // else, and it lands later or the daemon goes.
        const SEQUENCES: usize = 400;
        const STEPS: usize = 24;
        let mut dice = Dice(0x5eed_c0de_0bad_f00d);
        let checkouts = ["/repo", "/another-repo"];
        for sequence in 0..SEQUENCES {
            let mut live = Registry::new();
            let mut log: Vec<Record> = Vec::new();
            let hoppers: Vec<SessionKey> = (0..3)
                .map(|n| SessionKey::new(Harness::new(Harness::CLAUDE_CODE), format!("hopper-{n}")))
                .collect();
            for hopper in &hoppers {
                log.extend(landed_records(
                    &mut live,
                    hopper,
                    Observed::bare(prompt("work")),
                ));
            }
            let mut story = Vec::new();
            for step in 0..STEPS {
                let hopper = &hoppers[dice.below(3) as usize];
                let cursor = SessionKey::new(
                    Harness::new(Harness::CURSOR),
                    format!("cursor-{}", dice.below(4)),
                );
                let workspace = checkouts[dice.below(2) as usize];
                let with_workspace = |event: Event| Observed {
                    workspace: Some(workspace.into()),
                    ..Observed::bare(event)
                };
                let roll = dice.below(8);
                story.push((
                    roll,
                    hopper.vendor_session_id.clone(),
                    cursor.vendor_session_id.clone(),
                    workspace,
                ));
                let routed = || {
                    with_workspace(Event::Routed {
                        next: Harness::CURSOR.into(),
                        hop: crate::event::Hop::HandedOff,
                        reason: "ordered".into(),
                        code_freshness: None,
                        provenance: None,
                    })
                };
                match roll {
                    0 | 1 => log.extend(landed_records(&mut live, hopper, routed())),
                    // Refused: observed and never written.
                    2 => {
                        live.observe(hopper, routed());
                    }
                    3 | 4 => log.extend(landed_records(
                        &mut live,
                        &cursor,
                        with_workspace(prompt("go on")),
                    )),
                    5 => log.extend(landed_records(
                        &mut live,
                        &cursor,
                        with_workspace(a_tool_event()),
                    )),
                    6 => log.extend(landed_records(
                        &mut live,
                        &cursor,
                        with_workspace(session_start()),
                    )),
                    // A restart: what comes next runs on what was read back.
                    _ => live = Registry::restore(&log),
                }
                assert_eq!(
                    live.pending,
                    Registry::restore(&log).pending,
                    "sequence {sequence} step {step}: {story:?}"
                );
            }
        }
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
            landed(&mut registry, &claude, routed);
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
        landed(&mut registry, &claude, routed);

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
