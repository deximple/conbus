//! The envelope every log line carries.

use serde::{Deserialize, Serialize};

use crate::clock::Timestamp;
use crate::event::{Event, Source, VendorAgentId, VendorTurnId};
use crate::ids::BusId;

/// The log schema this build writes.
///
/// Stamped on every record from the first commit, because the sync story
/// depends on the log being shippable rather than mergeable.
///
/// **When it moves.** v1 is not frozen until the first dogfooding session
/// produces a log worth keeping. After that, bump only when an older reader
/// would be *wrong* — a removed field, a changed type, a changed meaning —
/// never for an added optional field, which `#[serde(default)]` plus
/// `skip_serializing_if` already makes invisible in both directions.
///
/// **Envelope keys are reserved.** `Record` flattens both the vendor stamp and
/// the event body, so every key named here is a reserved word for every present
/// and future event variant. A collision serialises without complaint and then
/// fails to read back as a duplicate field, which means the line reaches the
/// source of truth and is lost on scan. There is no compile error for it — only
/// `no_event_body_shadows_an_envelope_key` below. A vendor field whose name
/// matches one of ours is renamed at the adapter, the way `SessionStart.source`
/// becomes `trigger`.
pub const SCHEMA_VERSION: u32 = 1;

/// One line of the append-only log.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Record {
    #[serde(rename = "v")]
    pub schema_version: u32,
    pub seq: u64,
    pub ts: Timestamp,
    pub bus_id: BusId,
    /// The vendor's turn correlator. `bus_id` says which conversation, this
    /// says which turn inside it. Absent before a session's first prompt, and
    /// on the session-lifecycle events where the vendor's value is stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_turn_id: Option<VendorTurnId>,
    /// Set when the record came from a subagent rather than the main thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_agent_id: Option<VendorAgentId>,
    #[serde(flatten)]
    pub source: Source,
    #[serde(flatten)]
    pub event: Event,
}

