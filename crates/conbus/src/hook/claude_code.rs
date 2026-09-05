//! Claude Code hook payloads, normalised onto the bus event model.
//!
//! Two sources of truth, and they are not equal. The extracted schema
//! definitions in `tests/fixtures/claude_code_2.1.241_contract.md` come from the
//! bundle's own `hook_event_name:Ct("...")` declarations; the captured payloads
//! beside them are real traffic. A `strings` dump is neither, and reading names
//! that way produced a `StopFailure` adapter that silently replaced the vendor's
//! reason with `"unspecified"` while every test passed.
//!
//! Which fields are pinned by which. Keep this exhaustive over `Payload` when
//! adding one — a field in neither list reads as verified and is not.
//!
//! Captured: `session_id`, `hook_event_name`, `cwd`, `source`, `prompt`,
//! `prompt_id`, `error`, `last_assistant_message`, `reason`.
//! Schema and emit site only: `agent_id`, `tool_name`, `tool_input`,
//! `tool_response`, `tool_use_id`, `duration_ms`, `is_interrupt`,
//! `error_details`, `tool_calls`, `notification_type`, `title`, `message`.
//! **No capture contains a `PostToolUse` at all**, and none has exercised a
//! subagent, a batch, a `PermissionRequest`, or a `Notification`.

use conbus_core::event::{
    Author, Body, Event, Harness, ToolOutcome, TurnState, VendorAgentId, VendorToolUseId,
    VendorTurnId,
};
use conbus_core::registry::{Observed, SessionKey};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::hook::{HookError, Normalized};

/// Whether the payload's `prompt_id` names a turn of *this* conversation.
///
/// `prompt_id` is one slot on a process-global request journal rather than a
/// per-event value, so recording it blindly can bind a record to a turn it does
/// not belong to. The dangerous case is narrow and specific: a `SessionStart`
/// whose source is `resume` or `fork` fires *before* the id is reset, so the
/// value there is the pre-resume conversation's — a link across two
/// conversations, which is the one thing the correlator must never invent.
///
/// Everywhere else the value belongs to the session in hand. `startup` and
/// `clear` carry none at all (`clear` resets first). `compact` carries the turn
/// the compaction happened inside, which is live. `SessionEnd` carries the turn
/// that just finished, which is this conversation's by the vendor's own
/// definition — "all subsequent events until the next prompt".
///
/// An unfamiliar `SessionStart` source is treated as unsafe: a missing
/// correlator costs a join, a wrong one corrupts the conversation graph.
fn turn_id_belongs_to_this_conversation(payload: &Payload) -> bool {
    if payload.hook_event_name != "SessionStart" {
        return true;
    }
    payload.source.as_deref() == Some("compact")
}

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
    /// Two vocabularies under one vendor key, told apart by `hook_event_name`.
    /// On `SessionStart`: startup | resume | clear | compact | fork.
    /// On `UserPromptSubmit`: the prompt's origin — never emitted by 2.1.241,
    /// whose two emit sites have it compiled out, but the schema says "payloads
    /// may omit it while the field rolls out", so read it and never depend on
    /// it. The values the schema lists here were taken on trust once and were
    /// wrong; the ones the vendor really writes are counted in `Author`, from
    /// the `promptSource` of 61,899 real prompts.
    #[serde(default)]
    pub source: Option<String>,
    /// Common base. The turn correlator; see `EVENTS_WITH_A_LIVE_TURN_ID`.
    #[serde(default)]
    pub prompt_id: Option<String>,
    /// Common base. Present only when the hook fired inside a subagent, whose
    /// events otherwise arrive under the parent's `session_id`.
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    /// `PostToolBatch`: every call in the batch that just resolved. A raw
    /// `Value` like the other vendor-shaped fields — a typed `Vec` here would
    /// make an unfamiliar shape cost the whole record, on every event, because
    /// this struct is shared by all of them.
    #[serde(default)]
    pub tool_calls: Option<Value>,
    /// `PostToolUse`, `PreToolUse`, `PostToolUseFailure`. Required by the
    /// schema on all three, so an absent one means the vendor changed — but
    /// nothing here detects that, and the record is written without it rather
    /// than rejected, because a hook that refuses a payload takes the turn down.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// `PostToolUseFailure`: the user pressed stop rather than the tool
    /// failing on its own.
    #[serde(default)]
    pub is_interrupt: Option<bool>,
    /// `PostToolUse`, `PostToolUseFailure`. Every `PostToolUse` path in 2.1.241
    /// passes a measurement; `PostToolUseFailure` does not always — a call that
    /// fails before it starts carries none. So an absent duration on a success
    /// means the vendor changed or the value arrived unreadable, and on a
    /// failure it may simply mean the call never ran.
    #[serde(default, deserialize_with = "lenient_millis")]
    pub duration_ms: Option<u64>,
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
    /// `SessionEnd`: clear | resume | logout | prompt_input_exit | other.
    /// `PermissionDenied`: why the harness refused. One vendor key, two
    /// vocabularies, told apart by `hook_event_name`.
    #[serde(default)]
    pub reason: Option<String>,
    /// `Notification`. Open string; the quota-lifted spelling is not pinned
    /// until a capture lands.
    #[serde(default)]
    pub notification_type: Option<String>,
    /// `Notification`.
    #[serde(default)]
    pub title: Option<String>,
    /// `Notification`.
    #[serde(default)]
    pub message: Option<String>,
}

