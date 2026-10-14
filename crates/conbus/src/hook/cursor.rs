//! Cursor hook payloads, normalised onto the bus event model.
//!
//! Cursor calls the Claude Code hook command (it reads that settings file)
//! but sends its own names. The live drop log pinned the fields: every
//! unmapped camelCase event that carried `cursor_version` used the set below.
//! Official docs agree on types; they are not the source of the names.
//!
//! `user_email` arrives and is discarded. The log is not a place for it.

use serde::Deserialize;
use serde_json::Value;

use conbus_core::event::{
    Body, Event, Harness, ToolOutcome, TurnState, TurnTokens, VendorAgentId, VendorToolUseId,
    VendorTurnId,
};
use conbus_core::registry::{Observed, SessionKey};

use super::{HookError, Normalized};

/// Events seen on the live drop log with `cursor_version`. Others stay
/// [`Normalized::Unknown`] so a new Cursor event is loud.
///
/// Field names from `~/.conbus/drops.jsonl`, 2026-09-06.
#[derive(Debug, Deserialize)]
struct Payload {
    hook_event_name: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    generation_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    workspace_roots: Option<Vec<String>>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_input: Option<Value>,
    #[serde(default)]
    tool_output: Option<Value>,
    #[serde(default)]
    tool_use_id: Option<String>,
    /// `postToolUse`. The vendor's name is `duration`, in milliseconds.
    #[serde(default, deserialize_with = "crate::hook::claude_code::lenient_millis")]
    duration: Option<u64>,
    /// `stop` / `subagentStop`: completed | aborted | error.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    subagent_id: Option<String>,
    /// `sessionEnd`. Official docs: completed | aborted | error | window_close | user_close.
    #[serde(default)]
    reason: Option<String>,
    /// `sessionEnd`. Documented; the 3.19.13 emit site currently omits it.
    /// When it carries a classified quota code, that is the overflow signal.
    #[serde(default)]
    error_message: Option<String>,
    /// `sessionEnd`. Composer status; 3.19.13 sends aborted/unknown/error.
    /// Read in case a later build puts a classified quota code here.
    #[serde(default)]
    final_status: Option<String>,
    /// `afterAgentResponse`. Last assistant bubble. Overflow only when
    /// it carries a classified proto code, not ordinary English.
    ///
    /// Live 2026-08-23 payload on this machine also carried
    /// `input_tokens` / `output_tokens` / `cache_read_tokens` /
    /// `cache_write_tokens`. Those are turn usage, not remaining quota.
    #[serde(default)]
    text: Option<String>,
    /// `afterAgentResponse` turn usage, as four counts. The fields are
    /// optional in Cursor's payload; absent means not reported.
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_tokens: Option<u64>,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
}

/// Whether this payload is Cursor's, not Claude Code's wearing the same command.
pub fn looks_like(payload: &str) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    fields
        .get("cursor_version")
        .and_then(Value::as_str)
        .is_some_and(|version| !version.is_empty())
}

