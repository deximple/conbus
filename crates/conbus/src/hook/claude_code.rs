//! Claude Code hook payloads, normalised onto the bus event model.
//!
//! Field names and enum values below were read off the installed CLI
//! (2.1.241), not guessed: `failure_reason`, and the `credits_required`
//! reason that the handoff's list of four does not mention.

use conbus_core::event::{Body, Event, Harness, TurnState};
use conbus_core::registry::SessionKey;
use serde::Deserialize;
use serde_json::Value;

/// A Claude Code hook payload, read tolerantly.
///
/// Only `session_id` and `hook_event_name` are required. Everything else is
/// optional on purpose: a hook that rejects an unfamiliar payload takes the
/// session's turn down with it, and hook failures here are silent by default.
#[derive(Debug, Deserialize)]
pub struct Payload {
    pub session_id: String,
    pub hook_event_name: String,
    #[serde(default)]
    pub cwd: Option<String>,
    /// `SessionStart`: startup | resume | clear | compact | fork | subagent.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_response: Option<Value>,
    /// `StopFailure`. One of eleven values; see `failure_state`.
    #[serde(default)]
    pub error: Option<String>,
    /// `StopFailure`: the vendor's own elaboration on `error`.
    #[serde(default)]
    pub error_details: Option<String>,
    /// `Stop`, `StopFailure`, `SubagentStop`: the assistant's closing text.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    /// `SessionEnd`: clear | logout | prompt_input_exit | other.
    #[serde(default)]
    pub reason: Option<String>,
}

/// The result of reading a hook payload.
#[derive(Debug, PartialEq, Eq)]
pub enum Normalized {
    Event {
        key: SessionKey,
        event: Event,
    },
    /// A hook event the recorder deliberately does not mirror.
    Ignored {
        hook_event_name: String,
    },
}

/// A payload we could not read at all.
#[derive(Debug, PartialEq, Eq)]
pub struct HookError(pub String);

/// Read a hook payload into a bus event.
pub fn normalize(json: &str) -> Result<Normalized, HookError> {
    let payload: Payload = serde_json::from_str(json).map_err(|err| HookError(err.to_string()))?;

    let event = match payload.hook_event_name.as_str() {
        "SessionStart" => Event::SessionCandidate {
            trigger: payload.source,
            cwd: payload.cwd,
        },
        "UserPromptSubmit" => Event::TurnStarted {
            prompt: Body::kept(payload.prompt.unwrap_or_default()),
        },
        "PostToolUse" => Event::ToolUse {
            tool: payload.tool_name.unwrap_or_default(),
            input: body_of(payload.tool_input),
            output: body_of(payload.tool_response),
        },
        "Stop" => Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: payload.last_assistant_message.map(Body::kept),
        },
        "StopFailure" => Event::TurnEnded {
            state: failure_state(payload.error, payload.error_details),
            assistant_message: payload.last_assistant_message.map(Body::kept),
        },
        "SessionEnd" => Event::SessionEnded {
            reason: payload.reason,
        },
        other => {
            return Ok(Normalized::Ignored {
                hook_event_name: other.to_string(),
            });
        }
    };

    Ok(Normalized::Event {
        key: SessionKey::new(Harness::new(Harness::CLAUDE_CODE), payload.session_id),
        event,
    })
}

fn body_of(value: Option<Value>) -> Body {
    Body::kept(value.map(|v| v.to_string()).unwrap_or_default())
}