/// Hook events this adapter has seen and decided not to mirror.
///
/// Spelled out so that an event which is *not* here is reported as unknown
/// rather than silently dropped. Read off the vendor's own event list in
/// `tests/fixtures/claude_code_2.1.241_contract.md`.
pub const DECLINED: [&str; 18] = [
    "UserPromptExpansion",
    "Setup",
    "SubagentStart",
    "PreCompact",
    "PostCompact",
    "TeammateIdle",
    "TaskCreated",
    "TaskCompleted",
    "Elicitation",
    "ElicitationResult",
    "ConfigChange",
    "InstructionsLoaded",
    "WorktreeCreate",
    "WorktreeRemove",
    "CwdChanged",
    "DirectoryAdded",
    "FileChanged",
    "MessageDisplay",
];

/// Pull tool-call ids out of whatever shape the vendor sent, and count what it
/// could not name.
///
/// Accepts the documented array of objects and a bare array of ids, and yields
/// nothing rather than failing for anything else. Reading the grouping is worth
/// having; it is not worth the record. The count is what keeps "no calls" and
/// "calls we could not read" from rendering as the same line.
fn batch_ids(value: Option<Value>) -> (Vec<VendorToolUseId>, usize) {
    let mut ids = Vec::new();
    let mut unreadable = 0;
    for call in value
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match call {
            Value::String(id) => ids.push(VendorToolUseId::new(id.as_str())),
            other => match other.get("tool_use_id").and_then(Value::as_str) {
                Some(id) => ids.push(VendorToolUseId::new(id)),
                None => unreadable += 1,
            },
        }
    }
    (ids, unreadable)
}