pub fn normalize(harness: &Harness, json: &str) -> Result<Normalized, HookError> {
    let payload: Payload = serde_json::from_str(json).map_err(|err| HookError(err.to_string()))?;
    // Present-but-empty is not an id. Worker-shaped sessions have been seen
    // sending `""` here, and `Some("")` used to win over a real `session_id`
    // behind it. What is left empty after this is refused by the hook client.
    let named = |id: &Option<String>| id.clone().filter(|id| !id.trim().is_empty());
    let session = named(&payload.conversation_id)
        .or_else(|| named(&payload.session_id))
        .or_else(|| payload.conversation_id.clone())
        .or_else(|| payload.session_id.clone())
        .ok_or_else(|| HookError("cursor payload has no conversation_id or session_id".into()))?;

    let vendor_turn_id = named(&payload.generation_id).map(VendorTurnId::new);
    let vendor_agent_id = payload.subagent_id.clone().map(VendorAgentId::new);
    let workspace = payload
        .workspace_roots
        .as_ref()
        .and_then(|roots| roots.first())
        .cloned()
        .or_else(|| payload.cwd.clone());

    let event = match payload.hook_event_name.as_str() {
        "sessionStart" => Event::SessionCandidate { trigger: None },
        "beforeSubmitPrompt" => Event::TurnStarted {
            author: None,
            prompt: Body::kept(payload.prompt.unwrap_or_default()),
        },
        "preToolUse" => Event::ToolRequested {
            tool: payload.tool_name.unwrap_or_default(),
            tool_use_id: payload.tool_use_id.map(VendorToolUseId::new),
            input: body_of(payload.tool_input),
        },
        "postToolUse" => Event::ToolUse {
            tool: payload.tool_name.unwrap_or_default(),
            tool_use_id: payload.tool_use_id.map(VendorToolUseId::new),
            duration_ms: payload.duration,
            input: body_of(payload.tool_input),
            outcome: ToolOutcome::Succeeded {
                output: body_of(payload.tool_output),
            },
        },
        "stop" | "subagentStop" => Event::TurnEnded {
            state: stop_or_quota(payload.status.as_deref()),
            assistant_message: None,
            tokens: None,
        },
        "sessionEnd" => {
            if let Some(message) = session_end_quota(&payload) {
                Event::TurnEnded {
                    state: TurnState::InterruptedQuota { reason: message },
                    assistant_message: None,
                    tokens: None,
                }
            } else {
                Event::SessionEnded {
                    reason: payload.reason,
                }
            }
        }
        "afterAgentResponse" => {
            let tokens = turn_tokens(&payload);
            let text = payload.text.unwrap_or_default();
            if response_is_quota_error(&text) {
                Event::TurnEnded {
                    state: TurnState::InterruptedQuota { reason: text },
                    assistant_message: None,
                    tokens,
                }
            } else {
                // Official last bubble. `stop` does not carry it; this event
                // does. Completing here is what lets the live payload land
                // and its keys get scanned — Unknown was dropping the
                // only Cursor channel that can name a quota code.
                Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: Some(Body::kept(text)),
                    tokens,
                }
            }
        }
        other => {
            return Ok(Normalized::Unknown {
                hook_event_name: other.to_string(),
            });
        }
    };

    Ok(Normalized::Event {
        key: SessionKey::new(harness.clone(), session),
        hook_event_name: payload.hook_event_name,
        observed: Box::new(Observed {
            vendor_turn_id,
            vendor_agent_id,
            workspace,
            event,
        }),
    })
}

