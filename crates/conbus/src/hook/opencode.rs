//! OpenCode plugin events, normalised onto the bus.
//!
//! OpenCode has no remaining-quota JSON. The official plugin bus publishes
//! `session.error` with `APIError.data.statusCode` and `responseBody`
//! (`GoUsageLimitError`, `account_rate_limit`). That is the overflow trigger.
//! The in-process plugin writes this envelope to `conbus hook --harness opencode`.

use serde::Deserialize;
use serde_json::Value;

use conbus_core::event::{Body, Event, Harness, TurnState, VendorTurnId};
use conbus_core::registry::{Observed, SessionKey};

use super::{HookError, Normalized};

/// Installer-stamped marker. Claude/Codex/Grok never send this key.
pub fn looks_like(payload: &str) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    fields.get("opencode_plugin").and_then(Value::as_u64) == Some(1)
}

#[derive(Debug, Deserialize)]
struct Payload {
    hook_event_name: String,
    session_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    error: Option<Value>,
}

pub fn normalize(json: &str) -> Result<Normalized, HookError> {
    let payload: Payload = serde_json::from_str(json).map_err(|err| HookError(err.to_string()))?;
    let event = match payload.hook_event_name.as_str() {
        "session.created" => Event::SessionCandidate { trigger: None },
        // The plugin decides what a prompt is and sends each one once, as
        // `message.part.updated` (install/opencode_plugin.js). `message.updated`
        // is what the 0.2.3 plugin also sent, once per agent step; it stays
        // readable because machines keep that plugin until `install --apply`.
        "message.updated" | "message.part.updated" => Event::TurnStarted {
            author: None,
            prompt: Body::kept(payload.prompt.unwrap_or_default()),
        },
        "session.error" => Event::TurnEnded {
            state: error_state(payload.error.as_ref()),
            assistant_message: None,
            tokens: None,
        },
        other => {
            return Ok(Normalized::Unknown {
                hook_event_name: other.to_string(),
            });
        }
    };
    Ok(Normalized::Event {
        key: SessionKey::new(Harness::new(Harness::OPENCODE), payload.session_id),
        hook_event_name: payload.hook_event_name,
        observed: Box::new(Observed {
            vendor_turn_id: payload.message_id.map(VendorTurnId::new),
            vendor_agent_id: None,
            workspace: payload.cwd,
            event,
        }),
    })
}

fn error_state(error: Option<&Value>) -> TurnState {
    match quota_reason(error) {
        Some(reason) => TurnState::InterruptedQuota { reason },
        None => TurnState::InterruptedPolicy {
            reason: error
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("error")
                .into(),
        },
    }
}

fn quota_reason(error: Option<&Value>) -> Option<String> {
    let error = error?;
    let name = error.get("name").and_then(Value::as_str).unwrap_or("");
    let data = error.get("data");
    let status = data
        .and_then(|d| d.get("statusCode"))
        .and_then(Value::as_u64);
    let body = data
        .and_then(|d| d.get("responseBody"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let message = data
        .and_then(|d| d.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if status == Some(429)
        || name == "QuotaExceededError"
        || body.contains("GoUsageLimitError")
        || body.contains("account_rate_limit")
        || message.contains("account_rate_limit")
    {
        Some(if body.contains("GoUsageLimitError") {
            "GoUsageLimitError".into()
        } else if status == Some(429) {
            "rate_limit".into()
        } else {
            "account_rate_limit".into()
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::normalize;

    const LIMIT: &str = r#"{
        "opencode_plugin": 1,
        "hook_event_name": "session.error",
        "session_id": "ses_1",
        "cwd": "/repo",
        "error": {
            "name": "APIError",
            "data": {
                "statusCode": 429,
                "isRetryable": true,
                "responseBody": "GoUsageLimitError",
                "message": "account_rate_limit"
            }
        }
    }"#;

    #[test]
    fn a_429_go_limit_is_the_quota_interruption() {
        match normalize(&Harness::new(Harness::CLAUDE_CODE), LIMIT).expect("ok") {
            Normalized::Event { key, observed, .. } => {
                assert_eq!(key.harness.as_str(), Harness::OPENCODE);
                assert_eq!(key.vendor_session_id, "ses_1");
                match &observed.event {
                    Event::TurnEnded {
                        state: TurnState::InterruptedQuota { reason },
                        ..
                    } => assert!(
                        reason.contains("GoUsageLimitError") || reason == "rate_limit",
                        "{reason}"
                    ),
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_auth_error_is_not_quota() {
        let json = r#"{
            "opencode_plugin": 1,
            "hook_event_name": "session.error",
            "session_id": "ses_1",
            "error": {"name": "ProviderAuthError", "data": {"providerID": "x", "message": "no"}}
        }"#;
        match normalize(&Harness::new(Harness::OPENCODE), json).expect("ok") {
            Normalized::Event { observed, .. } => match &observed.event {
                Event::TurnEnded {
                    state: TurnState::InterruptedPolicy { .. },
                    ..
                } => {}
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_prompt_is_a_turn_under_the_id_of_its_message() {
        // A `msg_…` id names exactly one message, which is what lets the
        // plugin send a person's message once and the log tell a re-report.
        let json = r#"{
            "opencode_plugin": 1,
            "hook_event_name": "message.part.updated",
            "session_id": "ses_1",
            "cwd": "/repo",
            "message_id": "msg_1",
            "prompt": "the checkout test is flaky"
        }"#;
        match normalize(&Harness::new(Harness::OPENCODE), json).expect("ok") {
            Normalized::Event { key, observed, .. } => {
                assert_eq!(key.vendor_session_id, "ses_1");
                assert_eq!(
                    observed.vendor_turn_id.as_ref().map(VendorTurnId::as_str),
                    Some("msg_1")
                );
                assert_eq!(
                    observed.event,
                    Event::TurnStarted {
                        author: None,
                        prompt: Body::kept("the checkout test is flaky"),
                    }
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_claude_payload_is_not_stolen() {
        let json = r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi","prompt_id":"T"}"#;
        assert!(!looks_like(json));
    }
}