/// Which interruptions mean "this leg cannot serve the turn right now".
///
/// The vendor enum has eleven values: authentication_failed,
/// oauth_org_not_allowed, account_on_hold, billing_error, rate_limit,
/// overloaded, invalid_request, model_not_found, server_error, unknown,
/// max_output_tokens. Only the two capacity ones route. Everything else keeps
/// its vendor reason verbatim, so an unfamiliar value — or a better
/// classification once the router exists — is a re-read of the log rather than
/// a guess made now.
fn failure_state(error: Option<String>, details: Option<String>) -> TurnState {
    let reason = match (error, details) {
        (Some(error), Some(details)) => format!("{error}: {details}"),
        (Some(error), None) => error,
        (None, Some(details)) => details,
        (None, None) => "unspecified".to_string(),
    };
    match reason.split(':').next().unwrap_or_default() {
        "rate_limit" | "overloaded" => TurnState::InterruptedQuota { reason },
        _ => TurnState::InterruptedPolicy { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SessionKey {
        SessionKey::new(Harness::new(Harness::CLAUDE_CODE), "abc123")
    }

    fn event_of(json: &str) -> Event {
        match normalize(json).expect("normalise") {
            Normalized::Event { key: got, event } => {
                assert_eq!(got, key());
                event
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn a_user_prompt_becomes_a_started_turn() {
        let event = event_of(
            r#"{"session_id":"abc123","transcript_path":"/t.jsonl","cwd":"/repo",
                "hook_event_name":"UserPromptSubmit","prompt":"ship the recorder"}"#,
        );

        assert_eq!(
            event,
            Event::TurnStarted {
                prompt: Body::kept("ship the recorder")
            }
        );
    }

    #[test]
    fn a_session_start_carries_its_source_and_directory() {
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"SessionStart",
                "source":"resume","cwd":"/repo"}"#,
        );

        assert_eq!(
            event,
            Event::SessionCandidate {
                trigger: Some("resume".into()),
                cwd: Some("/repo".into()),
            }
        );
    }

    #[test]
    fn a_tool_call_keeps_its_name_and_both_bodies() {
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Read",
                "tool_input":{"file_path":"/etc/hosts"},"tool_response":{"ok":true}}"#,
        );

        assert_eq!(
            event,
            Event::ToolUse {
                tool: "Read".into(),
                input: Body::kept(r#"{"file_path":"/etc/hosts"}"#),
                output: Body::kept(r#"{"ok":true}"#),
            }
        );
    }

    #[test]
    fn a_plain_stop_is_a_completed_turn() {
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"Stop","stop_hook_active":false}"#,
        );

        assert_eq!(
            event,
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: None,
            }
        );
    }

    #[test]
    fn a_rate_limit_stop_is_the_quota_interruption_the_router_acts_on() {
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"StopFailure","error":"rate_limit"}"#,
        );

        assert_eq!(
            event,
            Event::TurnEnded {
                state: TurnState::InterruptedQuota {
                    reason: "rate_limit".into()
                },
                assistant_message: None,
            }
        );
    }

    #[test]
    fn an_overloaded_vendor_counts_as_quota_not_policy() {
        // A vendor outage is the third thing the product survives, alongside
        // limits and policies, and it is a routing signal like a rate limit.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"StopFailure","error":"overloaded"}"#,
        );

        assert_eq!(
            event,
            Event::TurnEnded {
                state: TurnState::InterruptedQuota {
                    reason: "overloaded".into()
                },
                assistant_message: None,
            }
        );
    }

    #[test]
    fn org_billing_and_auth_failures_are_policy_interruptions() {
        for reason in [
            "oauth_org_not_allowed",
            "billing_error",
            "authentication_failed",
            "account_on_hold",
        ] {
            let event = event_of(&format!(
                r#"{{"session_id":"abc123","hook_event_name":"StopFailure","error":"{reason}"}}"#
            ));

            assert_eq!(
                event,
                Event::TurnEnded {
                    state: TurnState::InterruptedPolicy {
                        reason: reason.into()
                    },
                    assistant_message: None,
                }
            );
        }
    }

    #[test]
    fn an_unknown_failure_reason_is_kept_verbatim_rather_than_guessed_at() {
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"StopFailure","error":"something_new"}"#,
        );

        assert_eq!(
            event,
            Event::TurnEnded {
                state: TurnState::InterruptedPolicy {
                    reason: "something_new".into()
                },
                assistant_message: None,
            },
            "an unrecognised reason must survive in the log so it can be reclassified later"
        );
    }

    #[test]
    fn a_session_end_carries_its_reason() {
        let event =
            event_of(r#"{"session_id":"abc123","hook_event_name":"SessionEnd","reason":"clear"}"#);

        assert_eq!(
            event,
            Event::SessionEnded {
                reason: Some("clear".into())
            }
        );
    }

    #[test]
    fn hook_events_the_recorder_does_not_mirror_are_named_not_swallowed() {
        let normalized = normalize(
            r#"{"session_id":"abc123","hook_event_name":"PreToolUse","tool_name":"Bash"}"#,
        )
        .expect("normalise");

        assert_eq!(
            normalized,
            Normalized::Ignored {
                hook_event_name: "PreToolUse".into()
            }
        );
    }

    #[test]
    fn a_payload_without_a_session_id_is_an_error_not_a_guess() {
        let err = normalize(r#"{"hook_event_name":"Stop"}"#).expect_err("must fail");

        assert!(err.0.contains("session_id"), "unhelpful error: {}", err.0);
    }

    #[test]
    fn an_unknown_field_does_not_break_the_hook() {
        // Vendor payloads gain fields between releases. Rejecting one would take
        // the user's turn down, and hook failures here are silent.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"UserPromptSubmit","prompt":"hi",
                "some_field_added_next_release":{"nested":true}}"#,
        );

        assert_eq!(
            event,
            Event::TurnStarted {
                prompt: Body::kept("hi")
            }
        );
    }
}