/// Official Cursor error codes from the same proto `classifyTurnError` uses.
/// They are telemetry-only on 3.19.13 `stop`. `sessionEnd` already arrives;
/// overflow only when one of its fields carries a classified code.
fn session_end_quota(payload: &Payload) -> Option<String> {
    [
        payload.error_message.as_deref(),
        payload.final_status.as_deref(),
        payload.reason.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find(|text| classified_quota(text))
    .map(str::to_string)
}

const QUOTA_CODES: [&str; 8] = [
    "FREE_USER_RATE_LIMIT_EXCEEDED",
    "PRO_USER_RATE_LIMIT_EXCEEDED",
    "FREE_USER_USAGE_LIMIT",
    "PRO_USER_USAGE_LIMIT",
    "RESOURCE_EXHAUSTED",
    "RATE_LIMITED_CHANGEABLE",
    "GENERIC_RATE_LIMIT_EXCEEDED",
    "RATE_LIMITED",
];

/// A status or error field that names a limit. These are Cursor's own fields,
/// so containing a code is enough.
fn classified_quota(text: &str) -> bool {
    QUOTA_CODES.iter().any(|code| text.contains(code))
}

/// Longest agent response still read as an error line.
const ERROR_LINE_MAX: usize = 160;

/// Whether an agent response is Cursor reporting a limit rather than the agent
/// talking about one.
///
/// `afterAgentResponse` is the agent's own words, and the only place Cursor
/// names a limit it hit: the bubble reads `request failed: PRO_USER_USAGE_LIMIT`.
/// A reply that explains gRPC's RESOURCE_EXHAUSTED contains the code too. The
/// error is one short line that ends in the code; prose is not.
fn response_is_quota_error(text: &str) -> bool {
    let line = text.trim();
    !line.contains('\n')
        && line.chars().count() <= ERROR_LINE_MAX
        && line
            .rsplit([' ', ':'])
            .next()
            .is_some_and(|last| QUOTA_CODES.contains(&last))
}

fn stop_or_quota(status: Option<&str>) -> TurnState {
    match status {
        Some(code) if classified_quota(code) => TurnState::InterruptedQuota {
            reason: code.to_string(),
        },
        other => stop_state(other),
    }
}

fn stop_state(status: Option<&str>) -> TurnState {
    match status {
        Some("aborted") => TurnState::InterruptedUser,
        Some("error") => TurnState::InterruptedPolicy {
            reason: "error".into(),
        },
        _ => TurnState::Complete,
    }
}

/// The turn's token counts when the payload reports any of them. The four
/// are one group on Cursor's side, but a partial group is still what the
/// vendor said — a missing field reads as zero rather than dropping the rest.
fn turn_tokens(payload: &Payload) -> Option<TurnTokens> {
    if payload.input_tokens.is_none()
        && payload.output_tokens.is_none()
        && payload.cache_read_tokens.is_none()
        && payload.cache_write_tokens.is_none()
    {
        return None;
    }
    Some(TurnTokens {
        input: payload.input_tokens.unwrap_or(0),
        output: payload.output_tokens.unwrap_or(0),
        cache_read: payload.cache_read_tokens.unwrap_or(0),
        cache_write: payload.cache_write_tokens.unwrap_or(0),
    })
}

fn body_of(value: Option<Value>) -> Body {
    Body::kept(value.map(|v| v.to_string()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::normalize;

    const BEFORE: &str = r#"{
        "hook_event_name":"beforeSubmitPrompt",
        "cursor_version":"2.0.0",
        "conversation_id":"conv-1",
        "session_id":"conv-1",
        "generation_id":"gen-1",
        "prompt":"continue the checkout",
        "workspace_roots":["/repo"],
        "user_email":"someone@example.com"
    }"#;

    const PRE: &str = r#"{
        "hook_event_name":"preToolUse",
        "cursor_version":"2.0.0",
        "conversation_id":"conv-1",
        "session_id":"conv-1",
        "generation_id":"gen-1",
        "tool_name":"Read",
        "tool_use_id":"tu-1",
        "tool_input":{"path":"/repo/README.md"},
        "workspace_roots":["/repo"]
    }"#;

    const POST: &str = r#"{
        "hook_event_name":"postToolUse",
        "cursor_version":"2.0.0",
        "conversation_id":"conv-1",
        "session_id":"conv-1",
        "generation_id":"gen-1",
        "tool_name":"Read",
        "tool_use_id":"tu-1",
        "tool_input":{"path":"/repo/README.md"},
        "tool_output":{"ok":true},
        "duration":12,
        "workspace_roots":["/repo"]
    }"#;

    const STOP: &str = r#"{
        "hook_event_name":"stop",
        "cursor_version":"2.0.0",
        "conversation_id":"conv-1",
        "session_id":"conv-1",
        "generation_id":"gen-1",
        "status":"completed",
        "workspace_roots":["/repo"]
    }"#;

    fn event_of(json: &str) -> Event {
        match normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("ok") {
            Normalized::Event { observed, .. } => observed.event.clone(),
            other => panic!("{other:?}"),
        }
    }

    fn key_of(json: &str) -> (String, Option<String>) {
        match normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("ok") {
            Normalized::Event { key, observed, .. } => (
                key.vendor_session_id,
                observed
                    .vendor_turn_id
                    .as_ref()
                    .map(|id| id.as_str().to_string()),
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_conversation_id_gives_way_to_a_real_session_id() {
        // `Some("")` used to win, so a worker-shaped payload with an empty
        // conversation id was keyed on nothing even when `session_id` named it.
        let json = r#"{
            "hook_event_name":"preToolUse",
            "cursor_version":"3.19.13",
            "conversation_id":"",
            "session_id":"conv-worker",
            "generation_id":"",
            "tool_name":"Shell",
            "tool_use_id":"tu-9",
            "tool_input":{"command":"ls"},
            "workspace_roots":["/repo"]
        }"#;
        assert_eq!(key_of(json), ("conv-worker".to_string(), None));
    }

    #[test]
    fn a_payload_that_names_no_session_keeps_an_empty_key_for_the_client_to_refuse() {
        // The adapter reports what arrived; refusing it is one decision made in
        // one place (`hook::NO_SESSION`), for every harness.
        let json = r#"{
            "hook_event_name":"postToolUse",
            "cursor_version":"3.19.13",
            "conversation_id":"",
            "session_id":"",
            "generation_id":"",
            "tool_name":"Read",
            "tool_use_id":"tu-10",
            "tool_output":{"ok":true},
            "workspace_roots":["/repo"]
        }"#;
        assert_eq!(key_of(json), (String::new(), None));
    }

    #[test]
    fn a_cursor_payload_arriving_as_claude_code_is_still_read_as_cursor() {
        let Normalized::Event { key, .. } =
            normalize(&Harness::new(Harness::CLAUDE_CODE), BEFORE).expect("ok")
        else {
            panic!("expected an event");
        };
        assert_eq!(key.harness.as_str(), Harness::CURSOR);
        assert_eq!(key.vendor_session_id, "conv-1");
    }

    #[test]
    fn the_prompt_is_a_started_turn_and_the_email_is_not_kept() {
        match event_of(BEFORE) {
            Event::TurnStarted { prompt, .. } => match prompt {
                Body::Kept { text } => assert_eq!(text, "continue the checkout"),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
        assert!(!BEFORE.is_empty());
        let Normalized::Event { observed, .. } =
            normalize(&Harness::new(Harness::CLAUDE_CODE), BEFORE).expect("ok")
        else {
            panic!("event");
        };
        let dumped = format!("{:?}", observed.event);
        assert!(!dumped.contains("someone@example.com"), "{dumped}");
    }

    #[test]
    fn a_tool_call_keeps_its_name_duration_and_output_field() {
        match event_of(PRE) {
            Event::ToolRequested { tool, .. } => assert_eq!(tool, "Read"),
            other => panic!("{other:?}"),
        }
        match event_of(POST) {
            Event::ToolUse {
                duration_ms,
                outcome: ToolOutcome::Succeeded { output },
                ..
            } => {
                assert_eq!(duration_ms, Some(12));
                let Body::Kept { text } = output else {
                    panic!("{output:?}");
                };
                assert!(text.contains("ok"), "{text}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_completed_stop_ends_the_turn() {
        match event_of(STOP) {
            Event::TurnEnded {
                state: TurnState::Complete,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_aborted_stop_is_the_user() {
        let json = STOP.replace("completed", "aborted");
        match event_of(&json) {
            Event::TurnEnded {
                state: TurnState::InterruptedUser,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_stop_with_a_classified_quota_code_overflows() {
        let json = STOP.replace("completed", "PRO_USER_USAGE_LIMIT");
        match event_of(&json) {
            Event::TurnEnded {
                state: TurnState::InterruptedQuota { reason },
                ..
            } => assert_eq!(reason, "PRO_USER_USAGE_LIMIT"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_error_stop_is_not_quota() {
        // Cursor's hook stdin is status=completed|aborted|error only.
        // The binary classifies FREE_USER_RATE_LIMIT_EXCEEDED internally
        // and keeps that code off this payload. Promoting every error
        // would hop on compiler failures.
        let json = STOP.replace("completed", "error");
        match event_of(&json) {
            Event::TurnEnded {
                state: TurnState::InterruptedPolicy { reason },
                ..
            } => assert_eq!(reason, "error"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unseen_cursor_event_is_unknown_not_ignored() {
        let json = r#"{"hook_event_name":"afterAgentThought","cursor_version":"2.0.0","conversation_id":"c"}"#;
        match normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("ok") {
            Normalized::Unknown { hook_event_name } => {
                assert_eq!(hook_event_name, "afterAgentThought");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_claude_payload_is_not_stolen_by_the_cursor_adapter() {
        let json = r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi","prompt_id":"T"}"#;
        let Normalized::Event { key, .. } =
            normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("ok")
        else {
            panic!("event");
        };
        assert_eq!(key.harness.as_str(), Harness::CLAUDE_CODE);
    }

    #[test]
    fn a_session_end_without_a_quota_code_is_not_overflow() {
        let json = r#"{
            "hook_event_name":"sessionEnd",
            "cursor_version":"2.0.0",
            "conversation_id":"conv-1",
            "reason":"error",
            "error_message":"compile failed",
            "final_status":"aborted"
        }"#;
        match event_of(json) {
            Event::SessionEnded { reason } => assert_eq!(reason.as_deref(), Some("error")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_session_end_with_a_classified_quota_code_overflows() {
        let json = r#"{
            "hook_event_name":"sessionEnd",
            "cursor_version":"2.0.0",
            "conversation_id":"conv-1",
            "reason":"error",
            "error_message":"PRO_USER_USAGE_LIMIT",
            "final_status":"aborted"
        }"#;
        match event_of(json) {
            Event::TurnEnded {
                state: TurnState::InterruptedQuota { reason },
                ..
            } => assert_eq!(reason, "PRO_USER_USAGE_LIMIT"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_session_end_quota_code_in_final_status_overflows() {
        let json = r#"{
            "hook_event_name":"sessionEnd",
            "cursor_version":"2.0.0",
            "conversation_id":"conv-1",
            "reason":"error",
            "final_status":"RESOURCE_EXHAUSTED"
        }"#;
        match event_of(json) {
            Event::TurnEnded {
                state: TurnState::InterruptedQuota { reason },
                ..
            } => assert_eq!(reason, "RESOURCE_EXHAUSTED"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_after_agent_response_with_a_quota_code_overflows() {
        let json = include_str!("../../tests/fixtures/cursor_3.19_after_agent_response.json");
        match event_of(json) {
            Event::TurnEnded {
                state: TurnState::InterruptedQuota { reason },
                ..
            } => assert!(reason.contains("PRO_USER_USAGE_LIMIT")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_after_agent_response_reports_the_turns_tokens() {
        let json = serde_json::json!({
            "hook_event_name": "afterAgentResponse",
            "cursor_version": "3.19.13",
            "conversation_id": "conv-1",
            "generation_id": "gen-1",
            "text": "done",
            "input_tokens": 1200,
            "output_tokens": 80,
            "cache_read_tokens": 900,
            "cache_write_tokens": 100,
        })
        .to_string();
        match event_of(&json) {
            Event::TurnEnded {
                state: TurnState::Complete,
                tokens:
                    Some(TurnTokens {
                        input: 1200,
                        output: 80,
                        cache_read: 900,
                        cache_write: 100,
                    }),
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_quota_after_agent_response_still_carries_its_tokens() {
        // The fixture's `request failed: PRO_USER_USAGE_LIMIT` bubble reports
        // the turn's usage too; a refused turn still spent tokens.
        let json = include_str!("../../tests/fixtures/cursor_3.19_after_agent_response.json");
        match event_of(json) {
            Event::TurnEnded {
                tokens: Some(tokens),
                ..
            } => assert_eq!((tokens.input, tokens.output), (1, 1)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_after_agent_response_without_token_fields_carries_none() {
        // Older builds and other senders never had the fields; absent is
        // "not reported", not zero.
        match after_agent_response("done") {
            Event::TurnEnded { tokens: None, .. } => {}
            other => panic!("{other:?}"),
        }
    }

    fn after_agent_response(text: &str) -> Event {
        event_of(
            &serde_json::json!({
                "hook_event_name": "afterAgentResponse",
                "cursor_version": "3.19.13",
                "conversation_id": "conv-1",
                "generation_id": "gen-1",
                "text": text,
            })
            .to_string(),
        )
    }

    #[test]
    fn a_reply_that_mentions_a_quota_code_is_a_reply_not_a_stop() {
        // `afterAgentResponse` carries the agent's own words. A reply that
        // explains gRPC's RESOURCE_EXHAUSTED contains the code and is not
        // Cursor refusing a turn.
        let reply =
            "The API returned RESOURCE_EXHAUSTED, so I retried with backoff and the tests pass.";
        match after_agent_response(reply) {
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: Some(Body::Kept { text }),
                ..
            } => assert_eq!(text, reply),
            other => panic!("{other:?}"),
        }
        let quoted = "Found it:\nRESOURCE_EXHAUSTED";
        assert!(
            matches!(
                after_agent_response(quoted),
                Event::TurnEnded {
                    state: TurnState::Complete,
                    ..
                }
            ),
            "a reply that ends on a code line is still prose"
        );
    }

    #[test]
    fn a_bare_quota_code_as_the_whole_reply_is_the_stop() {
        for line in [
            "RESOURCE_EXHAUSTED",
            "request failed: FREE_USER_RATE_LIMIT_EXCEEDED",
        ] {
            match after_agent_response(line) {
                Event::TurnEnded {
                    state: TurnState::InterruptedQuota { reason },
                    ..
                } => assert_eq!(reason, line),
                other => panic!("{line}: {other:?}"),
            }
        }
    }

    #[test]
    fn an_after_agent_response_without_a_quota_code_keeps_the_bubble() {
        let json = r#"{
            "hook_event_name":"afterAgentResponse",
            "cursor_version":"2.0.0",
            "conversation_id":"conv-1",
            "text":"the tests passed"
        }"#;
        match event_of(json) {
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: Some(Body::Kept { text }),
                ..
            } => assert_eq!(text, "the tests passed"),
            other => panic!("{other:?}"),
        }
    }
}
