//! Grok hook payloads.
//!
//! Official docs (`~/.grok/docs/user-guide/10-hooks.md`) send Claude's
//! `hook_event_name` plus Grok's own camelCase twins (`sessionId`,
//! `hookEventName`, `errorDetails`). `StopFailure.error` is `rate_limit` —
//! the same classified quota signal Claude uses. There is no remaining-quota
//! JSON; this is the overflow trigger.
//!
//! Grok also runs Claude Code's registration of this hook, so every Grok event
//! arrives twice. The copy that did not come through `--harness grok` is
//! answered and not recorded (`client::grok_echo`).

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
    let event = object
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reason = object
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    // 1.0.71 fires a second `Stop` after `SessionEnd`, with reason `shutdown`
    // and no prompt id. It names no turn and ends none.
    if event == "Stop" && reason.as_deref() == Some("shutdown") && !object.contains_key("prompt_id")
    {
        return Ok(Normalized::Ignored {
            hook_event_name: event,
        });
    }
    // `StopCancelled` runs instead of `Stop` when a turn ends without
    // completing. It is a `Stop` with an outcome: read it as one, then say how
    // it ended.
    if event == "StopCancelled" {
        object.insert("hook_event_name".into(), Value::String("Stop".into()));
        let normalized = claude_code::normalize(&Harness::new(Harness::GROK), &value.to_string())?;
        return Ok(cancelled(
            normalized,
            reason.unwrap_or_else(|| "unknown".into()),
        ));
    }
    claude_code::normalize(&Harness::new(Harness::GROK), &value.to_string())
}

/// A person stopping the turn is `InterruptedUser`; any other reason Grok
/// gives (`max_turns`, `no_progress`, `hook_stopped`, `unknown`) is kept
/// verbatim as a policy interruption.
fn cancelled(normalized: Normalized, reason: String) -> Normalized {
    let Normalized::Event {
        key, mut observed, ..
    } = normalized
    else {
        return normalized;
    };
    if let conbus_core::event::Event::TurnEnded { state, .. } = &mut observed.event {
        *state = match reason.as_str() {
            "user_interrupt" | "permission_rejected" | "permission_cancelled" => {
                conbus_core::event::TurnState::InterruptedUser
            }
            _ => conbus_core::event::TurnState::InterruptedPolicy { reason },
        };
    }
    Normalized::Event {
        key,
        hook_event_name: "StopCancelled".into(),
        observed,
    }
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

    /// Grok 1.0.71 as captured on 2026-11-01: after `SessionEnd` it fires a
    /// second `Stop` with `reason: "shutdown"` and no prompt id.
    const SHUTDOWN_STOP: &str = r#"{
        "hookEventName": "Stop", "hook_event_name": "Stop",
        "sessionId": "grok-sess", "session_id": "grok-sess",
        "reason": "shutdown", "stopHookActive": false, "stop_hook_active": false,
        "cwd": "/repo", "workspaceRoot": "/repo"
    }"#;

    fn cancelled(reason: &str) -> String {
        format!(
            r#"{{"hookEventName":"StopCancelled","hook_event_name":"StopCancelled",
            "sessionId":"grok-sess","session_id":"grok-sess","promptId":"turn-1",
            "reason":"{reason}","cwd":"/repo"}}"#
        )
    }

    #[test]
    fn grok_s_stop_at_shutdown_is_not_a_turn_end() {
        // It follows the session's end and names no turn; read as a `Stop` it
        // wrote a second `turn_ended` after every `session_ended`.
        assert!(matches!(
            normalize(&Harness::new(Harness::GROK), SHUTDOWN_STOP).expect("ok"),
            Normalized::Ignored { .. }
        ));
    }

    #[test]
    fn a_grok_turn_that_ends_without_completing_is_recorded() {
        // `StopCancelled` runs instead of `Stop` when a turn ends without
        // completing (Grok's hook docs). A person stopping it is the only
        // vendor signal anywhere that says so; anything else is the vendor's
        // own reason, kept verbatim.
        let state = |reason: &str| match normalize(&Harness::new(Harness::GROK), &cancelled(reason))
            .expect("ok")
        {
            Normalized::Event { observed, .. } => match observed.event {
                Event::TurnEnded { state, .. } => state,
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        for person in [
            "user_interrupt",
            "permission_rejected",
            "permission_cancelled",
        ] {
            assert_eq!(state(person), TurnState::InterruptedUser, "{person}");
        }
        assert_eq!(
            state("max_turns"),
            TurnState::InterruptedPolicy {
                reason: "max_turns".into()
            }
        );
    }
}
