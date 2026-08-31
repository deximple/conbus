//! The `conbus hook` client: vendor hook payloads in, bus events out.

pub mod claude_code;
pub mod client;
pub mod codex;

use conbus_core::event::Harness;
use conbus_core::registry::{Observed, SessionKey};

/// The result of reading a hook payload.
#[derive(Debug, PartialEq, Eq)]
pub enum Normalized {
    Event {
        key: SessionKey,
        /// Boxed only to keep the two variants a similar size; one of these is
        /// built per hook process and consumed immediately.
        observed: Box<Observed>,
    },
    /// A hook event the recorder deliberately does not mirror.
    Ignored { hook_event_name: String },
}

/// A payload we could not read at all.
#[derive(Debug, PartialEq, Eq)]
pub struct HookError(pub String);

/// Read a payload with the adapter that matches the harness that sent it.
///
/// Dispatching on the harness is load-bearing rather than tidy. Every field on
/// every `Payload` here is optional by design, so the wrong adapter does not
/// fail — it succeeds and silently drops whatever it does not recognise. Codex
/// sends its correlator as `turn_id` and Claude Code as `prompt_id`, so parsing
/// one with the other's adapter produces records with no correlator at all and
/// a full green test suite.
///
/// An unfamiliar harness is refused rather than guessed at. Cursor, Copilot CLI
/// and Devin CLI read Claude Code's settings format, so they will most likely
/// share that adapter — but "most likely" is how the wrong parser gets chosen,
/// and none of their payloads have been seen yet.
pub fn normalize(harness: &Harness, payload: &str) -> Result<Normalized, HookError> {
    match harness.as_str() {
        Harness::CLAUDE_CODE => claude_code::normalize(harness, payload),
        Harness::CODEX => codex::normalize(harness, payload),
        other => Err(HookError(format!(
            "no adapter for harness {other:?}; conbus reads claude_code and codex"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same prompt, spelled the way each vendor spells it.
    const CLAUDE: &str = r#"{"session_id":"s","hook_event_name":"UserPromptSubmit",
        "prompt":"hi","prompt_id":"CLAUDE-TURN"}"#;
    const CODEX: &str = r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","cwd":"/repo",
        "model":"gpt-5-codex","permission_mode":"default","prompt":"hi","transcript_path":null,
        "turn_id":"CODEX-TURN"}"#;

    fn turn_id_of(harness: &str, payload: &str) -> Option<String> {
        match normalize(&Harness::new(harness), payload).expect("normalise") {
            Normalized::Event { observed, .. } => observed
                .vendor_turn_id
                .as_ref()
                .map(|id| id.as_str().to_string()),
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn each_harness_is_read_by_its_own_adapter() {
        assert_eq!(
            turn_id_of(Harness::CLAUDE_CODE, CLAUDE),
            Some("CLAUDE-TURN".into())
        );
        assert_eq!(turn_id_of(Harness::CODEX, CODEX), Some("CODEX-TURN".into()));
    }

    #[test]
    fn the_wrong_adapter_would_lose_the_correlator_without_failing() {
        // Both of these parse. Neither errors. Both drop the correlator. This
        // is why the dispatch above exists and why the two adapters do not
        // share a `Payload`.
        assert_eq!(turn_id_of(Harness::CODEX, CLAUDE), None);
        assert_eq!(turn_id_of(Harness::CLAUDE_CODE, CODEX), None);
    }

    #[test]
    fn the_harness_that_asked_is_the_harness_that_is_stamped() {
        let Normalized::Event { key, .. } =
            normalize(&Harness::new(Harness::CODEX), CODEX).expect("normalise")
        else {
            panic!("expected an event");
        };

        assert_eq!(key.harness.as_str(), Harness::CODEX);
    }

    #[test]
    fn an_unknown_harness_is_refused_rather_than_guessed_at() {
        let err = normalize(&Harness::new("cursor"), CLAUDE).expect_err("must refuse");

        assert!(err.0.contains("cursor"), "unhelpful error: {}", err.0);
    }
}
