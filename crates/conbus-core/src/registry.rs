//! Which vendor session belongs to which conversation.
//!
//! The rule this module exists to enforce: a `bus_id` is minted on the first
//! user prompt, never on a session start. Claude Code fires `SessionStart`
//! several times per project within a few hundred milliseconds and some of
//! those are ghosts that never get a transcript
//! (anthropics/claude-code#78455). Minting there would pile up ids that no
//! conversation ever uses, and can leave a real session aliased to a ghost.

use std::collections::HashMap;

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
    pub event: Event,
}

impl Observed {
    /// An event that carried no correlators.
    pub fn bare(event: Event) -> Self {
        Observed {
            vendor_turn_id: None,
            vendor_agent_id: None,
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

/// The vendor-session-to-conversation map. Lives in the daemon's memory.
#[derive(Default)]
pub struct Registry {
    bound: HashMap<SessionKey, BusId>,
    /// Session starts seen but not yet proven real, kept so that a conversation
    /// can log where it began once its first prompt arrives.
    candidates: HashMap<SessionKey, Observed>,
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
    /// conversation. Cost is one pass over the log; the derived SQLite index
    /// the plan schedules is what replaces it once the log stops being small.
    pub fn restore(records: &[Record]) -> Self {
        let mut registry = Registry::new();
        for record in records {
            let Some(vendor_session_id) = &record.source.vendor_session_id else {
                continue;
            };
            registry.bound.insert(
                SessionKey::new(record.source.harness.clone(), vendor_session_id.clone()),
                record.bus_id,
            );
        }
        registry
    }

    /// The conversation a vendor session is bound to, if any.
    pub fn bus_id(&self, key: &SessionKey) -> Option<BusId> {
        self.bound.get(key).copied()
    }

    /// Record an event and say what should reach the log.
    pub fn observe(&mut self, key: &SessionKey, observed: Observed) -> Disposition {
        // A bound session is already proven real, so nothing about it is
        // deferred — including the session starts that fire again on resume,
        // compact, and fork.
        if let Some(bus_id) = self.bound.get(key).copied() {
            return Disposition::Write {
                bus_id,
                observed: vec![observed],
            };
        }
        if matches!(observed.event, Event::SessionCandidate { .. }) {
            self.candidates.insert(key.clone(), observed);
            return Disposition::Deferred;
        }
        // Unbound. Only a prompt proves a session is real enough to mint, and
        // any prompt does — a machine-authored turn is still a conversation,
        // and refusing to mint would drop its head permanently.
        if !matches!(observed.event, Event::TurnStarted { .. }) {
            return Disposition::Orphaned;
        }
        let bus_id = BusId::mint();
        self.bound.insert(key.clone(), bus_id);
        let observed = self
            .candidates
            .remove(key)
            .into_iter()
            .chain(std::iter::once(observed))
            .collect();
        Disposition::Write { bus_id, observed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Body, TurnState};

    fn key() -> SessionKey {
        SessionKey::new(Harness::new(Harness::CLAUDE_CODE), "vendor-abc")
    }

    fn session_start() -> Event {
        Event::SessionCandidate {
            trigger: Some("startup".into()),
            cwd: Some("/Applications/conbus".into()),
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
        use crate::clock::Timestamp;
        use crate::event::Source;
        use crate::record::SCHEMA_VERSION;

        let bus_id = BusId::mint();
        let logged = Record {
            schema_version: SCHEMA_VERSION,
            seq: 0,
            ts: Timestamp::now(),
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some("vendor-abc".into()),
            },
            event: prompt("before the restart"),
        };

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
    fn a_session_start_alone_mints_nothing() {
        let mut registry = Registry::new();

        assert_eq!(
            registry.observe(&key(), Observed::bare(session_start())),
            Disposition::Deferred
        );
        assert_eq!(registry.bus_id(&key()), None, "a ghost must not get an id");
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
            cwd: Some("/Applications/conbus".into()),
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
            }),
        );

        assert_eq!(disposition, Disposition::Orphaned);
        assert_eq!(registry.bus_id(&key()), None);
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
}