/// Read a hook payload into a bus event.
pub fn normalize(harness: &Harness, json: &str) -> Result<Normalized, HookError> {
    let payload: Payload = serde_json::from_str(json).map_err(|err| HookError(err.to_string()))?;

    let vendor_turn_id = turn_id_belongs_to_this_conversation(&payload)
        .then(|| payload.prompt_id.clone().map(VendorTurnId::new))
        .flatten();
    let vendor_agent_id = payload.agent_id.clone().map(VendorAgentId::new);

    let event = match payload.hook_event_name.as_str() {
        "SessionStart" => Event::SessionCandidate {
            trigger: payload.source,
        },
        "UserPromptSubmit" => Event::TurnStarted {
            author: payload.source.map(Author::new),
            prompt: Body::kept(payload.prompt.unwrap_or_default()),
        },
        "PreToolUse" => Event::ToolRequested {
            tool: payload.tool_name.unwrap_or_default(),
            tool_use_id: payload.tool_use_id.map(VendorToolUseId::new),
            input: body_of(payload.tool_input),
        },
        "PostToolBatch" => {
            let (tool_calls, unreadable) = batch_ids(payload.tool_calls);
            Event::ToolBatchEnded {
                tool_calls,
                unreadable,
            }
        }
        "PermissionRequest" => Event::PermissionAsked {
            tool: payload.tool_name.unwrap_or_default(),
            input: body_of(payload.tool_input),
        },
        "Notification" => Event::Notified {
            kind: payload.notification_type.unwrap_or_default(),
            title: payload.title,
            message: payload.message.unwrap_or_default(),
        },
        "PermissionDenied" => Event::ToolDenied {
            tool: payload.tool_name.unwrap_or_default(),
            tool_use_id: payload.tool_use_id.map(VendorToolUseId::new),
            input: body_of(payload.tool_input),
            reason: payload.reason.unwrap_or_default(),
        },
        "PostToolUse" => Event::ToolUse {
            tool: payload.tool_name.unwrap_or_default(),
            tool_use_id: payload.tool_use_id.map(VendorToolUseId::new),
            duration_ms: payload.duration_ms,
            input: body_of(payload.tool_input),
            outcome: ToolOutcome::Succeeded {
                output: body_of(payload.tool_response),
            },
        },
        // The vendor sends this *instead of* `PostToolUse`, and it carries no
        // `tool_response` — a call that fails would otherwise leave the log
        // with no record at all, making the tool history a record of successes.
        "PostToolUseFailure" => Event::ToolUse {
            tool: payload.tool_name.unwrap_or_default(),
            tool_use_id: payload.tool_use_id.map(VendorToolUseId::new),
            duration_ms: payload.duration_ms,
            input: body_of(payload.tool_input),
            outcome: ToolOutcome::Failed {
                error: Body::kept(payload.error.unwrap_or_default()),
                interrupted: payload.is_interrupt.unwrap_or(false),
            },
        },
        "Stop" => Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: payload.last_assistant_message.map(Body::kept),
        },
        "StopFailure" => Event::TurnEnded {
            state: failure_state(payload.error, payload.error_details),
            assistant_message: payload.last_assistant_message.map(Body::kept),
        },
        // A subagent's closing text was reaching no retention level at all,
        // while the main thread's identical text was kept. Its `agent_id` is
        // already on the envelope, so the record says whose turn ended.
        //
        // There is no matching `tool_requested`-style start: `SubagentStart`
        // carries no prompt, so mirroring it would write a turn that began with
        // nothing. The asymmetry is the vendor's, not ours.
        "SubagentStop" => Event::TurnEnded {
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
        hook_event_name: payload.hook_event_name,
        observed: Box::new(Observed {
            vendor_turn_id,
            vendor_agent_id,
            workspace: payload.cwd,
            event,
        }),
    })
}

