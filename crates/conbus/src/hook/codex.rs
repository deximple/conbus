//! Codex hook payloads, normalised onto the bus event model.
//!
//! Pinned by the draft-07 schemas embedded in the shipped native binary; see
//! `tests/fixtures/codex_0.147.0_contract.md`. Deliberately its own `Payload`
//! rather than sharing Claude Code's: the correlator here is `turn_id`, not
//! `prompt_id`, and every field on the Claude struct is `#[serde(default)]`, so
//! feeding it a Codex payload deserialises cleanly and silently drops the
//! correlator on every record. That is the `failure_reason` bug one layer up.

use conbus_core::event::{
    Body, Event, Harness, ToolOutcome, TurnState, VendorAgentId, VendorToolUseId, VendorTurnId,
};
use conbus_core::registry::{Observed, SessionKey};
use serde::Deserialize;
use serde_json::Value;

use crate::hook::{HookError, Normalized};

/// A Codex hook payload, read tolerantly for the same reason the Claude one is:
/// a hook that rejects an unfamiliar payload takes the user's turn down.
#[derive(Debug, Deserialize)]
pub struct Payload {
    pub session_id: String,
    pub hook_event_name: String,
    #[serde(default)]
    pub cwd: Option<String>,
    /// `SessionStart`: startup | resume | clear | compact. No `fork`, unlike
    /// Claude Code.
    #[serde(default)]
    pub source: Option<String>,
    /// The turn correlator. Required on nine events and absent on
    /// `SessionStart` and `SessionEnd`, so unlike Claude's `prompt_id` it is
    /// never stale and needs no drop rule.
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_response: Option<Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    /// `SessionEnd`. A single constant, `other`, in 0.147.0.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Hook events this adapter has seen and decided not to mirror. Anything not
/// here is reported as unknown rather than dropped in silence.
pub const DECLINED: [&str; 5] = [
    "PermissionRequest",
    "PreCompact",
    "PostCompact",
    "SubagentStart",
    "SubagentStop",
];

/// Read a Codex hook payload into a bus event.
pub fn normalize(harness: &Harness, json: &str) -> Result<Normalized, HookError> {
    let payload: Payload = serde_json::from_str(json).map_err(|err| HookError(err.to_string()))?;

    let vendor_turn_id = payload.turn_id.clone().map(VendorTurnId::new);
    let vendor_agent_id = payload.agent_id.clone().map(VendorAgentId::new);

    let event = match payload.hook_event_name.as_str() {
        "SessionStart" => Event::SessionCandidate {
            trigger: payload.source,
            cwd: payload.cwd,
        },
        "UserPromptSubmit" => Event::TurnStarted {
            // Codex sends no author on a prompt. Absent, never guessed.
            author: None,
            prompt: Body::kept(payload.prompt.unwrap_or_default()),
        },
        "PreToolUse" => Event::ToolRequested {
            tool: payload.tool_name.unwrap_or_default(),
            tool_use_id: payload.tool_use_id.map(VendorToolUseId::new),
            input: body_of(payload.tool_input),
        },
        "PostToolUse" => Event::ToolUse {
            tool: payload.tool_name.unwrap_or_default(),
            tool_use_id: payload.tool_use_id.map(VendorToolUseId::new),
            // Codex does not measure tool calls in its hook payloads.
            duration_ms: None,
            input: body_of(payload.tool_input),
            outcome: ToolOutcome::Succeeded {
                output: body_of(payload.tool_response),
            },
        },
        "Stop" => Event::TurnEnded {
            // There is no `StopFailure` on this surface, so a Codex turn never
            // reports quota or policy through a hook. That signal has to come
            // from the app-server's `account/rateLimits` instead.
            state: TurnState::Complete,
            assistant_message: payload.last_assistant_message.map(Body::kept),
        },
        "SessionEnd" => Event::SessionEnded {
            reason: payload.reason,
        },
        other if DECLINED.contains(&other) => {
            return Ok(Normalized::Ignored {
                hook_event_name: other.to_string(),
            });
        }
        other => {
            return Ok(Normalized::Unknown {
                hook_event_name: other.to_string(),
            });
        }
    };

    Ok(Normalized::Event {
        key: SessionKey::new(harness.clone(), payload.session_id),
        observed: Box::new(Observed {
            vendor_turn_id,
            vendor_agent_id,
            event,
        }),
    })
}

fn body_of(value: Option<Value>) -> Body {
    Body::kept(value.map(|v| v.to_string()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed_of(json: &str) -> Observed {
        match normalize(&Harness::new(Harness::CODEX), json).expect("normalise") {
            Normalized::Event { key, observed } => {
                assert_eq!(key.harness.as_str(), Harness::CODEX);
                assert_eq!(key.vendor_session_id, "cx-1");
                *observed
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }

    const TURN: &str = "CX-TURN-1";

    fn payload(rest: &str) -> String {
        format!(
            r#"{{"session_id":"cx-1","cwd":"/repo","model":"gpt-5-codex",
                "permission_mode":"default","transcript_path":null,{rest}}}"#
        )
    }

    #[test]
    fn the_correlator_is_turn_id_not_prompt_id() {
        // The single most important difference from Claude Code. Reading
        // `prompt_id` here yields `None` on every record and every test stays
        // green, which is why this adapter does not share Claude's `Payload`.
        for event in ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"] {
            let observed = observed_of(&payload(&format!(
                r#""hook_event_name":"{event}","prompt":"p","tool_name":"shell",
                   "tool_input":{{}},"tool_response":{{}},"tool_use_id":"call_1",
                   "stop_hook_active":false,"turn_id":"{TURN}""#
            )));

            assert_eq!(
                observed.vendor_turn_id.as_ref().map(VendorTurnId::as_str),
                Some(TURN),
                "{event} dropped the correlator"
            );
        }
    }

    #[test]
    fn a_prompt_becomes_a_started_turn_with_no_author() {
        // Codex has no `source` on `UserPromptSubmit`, so the author is unknown
        // on every Codex turn. Absent, not defaulted to a human.
        let event = observed_of(&payload(
            r#""hook_event_name":"UserPromptSubmit","prompt":"port the adapter","turn_id":"CX-TURN-1""#,
        ))
        .event;

        assert_eq!(
            event,
            Event::TurnStarted {
                author: None,
                prompt: Body::kept("port the adapter"),
            }
        );
    }

    #[test]
    fn a_tool_call_records_no_duration_because_codex_sends_none() {
        let event = observed_of(&payload(
            r#""hook_event_name":"PostToolUse","tool_name":"shell","tool_input":{"cmd":"ls"},
               "tool_response":{"ok":true},"tool_use_id":"call_1","turn_id":"CX-TURN-1""#,
        ))
        .event;

        assert_eq!(
            event,
            Event::ToolUse {
                tool: "shell".into(),
                tool_use_id: Some(VendorToolUseId::new("call_1")),
                duration_ms: None,
                input: Body::kept(r#"{"cmd":"ls"}"#),
                outcome: ToolOutcome::Succeeded {
                    output: Body::kept(r#"{"ok":true}"#),
                },
            }
        );
    }

    #[test]
    fn a_stop_carries_the_closing_message() {
        let event = observed_of(&payload(
            r#""hook_event_name":"Stop","stop_hook_active":false,
               "last_assistant_message":"ported.","turn_id":"CX-TURN-1""#,
        ))
        .event;

        assert_eq!(
            event,
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: Some(Body::kept("ported.")),
            }
        );
    }

    #[test]
    fn a_session_start_carries_its_source_and_no_correlator() {
        let observed = observed_of(&payload(
            r#""hook_event_name":"SessionStart","source":"resume""#,
        ));

        assert_eq!(
            observed.event,
            Event::SessionCandidate {
                trigger: Some("resume".into()),
                cwd: Some("/repo".into()),
            }
        );
        assert_eq!(observed.vendor_turn_id, None, "Codex sends none here");
    }

    #[test]
    fn a_permission_request_is_not_a_denial() {
        // `PermissionRequest` fires on every approval prompt, before the
        // decision. Codex has no `PermissionDenied` at all, so mapping this to
        // `ToolDenied` would record every approval as a refusal.
        let normalized = normalize(
            &Harness::new(Harness::CODEX),
            &payload(
                r#""hook_event_name":"PermissionRequest","tool_name":"shell","tool_input":{},
                   "turn_id":"CX-TURN-1""#,
            ),
        )
        .expect("normalise");

        assert_eq!(
            normalized,
            Normalized::Ignored {
                hook_event_name: "PermissionRequest".into()
            }
        );
    }

    #[test]
    fn an_unknown_field_does_not_break_the_hook() {
        let event = observed_of(&payload(
            r#""hook_event_name":"UserPromptSubmit","prompt":"hi","turn_id":"CX-TURN-1",
               "some_field_added_next_release":{"nested":true}"#,
        ))
        .event;

        assert_eq!(
            event,
            Event::TurnStarted {
                author: None,
                prompt: Body::kept("hi"),
            }
        );
    }
}