/// What a caller hands the log.
///
/// `Record` is what reaches disk, so `seq`, `ts`, and `schema_version` are
/// deliberately absent here: the log owns them. Deliberately no `Default` —
/// a new envelope field should break every construction site loudly rather
/// than default itself to absent on the only path to the source of truth.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub bus_id: BusId,
    pub source: Source,
    pub vendor_turn_id: Option<VendorTurnId>,
    pub vendor_agent_id: Option<VendorAgentId>,
    pub event: Event,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Author, Body, Harness, TurnState, VendorToolUseId};

    fn source() -> Source {
        Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: Some("2.1.0".into()),
            vendor_session_id: Some("f0e1d2c3".into()),
        }
    }

    fn sample() -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 7,
            ts: "2026-08-30T07:41:02.481Z".parse().expect("ts"),
            bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: source(),
            event: Event::TurnStarted {
                author: None,
                prompt: Body::kept("ship the recorder"),
            },
        }
    }

    /// Every `TurnState` variant. `TurnEnded` flattens this, so its tag and
    /// fields are top-level keys of the log line too — the guard has to reach
    /// them, and the exhaustive match is what makes a new one break the build.
    fn every_turn_state() -> Vec<TurnState> {
        let all = vec![
            TurnState::Complete,
            TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            },
            TurnState::InterruptedPolicy {
                reason: "billing_error".into(),
            },
            TurnState::InterruptedUser,
        ];
        for state in &all {
            match state {
                TurnState::Complete
                | TurnState::InterruptedQuota { .. }
                | TurnState::InterruptedPolicy { .. }
                | TurnState::InterruptedUser => {}
            }
        }
        all
    }

    /// Every `Event` variant, with **every optional field populated**.
    ///
    /// Populating them is the whole point. `skip_serializing_if` means an
    /// absent field emits no key, so a variant built with `None` everywhere
    /// cannot collide with anything — and optional-with-skip is the shape of
    /// every field this schema is likely to grow. A guard built on `None`
    /// passes while the collision it exists to catch is live in production.
    fn every_event_variant() -> Vec<Event> {
        let mut all = vec![
            Event::SessionCandidate {
                trigger: Some("compact".into()),
                cwd: Some("/repo".into()),
            },
            Event::TurnStarted {
                author: Some(Author::new(Author::SCHEDULE_WAKEUP)),
                prompt: Body::kept("p"),
            },
            Event::ToolUse {
                tool: "Read".into(),
                tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
                duration_ms: Some(37),
                input: Body::kept("i"),
                output: Body::kept("o"),
            },
            Event::SessionEnded {
                reason: Some("other".into()),
            },
        ];
        all.extend(
            every_turn_state()
                .into_iter()
                .map(|state| Event::TurnEnded {
                    state,
                    assistant_message: Some(Body::kept("done")),
                }),
        );
        for event in &all {
            match event {
                Event::SessionCandidate { .. }
                | Event::TurnStarted { .. }
                | Event::ToolUse { .. }
                | Event::TurnEnded { .. }
                | Event::SessionEnded { .. } => {}
            }
        }
        all
    }

    /// A record with every envelope field populated, for the same reason.
    fn fully_populated(event: Event) -> Record {
        Record {
            vendor_turn_id: Some(VendorTurnId::new("ed06c859-f62e-418f-96f0-556d61fa97d5")),
            vendor_agent_id: Some(VendorAgentId::new("agent-7")),
            event,
            ..sample()
        }
    }

    #[test]
    fn no_event_body_shadows_an_envelope_key() {
        // `Record` flattens both the vendor stamp and the event body, which
        // makes every envelope key a reserved word for every event body — and
        // `TurnEnded` flattens `TurnState`, so that enum's keys are top-level
        // too. A collision serialises without complaint and then fails to read
        // back as a duplicate field, so the line reaches the source of truth
        // and is lost on scan. There is no compile error for it; this is the
        // only guard, and it only works because every field below is populated.
        for event in every_event_variant() {
            let record = fully_populated(event);

            let line = serde_json::to_string(&record).expect("serialise");

            assert_eq!(
                serde_json::from_str::<Record>(&line).expect("must read back"),
                record,
                "event body shadows an envelope key: {line}"
            );
        }
    }

    #[test]
    fn every_populated_field_actually_reaches_the_line() {
        // The guard above is only worth anything if the fields it populates are
        // really serialised. A `#[serde(skip)]` anywhere would make the
        // collision test vacuous and silently drop the field from the log.
        let line = serde_json::to_string(&fully_populated(Event::TurnStarted {
            author: Some(Author::new(Author::SCHEDULE_WAKEUP)),
            prompt: Body::kept("p"),
        }))
        .expect("serialise");

        for key in [
            "vendor_turn_id",
            "vendor_agent_id",
            "author",
            "harness",
            "vendor_session_id",
            "event",
            "prompt",
        ] {
            assert!(
                line.contains(&format!("\"{key}\"")),
                "{key} never reached the line: {line}"
            );
        }
        assert!(line.contains(r#""author":"schedule_wakeup""#), "{line}");

        // The same, for the fields that only exist on a tool call.
        let line = serde_json::to_string(&fully_populated(Event::ToolUse {
            tool: "Read".into(),
            tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
            duration_ms: Some(37),
            input: Body::kept("i"),
            output: Body::kept("o"),
        }))
        .expect("serialise");

        assert!(line.contains(r#""tool_use_id":"toolu_01ABC""#), "{line}");
        assert!(line.contains(r#""duration_ms":37"#), "{line}");
    }

    #[test]
    fn every_turn_state_reaches_the_line_with_its_tag() {
        for state in every_turn_state() {
            let record = fully_populated(Event::TurnEnded {
                state: state.clone(),
                assistant_message: None,
            });

            let line = serde_json::to_string(&record).expect("serialise");

            assert!(
                line.contains(r#""state":"#),
                "{state:?} lost its tag: {line}"
            );
            assert_eq!(
                serde_json::from_str::<Record>(&line).expect("read back"),
                record
            );
        }
    }

    #[test]
    fn the_vendor_turn_and_agent_ids_ride_the_envelope() {
        let record = Record {
            vendor_turn_id: Some(VendorTurnId::new("ed06c859-f62e-418f-96f0-556d61fa97d5")),
            vendor_agent_id: Some(VendorAgentId::new("agent-7")),
            ..sample()
        };

        let line = serde_json::to_string(&record).expect("serialise");

        assert!(
            line.contains(r#""vendor_turn_id":"ed06c859-f62e-418f-96f0-556d61fa97d5""#),
            "{line}"
        );
        assert!(line.contains(r#""vendor_agent_id":"agent-7""#), "{line}");
        assert_eq!(
            serde_json::from_str::<Record>(&line).expect("read back"),
            record
        );
    }

    #[test]
    fn an_absent_correlator_omits_the_key_rather_than_writing_null() {
        // Greppability is the whole argument for the format.
        // `"vendor_turn_id":null` on every line would make the key match
        // everywhere and cost a second filtering pass.
        let line = serde_json::to_string(&sample()).expect("serialise");

        assert!(!line.contains("vendor_turn_id"), "{line}");
        assert!(!line.contains("vendor_agent_id"), "{line}");
        assert!(!line.contains("null"), "{line}");
    }

    #[test]
    fn a_record_is_one_flat_greppable_json_line() {
        let line = serde_json::to_string(&sample()).expect("serialise");

        assert!(!line.contains('\n'), "a record must be one line");
        assert_eq!(
            line,
            r#"{"v":1,"seq":7,"ts":"2026-08-30T07:41:02.481Z","bus_id":"01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e","harness":"claude_code","harness_version":"2.1.0","vendor_session_id":"f0e1d2c3","event":"turn_started","prompt":{"kind":"kept","text":"ship the recorder"}}"#
        );
        assert_eq!(
            serde_json::from_str::<Record>(&line).expect("round trip"),
            sample()
        );
    }

    #[test]
    fn a_quota_interrupted_turn_names_the_vendor_reason() {
        let record = Record {
            seq: 8,
            event: Event::TurnEnded {
                state: TurnState::InterruptedQuota {
                    reason: "rate_limit".into(),
                },
                assistant_message: None,
            },
            ..sample()
        };

        let line = serde_json::to_string(&record).expect("serialise");

        assert!(line.contains(r#""event":"turn_ended""#));
        assert!(line.contains(r#""state":"interrupted_quota""#));
        assert!(line.contains(r#""reason":"rate_limit""#));
        assert_eq!(
            serde_json::from_str::<Record>(&line).expect("round trip"),
            record
        );
    }
}
