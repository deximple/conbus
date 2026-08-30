//! Which vendor session belongs to which conversation.
//!
//! The rule this module exists to enforce: a `bus_id` is minted on the first
//! user prompt, never on a session start. Claude Code fires `SessionStart`
//! several times per project within a few hundred milliseconds and some of
//! those are ghosts that never get a transcript
//! (anthropics/claude-code#78455). Minting there would pile up ids that no
//! conversation ever uses, and can leave a real session aliased to a ghost.

use std::collections::HashMap;

use crate::event::{Event, Harness};
use crate::ids::BusId;

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

/// What the daemon should do with an observed event.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Disposition {
    /// Remembered as a session candidate. Nothing reaches the log yet, because
    /// this session may turn out to be a ghost.
    Deferred,
    /// Write these events, in order, under this conversation.
    Write { bus_id: BusId, events: Vec<Event> },
    /// Belongs to no conversation, and this kind of event cannot start one.
    Orphaned,
}

/// The vendor-session-to-conversation map. Lives in the daemon's memory.
#[derive(Default)]
pub struct Registry {
    bound: HashMap<SessionKey, BusId>,
    /// Session starts seen but not yet proven real, kept so that a conversation
    /// can log where it began once its first prompt arrives.
    candidates: HashMap<SessionKey, Event>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The conversation a vendor session is bound to, if any.
    pub fn bus_id(&self, key: &SessionKey) -> Option<BusId> {
        self.bound.get(key).copied()
    }

    /// Record an event and say what should reach the log.
    pub fn observe(&mut self, key: &SessionKey, event: Event) -> Disposition {
        // A bound session is already proven real, so nothing about it is
        // deferred — including the session starts that fire again on resume,
        // compact, and fork.
        if let Some(bus_id) = self.bound.get(key).copied() {
            return Disposition::Write {
                bus_id,
                events: vec![event],
            };
        }
        if matches!(event, Event::SessionCandidate { .. }) {
            self.candidates.insert(key.clone(), event);
            return Disposition::Deferred;
        }
        // Unbound. Only a user prompt proves a session is real enough to mint.
        if !matches!(event, Event::TurnStarted { .. }) {
            return Disposition::Orphaned;
        }
        let bus_id = BusId::mint();
        self.bound.insert(key.clone(), bus_id);
        let events = self
            .candidates
            .remove(key)
            .into_iter()
            .chain(std::iter::once(event))
            .collect();
        Disposition::Write { bus_id, events }
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
            prompt: Body::kept(text),
        }
    }

    #[test]
    fn a_session_start_alone_mints_nothing() {
        let mut registry = Registry::new();

        assert_eq!(
            registry.observe(&key(), session_start()),
            Disposition::Deferred
        );
        assert_eq!(registry.bus_id(&key()), None, "a ghost must not get an id");
    }

    #[test]
    fn the_first_prompt_mints_the_id_and_flushes_the_remembered_session_start() {
        let mut registry = Registry::new();
        registry.observe(&key(), session_start());

        let disposition = registry.observe(&key(), prompt("ship the recorder"));

        let Disposition::Write { bus_id, events } = disposition else {
            panic!("the first prompt must mint, got {disposition:?}");
        };
        assert_eq!(registry.bus_id(&key()), Some(bus_id));
        assert_eq!(
            events,
            vec![session_start(), prompt("ship the recorder")],
            "the session start must reach the log behind the prompt that proved it real"
        );
    }

    #[test]
    fn later_events_in_the_same_session_reuse_the_id_and_flush_nothing_extra() {
        let mut registry = Registry::new();
        registry.observe(&key(), session_start());
        let Disposition::Write { bus_id, .. } = registry.observe(&key(), prompt("first")) else {
            panic!("the first prompt must mint");
        };

        let second = registry.observe(&key(), prompt("second"));
        let ended = registry.observe(
            &key(),
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: None,
            },
        );

        assert_eq!(
            second,
            Disposition::Write {
                bus_id,
                events: vec![prompt("second")]
            }
        );
        assert_eq!(
            ended,
            Disposition::Write {
                bus_id,
                events: vec![Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                }]
            }
        );
    }

    #[test]
    fn a_resume_on_a_bound_session_is_logged_rather_than_held_as_a_candidate() {
        // `SessionStart` fires again on `--resume`, `compact`, and `fork`. That
        // session is already proven real, so there is nothing to defer: holding
        // it as a candidate loses the record and leaks the entry.
        let mut registry = Registry::new();
        let Disposition::Write { bus_id, .. } = registry.observe(&key(), prompt("first")) else {
            panic!("the first prompt must mint");
        };
        let resumed = Event::SessionCandidate {
            trigger: Some("resume".into()),
            cwd: Some("/Applications/conbus".into()),
        };

        let disposition = registry.observe(&key(), resumed.clone());

        assert_eq!(
            disposition,
            Disposition::Write {
                bus_id,
                events: vec![resumed]
            }
        );
    }

    #[test]
    fn a_prompt_mints_even_when_the_session_start_was_never_seen() {
        // Claude Code Desktop's `/clear` omits `source` (#76704), and a hook can
        // be registered mid-session. A missing session start must not cost us
        // the conversation.
        let mut registry = Registry::new();

        let disposition = registry.observe(&key(), prompt("no session start arrived"));

        let Disposition::Write { bus_id, events } = disposition else {
            panic!("a prompt must mint on its own, got {disposition:?}");
        };
        assert_eq!(registry.bus_id(&key()), Some(bus_id));
        assert_eq!(events, vec![prompt("no session start arrived")]);
    }

    #[test]
    fn an_event_that_cannot_start_a_conversation_is_reported_not_dropped() {
        let mut registry = Registry::new();

        let disposition = registry.observe(
            &key(),
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: None,
            },
        );

        assert_eq!(disposition, Disposition::Orphaned);
        assert_eq!(registry.bus_id(&key()), None);
    }

    #[test]
    fn two_vendor_sessions_get_two_conversations() {
        let mut registry = Registry::new();
        let other = SessionKey::new(Harness::new(Harness::CLAUDE_CODE), "vendor-xyz");

        let Disposition::Write { bus_id: first, .. } = registry.observe(&key(), prompt("a")) else {
            panic!("mint");
        };
        let Disposition::Write { bus_id: second, .. } = registry.observe(&other, prompt("b"))
        else {
            panic!("mint");
        };

        assert_ne!(first, second);
    }

    #[test]
    fn the_same_session_id_under_two_harnesses_is_two_conversations() {
        let mut registry = Registry::new();
        let codex = SessionKey::new(Harness::new(Harness::CODEX), "vendor-abc");

        let Disposition::Write { bus_id: claude, .. } = registry.observe(&key(), prompt("a"))
        else {
            panic!("mint");
        };
        let Disposition::Write {
            bus_id: codex_id, ..
        } = registry.observe(&codex, prompt("b"))
        else {
            panic!("mint");
        };

        assert_ne!(
            claude, codex_id,
            "vendor session ids are only unique per vendor"
        );
    }
}
