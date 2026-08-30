//! The envelope every log line carries.

use serde::{Deserialize, Serialize};

use crate::clock::Timestamp;
use crate::event::{Event, Source};
use crate::ids::BusId;

/// The log schema this build writes.
///
/// Stamped on every record from the first commit, because the sync story
/// depends on the log being shippable rather than mergeable.
pub const SCHEMA_VERSION: u32 = 1;

/// One line of the append-only log.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Record {
    #[serde(rename = "v")]
    pub schema_version: u32,
    pub seq: u64,
    pub ts: Timestamp,
    pub bus_id: BusId,
    #[serde(flatten)]
    pub source: Source,
    #[serde(flatten)]
    pub event: Event,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Body, Harness, TurnState};

    fn source() -> Source {
        Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: Some("2.1.0".into()),
            vendor_session_id: Some("f0e1d2c3".into()),
        }
    }

    #[test]
    fn a_record_is_one_flat_greppable_json_line() {
        let record = Record {
            schema_version: SCHEMA_VERSION,
            seq: 7,
            ts: "2026-08-30T07:41:02.481Z".parse().expect("ts"),
            bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
            source: source(),
            event: Event::TurnStarted {
                prompt: Body::kept("ship the recorder"),
            },
        };

        let line = serde_json::to_string(&record).expect("serialise");

        assert!(!line.contains('\n'), "a record must be one line");
        assert_eq!(
            line,
            r#"{"v":1,"seq":7,"ts":"2026-08-30T07:41:02.481Z","bus_id":"01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e","harness":"claude_code","harness_version":"2.1.0","vendor_session_id":"f0e1d2c3","event":"turn_started","prompt":{"kind":"kept","text":"ship the recorder"}}"#
        );
        assert_eq!(
            serde_json::from_str::<Record>(&line).expect("round trip"),
            record
        );
    }

    #[test]
    fn a_quota_interrupted_turn_names_the_vendor_reason() {
        let record = Record {
            schema_version: SCHEMA_VERSION,
            seq: 8,
            ts: "2026-08-30T07:45:00.000Z".parse().expect("ts"),
            bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
            source: source(),
            event: Event::TurnEnded {
                state: TurnState::InterruptedQuota {
                    reason: "rate_limit".into(),
                },
                assistant_message: None,
            },
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
