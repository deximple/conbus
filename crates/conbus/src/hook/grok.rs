//! Grok hook payloads.
//!
//! Official docs (`~/.grok/docs/user-guide/10-hooks.md`) send Claude's
//! `hook_event_name` plus Grok's own camelCase twins (`sessionId`,
//! `hookEventName`, `errorDetails`). `StopFailure.error` is `rate_limit` —
//! the same classified quota signal Claude uses. There is no remaining-quota
//! JSON; this is the overflow trigger.

use serde_json::{Map, Value};

use conbus_core::event::Harness;

use super::claude_code;
use super::{HookError, Normalized};

/// Grok's own event name sits on `hookEventName`. Claude never sends that key.
pub fn looks_like(payload: &str) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    fields
        .get("hookEventName")
        .and_then(Value::as_str)
        .is_some_and(|name| !name.is_empty())
        && fields.get("cursor_version").is_none()
}

pub fn normalize(json: &str) -> Result<Normalized, HookError> {
    let mut value: Value = serde_json::from_str(json).map_err(|err| HookError(err.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| HookError("grok payload is not an object".into()))?;
    alias(object, "sessionId", "session_id");
    alias(object, "promptId", "prompt_id");
    alias(object, "errorDetails", "error_details");
    alias(object, "lastAssistantMessage", "last_assistant_message");
    alias(object, "toolName", "tool_name");
    alias(object, "toolInput", "tool_input");
    alias(object, "toolUseId", "tool_use_id");
    if !object.contains_key("cwd") {
        alias(object, "workspaceRoot", "cwd");
    }
    claude_code::normalize(&Harness::new(Harness::GROK), &value.to_string())
}

fn alias(object: &mut Map<String, Value>, from: &str, to: &str) {
    if object.contains_key(to) {
        return;
    }
    if let Some(value) = object.get(from).cloned() {
        object.insert(to.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::normalize;
    use conbus_core::event::{Event, TurnState};

    const STOP_FAIL: &str = r#"{
        "hookEventName": "stop_failure",
        "hook_event_name": "StopFailure",
        "sessionId": "grok-sess",
        "promptId": "turn-1",
        "error": "rate_limit",
        "errorDetails": "capacity",
        "cwd": "/repo"
    }"#;

    #[test]
    fn a_grok_rate_limit_is_the_quota_interruption() {
        match normalize(&Harness::new(Harness::CLAUDE_CODE), STOP_FAIL).expect("ok") {
            Normalized::Event { key, observed, .. } => {
                assert_eq!(key.harness.as_str(), Harness::GROK);
                assert_eq!(key.vendor_session_id, "grok-sess");
                match &observed.event {
                    Event::TurnEnded {
                        state: TurnState::InterruptedQuota { reason },
                        ..
                    } => assert!(reason.contains("rate_limit"), "{reason}"),
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_claude_payload_is_not_stolen_by_the_grok_adapter() {
        let json = r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi","prompt_id":"T"}"#;
        assert!(!looks_like(json));
        let Normalized::Event { key, .. } =
            normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("ok")
        else {
            panic!("event");
        };
        assert_eq!(key.harness.as_str(), Harness::CLAUDE_CODE);
    }
}
