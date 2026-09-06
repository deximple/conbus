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
    Body, Event, Harness, ToolOutcome, TurnState, VendorAgentId, VendorToolUseId, VendorTurnId,
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
    let session = payload
        .conversation_id
        .clone()
        .or_else(|| payload.session_id.clone())
        .ok_or_else(|| HookError("cursor payload has no conversation_id or session_id".into()))?;

    let vendor_turn_id = payload.generation_id.clone().map(VendorTurnId::new);
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
        "stop" => Event::TurnEnded {
            state: stop_state(payload.status.as_deref()),
            assistant_message: None,
        },
        "subagentStop" => Event::TurnEnded {
            state: stop_state(payload.status.as_deref()),
            assistant_message: None,
        },
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

fn stop_state(status: Option<&str>) -> TurnState {
    match status {
        Some("aborted") => TurnState::InterruptedUser,
        Some("error") => TurnState::InterruptedPolicy {
            reason: "error".into(),
        },
        _ => TurnState::Complete,
    }
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
}