/// Read a duration without ever failing the payload.
///
/// Every other field here is a string or a raw `Value` so that an unfamiliar
/// shape costs one field rather than the record. A typed number would break
/// that invariant, and the vendor left room for it: this field is declared
/// `number` with no integer constraint, unlike a dozen sibling duration fields
/// in the same bundle that do carry one. `Payload` is also one struct for every
/// hook event, so a strict parse here would let a future `duration_ms` on
/// `Stop` start dropping turn-end records.
///
/// Anything that is not a finite, non-negative number that fits is recorded as
/// no measurement at all, which is honest — we did not receive one we could
/// read — and costs nothing else on the line.
fn lenient_millis<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| value.as_f64())
        .filter(|ms| ms.is_finite() && *ms >= 0.0 && *ms <= u64::MAX as f64)
        .map(|ms| ms.round() as u64))
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

    fn observed_of(json: &str) -> Observed {
        match normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("normalise") {
            Normalized::Event {
                key: got, observed, ..
            } => {
                assert_eq!(got, key());
                *observed
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }

    fn event_of(json: &str) -> Event {
        observed_of(json).event
    }

    const TURN: &str = "ed06c859-f62e-418f-96f0-556d61fa97d5";

    #[test]
    fn the_turn_correlator_is_taken_from_every_event_that_carries_this_turns_id() {
        // Each payload is shaped the way the vendor actually sends it: only
        // `UserPromptSubmit` carries `prompt`, only `PostToolUse` carries
        // `tool_name`. Gating on the wrong field would pass a uniform payload.
        let payloads = [
            r#"{"session_id":"abc123","hook_event_name":"UserPromptSubmit","prompt":"p","prompt_id":"TURN"}"#,
            r#"{"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{},"tool_response":{},"prompt_id":"TURN"}"#,
            r#"{"session_id":"abc123","hook_event_name":"Stop","stop_hook_active":false,"prompt_id":"TURN"}"#,
            r#"{"session_id":"abc123","hook_event_name":"StopFailure","error":"rate_limit","prompt_id":"TURN"}"#,
            r#"{"session_id":"abc123","hook_event_name":"SessionEnd","reason":"other","prompt_id":"TURN"}"#,
            r#"{"session_id":"abc123","hook_event_name":"SessionStart","source":"compact","prompt_id":"TURN"}"#,
        ];

        for payload in payloads {
            let observed = observed_of(&payload.replace("TURN", TURN));

            assert_eq!(
                observed.vendor_turn_id.as_ref().map(VendorTurnId::as_str),
                Some(TURN),
                "dropped the correlator: {payload}"
            );
        }
    }

    #[test]
    fn a_resumed_or_forked_session_start_does_not_link_to_the_previous_conversation() {
        // These two fire before the process-global prompt id is reset, so the
        // value is the pre-resume conversation's. It is the one case where
        // recording it would invent a link across two conversations.
        for source in ["resume", "fork"] {
            let observed = observed_of(&format!(
                r#"{{"session_id":"abc123","hook_event_name":"SessionStart",
                    "source":"{source}","prompt_id":"{TURN}"}}"#
            ));

            assert_eq!(
                observed.vendor_turn_id, None,
                "a {source} session start kept another conversation's turn"
            );
        }
    }

    #[test]
    fn an_unfamiliar_session_start_source_is_treated_as_unsafe() {
        // A missing correlator costs a join; a wrong one corrupts the graph.
        let observed = observed_of(&format!(
            r#"{{"session_id":"abc123","hook_event_name":"SessionStart",
                "source":"some_new_source","prompt_id":"{TURN}"}}"#
        ));

        assert_eq!(observed.vendor_turn_id, None);
    }

    #[test]
    fn a_subagents_tool_call_is_marked_as_one() {
        // A subagent's hooks arrive under the PARENT session id, so without
        // this the log interleaves two agents' work under one conversation
        // with nothing on the line to separate them. `prompt_id` does not help:
        // it is the same value for a subagent and its parent.
        let observed = observed_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Read",
                "prompt_id":"ed06c859-f62e-418f-96f0-556d61fa97d5","agent_id":"agent-7",
                "agent_type":"code-reviewer"}"#,
        );

        assert_eq!(
            observed.vendor_agent_id.as_ref().map(VendorAgentId::as_str),
            Some("agent-7")
        );
    }

    #[test]
    fn a_main_thread_record_carries_no_agent_id() {
        let observed = observed_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Read"}"#,
        );

        assert_eq!(observed.vendor_agent_id, None);
    }

    #[test]
    fn the_author_of_a_turn_is_recorded_when_the_vendor_sends_one() {
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"UserPromptSubmit","prompt":"hi",
                "source":"sdk"}"#,
        );

        let Event::TurnStarted { author, .. } = event else {
            panic!("expected a started turn");
        };
        assert_eq!(author.as_ref().map(Author::as_str), Some(Author::SDK));
    }

    #[test]
    fn an_absent_author_stays_absent_rather_than_becoming_a_human() {
        // Claude Code 2.1.241 never emits `source` — both emit sites have it
        // compiled out — so this is the case that actually happens. Defaulting
        // it to `user` is how a scheduled wakeup gets counted as a person.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#,
        );

        assert_eq!(
            event,
            Event::TurnStarted {
                author: None,
                prompt: Body::kept("hi")
            }
        );
    }

    #[test]
    fn an_unknown_author_is_kept_verbatim() {
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"UserPromptSubmit","prompt":"hi",
                "source":"some_new_entrypoint"}"#,
        );

        let Event::TurnStarted { author, .. } = event else {
            panic!("expected a started turn");
        };
        assert_eq!(
            author.as_ref().map(Author::as_str),
            Some("some_new_entrypoint")
        );
        assert!(!author.expect("author").is_human());
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
                author: None,
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
            }
        );
    }

    #[test]
    fn a_tool_request_is_recorded_before_the_tool_runs() {
        // `PreToolUse` fires before execution, so it is a different moment
        // rather than a different description of the same one — which is why
        // it is its own event and not a third `ToolOutcome`.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PreToolUse","tool_name":"Bash",
                "tool_input":{"command":"cargo test"},"tool_use_id":"toolu_01ABC"}"#,
        );

        assert_eq!(
            event,
            Event::ToolRequested {
                tool: "Bash".into(),
                tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
                input: Body::kept(r#"{"command":"cargo test"}"#),
            }
        );
    }

    #[test]
    fn a_batch_records_which_calls_were_issued_together_and_nothing_else() {
        // The payload repeats every call's name, input and response — all of
        // which are already on disk from the per-call events. Mirroring those
        // would put each body on disk a third time. What is *not* already in
        // the log is the grouping and the boundary, so that is what is kept.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolBatch","tool_calls":[
                {"tool_name":"Read","tool_input":{"p":"a"},"tool_use_id":"toolu_A",
                 "tool_response":{"ok":true}},
                {"tool_name":"Bash","tool_input":{"command":"false"},"tool_use_id":"toolu_B"}]}"#,
        );

        assert_eq!(
            event,
            Event::ToolBatchEnded {
                tool_calls: vec![
                    VendorToolUseId::new("toolu_A"),
                    VendorToolUseId::new("toolu_B"),
                ],
                unreadable: 0,
            }
        );
    }

    #[test]
    fn a_tool_calls_shape_we_do_not_expect_costs_the_batch_not_the_record() {
        // `Payload` is one struct for every hook event and every other field is
        // a string or a raw `Value`, precisely so an unfamiliar shape costs one
        // field. A strictly typed `Vec` here brought that back: `"tool_calls":3`
        // on a `Stop` payload dropped the turn-end record entirely.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"Stop","stop_hook_active":false,
                "last_assistant_message":"done","tool_calls":3}"#,
        );
        assert_eq!(
            event,
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: Some(Body::kept("done")),
            }
        );

        // And a batch whose entries the vendor simplifies to bare ids still
        // yields the grouping rather than nothing.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolBatch",
                "tool_calls":["toolu_A","toolu_B"]}"#,
        );
        assert_eq!(
            event,
            Event::ToolBatchEnded {
                tool_calls: vec![
                    VendorToolUseId::new("toolu_A"),
                    VendorToolUseId::new("toolu_B"),
                ],
                unreadable: 0,
            }
        );
    }

    #[test]
    fn a_hook_event_we_have_never_seen_is_named_as_unknown() {
        // A vendor event the adapter declined on purpose and one it has never
        // heard of are different facts. Treating both as "ignored" means a new
        // event arrives, is dropped, and nothing anywhere says so.
        assert_eq!(
            normalize(
                &Harness::new(Harness::CLAUDE_CODE),
                r#"{"session_id":"abc123","hook_event_name":"PreCompact","trigger":"auto"}"#
            )
            .expect("normalise"),
            Normalized::Ignored {
                hook_event_name: "PreCompact".into()
            }
        );
        assert_eq!(
            normalize(
                &Harness::new(Harness::CLAUDE_CODE),
                r#"{"session_id":"abc123","hook_event_name":"SomethingNewInTwoPointTwo"}"#
            )
            .expect("normalise"),
            Normalized::Unknown {
                hook_event_name: "SomethingNewInTwoPointTwo".into()
            }
        );
    }

    #[test]
    fn an_empty_batch_is_still_a_boundary() {
        // The event fires before the next model request, so it marks where a
        // turn's tool work stopped even when it names nothing.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolBatch","tool_calls":[]}"#,
        );

        assert_eq!(
            event,
            Event::ToolBatchEnded {
                tool_calls: vec![],
                unreadable: 0,
            }
        );
    }

    #[test]
    fn a_batch_entry_the_vendor_sends_without_an_id_is_counted_not_ignored() {
        // "the batch held one call" and "the batch held two, one of which we
        // could not read" must not render as the same line: the second means
        // the vendor's shape moved.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolBatch","tool_calls":[
                {"tool_name":"Read","tool_input":{}},
                {"tool_name":"Bash","tool_input":{},"tool_use_id":"toolu_B"}]}"#,
        );

        assert_eq!(
            event,
            Event::ToolBatchEnded {
                tool_calls: vec![VendorToolUseId::new("toolu_B")],
                unreadable: 1,
            }
        );
    }

    #[test]
    fn a_permission_request_is_the_ask_not_the_refusal() {
        // Fires before the person answers. An ordinary no has no later event
        // (`PermissionDenied` is classifier-only), so the ask itself is the
        // evidence. Mapping it to `ToolDenied` would log every yes as a no.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PermissionRequest","tool_name":"Bash",
                "tool_input":{"command":"ls"}}"#,
        );

        assert_eq!(
            event,
            Event::PermissionAsked {
                tool: "Bash".into(),
                input: Body::kept(r#"{"command":"ls"}"#),
            }
        );
    }

    #[test]
    fn a_notification_keeps_the_vendor_kind_verbatim() {
        // The quota-lifted spelling is not pinned. Guessing it is how
        // `rate_limit_reached` became `"primary"` in a unit test.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"Notification",
                "notification_type":"example_kind","title":"a title","message":"a message"}"#,
        );

        assert_eq!(
            event,
            Event::Notified {
                kind: "example_kind".into(),
                title: Some("a title".into()),
                message: "a message".into(),
            }
        );
    }

    #[test]
    fn a_refused_tool_says_why_it_never_ran() {
        // Its own event rather than a third `ToolOutcome`, because `tool_use`
        // means the tool ran: a denied call in that set would skew any
        // aggregation over executions unless every reader filtered it out.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PermissionDenied","tool_name":"Bash",
                "tool_input":{"command":"rm -rf /"},"tool_use_id":"toolu_01ABC",
                "reason":"User rejected the command"}"#,
        );

        assert_eq!(
            event,
            Event::ToolDenied {
                tool: "Bash".into(),
                tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
                input: Body::kept(r#"{"command":"rm -rf /"}"#),
                reason: "User rejected the command".into(),
            }
        );
    }

    #[test]
    fn a_denial_resolves_the_request_it_answers() {
        let requested = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PreToolUse","tool_name":"Bash",
                "tool_input":{"command":"rm -rf /"},"tool_use_id":"toolu_01ABC"}"#,
        );
        let denied = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PermissionDenied","tool_name":"Bash",
                "tool_input":{"command":"rm -rf /"},"tool_use_id":"toolu_01ABC","reason":"no"}"#,
        );

        let (
            Event::ToolRequested {
                tool_use_id: asked, ..
            },
            Event::ToolDenied {
                tool_use_id: answered,
                ..
            },
        ) = (&requested, &denied)
        else {
            panic!("expected a request and a denial");
        };
        assert_eq!(asked, answered);
    }

    #[test]
    fn a_request_and_its_result_share_one_tool_use_id() {
        // This is the join the id exists for, and it did not exist until the
        // request side was mirrored: before this, a call produced exactly one
        // record and there was nothing to join it to.
        let requested = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PreToolUse","tool_name":"Bash",
                "tool_input":{"command":"cargo test"},"tool_use_id":"toolu_01ABC"}"#,
        );
        let finished = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Bash",
                "tool_input":{"command":"cargo test"},"tool_response":{"ok":true},
                "tool_use_id":"toolu_01ABC","duration_ms":4210}"#,
        );

        let (
            Event::ToolRequested {
                tool_use_id: asked, ..
            },
            Event::ToolUse {
                tool_use_id: got, ..
            },
        ) = (&requested, &finished)
        else {
            panic!("expected a request and a result");
        };
        assert_eq!(asked, got);
        assert!(asked.is_some(), "an unjoinable pair is the same as no pair");
    }

    #[test]
    fn a_tool_call_keeps_its_name_its_bodies_and_its_own_id() {
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Read",
                "tool_input":{"file_path":"/etc/hosts"},"tool_response":{"ok":true},
                "tool_use_id":"toolu_01ABC","duration_ms":37}"#,
        );

        assert_eq!(
            event,
            Event::ToolUse {
                tool: "Read".into(),
                tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
                duration_ms: Some(37),
                input: Body::kept(r#"{"file_path":"/etc/hosts"}"#),
                outcome: ToolOutcome::Succeeded {
                    output: Body::kept(r#"{"ok":true}"#),
                },
            }
        );
    }

    #[test]
    fn a_duration_the_vendor_spells_differently_costs_the_duration_not_the_record() {
        // Every other field on `Payload` is a string or a raw `Value` so that a
        // shape surprise costs one field. A typed number breaks that invariant:
        // the vendor declares this `number` with no integer constraint — unlike
        // a dozen sibling duration fields in the same bundle, which do carry
        // `.int()` — so a fractional millisecond is legal, and rejecting it
        // would take the whole tool call out of the append-only log.
        for (spelling, expected) in [
            ("37", Some(37)),
            ("37.0", Some(37)),
            ("37.5", Some(38)),
            ("-3", None),
            ("\"37\"", None),
            ("null", None),
            ("1e300", None),
        ] {
            let event = event_of(&format!(
                r#"{{"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Read",
                    "tool_input":{{}},"tool_response":{{}},"tool_use_id":"toolu_01ABC",
                    "duration_ms":{spelling}}}"#
            ));

            let Event::ToolUse {
                tool,
                tool_use_id,
                duration_ms,
                ..
            } = event
            else {
                panic!("a {spelling} duration cost the whole record");
            };
            assert_eq!(duration_ms, expected, "duration_ms:{spelling}");
            assert_eq!(tool, "Read", "duration_ms:{spelling} lost the tool name");
            assert_eq!(
                tool_use_id,
                Some(VendorToolUseId::new("toolu_01ABC")),
                "duration_ms:{spelling} lost the tool use id"
            );
        }
    }

    #[test]
    fn a_strange_duration_on_another_event_does_not_cost_that_record_either() {
        // `Payload` is one struct for every hook event, so a `duration_ms` the
        // vendor later adds to `Stop` in an unexpected shape must not start
        // dropping the turn-end records the router depends on.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"UserPromptSubmit","prompt":"hi",
                "duration_ms":37.5}"#,
        );

        assert_eq!(
            event,
            Event::TurnStarted {
                author: None,
                prompt: Body::kept("hi"),
            }
        );
    }

    #[test]
    fn a_tool_call_without_a_duration_records_no_duration_rather_than_zero() {
        // `duration_ms` is optional on the wire — the emit site passes a
        // parameter that can be undefined, which JSON drops. Zero is a
        // measurement; absent is not, and a survival meter must tell them apart.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolUse","tool_name":"Read",
                "tool_input":{},"tool_response":{},"tool_use_id":"toolu_01ABC"}"#,
        );

        let Event::ToolUse {
            duration_ms,
            tool_use_id,
            ..
        } = event
        else {
            panic!("expected a tool call");
        };
        assert_eq!(duration_ms, None);
        assert_eq!(tool_use_id, Some(VendorToolUseId::new("toolu_01ABC")));
    }

    #[test]
    fn a_failed_tool_call_is_recorded_as_the_same_call_with_a_different_outcome() {
        // The vendor sends `PostToolUse` or `PostToolUseFailure` for one call,
        // never both, so a failure is the same event with a different ending —
        // and there is no `tool_response` on it, only an `error`.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolUseFailure","tool_name":"Bash",
                "tool_input":{"command":"false"},"tool_use_id":"toolu_01ABC",
                "error":"exit status 1","duration_ms":12}"#,
        );

        assert_eq!(
            event,
            Event::ToolUse {
                tool: "Bash".into(),
                tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
                duration_ms: Some(12),
                input: Body::kept(r#"{"command":"false"}"#),
                outcome: ToolOutcome::Failed {
                    error: Body::kept("exit status 1"),
                    interrupted: false,
                },
            }
        );
    }

    #[test]
    fn a_tool_call_the_user_interrupted_says_so() {
        // `is_interrupt` is the only signal in the whole hook surface that a
        // person stopped the work. Nothing derives `TurnState::InterruptedUser`
        // from it yet, but losing it would remove the only evidence.
        let event = event_of(
            r#"{"session_id":"abc123","hook_event_name":"PostToolUseFailure","tool_name":"Bash",
                "tool_input":{},"tool_use_id":"toolu_01ABC","error":"interrupted",
                "is_interrupt":true}"#,
        );

        let Event::ToolUse { outcome, .. } = event else {
            panic!("expected a tool call");
        };
        assert_eq!(
            outcome,
            ToolOutcome::Failed {
                error: Body::kept("interrupted"),
                interrupted: true,
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
    fn a_subagents_closing_text_reaches_the_log_like_the_main_thread_s() {
        let observed = observed_of(
            r#"{"session_id":"abc123","hook_event_name":"SubagentStop","stop_hook_active":false,
                "agent_id":"agent-7","agent_type":"code-reviewer",
                "agent_transcript_path":"/t.jsonl","last_assistant_message":"reviewed."}"#,
        );

        assert_eq!(
            observed.event,
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: Some(Body::kept("reviewed.")),
            }
        );
        assert_eq!(
            observed.vendor_agent_id.as_ref().map(VendorAgentId::as_str),
            Some("agent-7"),
            "without the agent id the record reads as the main thread's turn ending"
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
            &Harness::new(Harness::CLAUDE_CODE),
            r#"{"session_id":"abc123","hook_event_name":"PreCompact","trigger":"auto"}"#,
        )
        .expect("normalise");

        assert_eq!(
            normalized,
            Normalized::Ignored {
                hook_event_name: "PreCompact".into()
            }
        );
    }

    #[test]
    fn a_payload_without_a_session_id_is_an_error_not_a_guess() {
        let err = normalize(
            &Harness::new(Harness::CLAUDE_CODE),
            r#"{"hook_event_name":"Stop"}"#,
        )
        .expect_err("must fail");

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
                author: None,
                prompt: Body::kept("hi")
            }
        );
    }
}
