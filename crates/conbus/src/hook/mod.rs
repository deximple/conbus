//! The `conbus hook` client: vendor hook payloads in, bus events out.

pub mod claude_code;
pub mod client;
pub mod codex;
pub mod cursor;
pub mod grok;
pub mod inference;
pub mod opencode;

use conbus_core::event::Harness;
use conbus_core::registry::{Observed, SessionKey};

/// The result of reading a hook payload.
#[derive(Debug, PartialEq, Eq)]
pub enum Normalized {
    Event {
        key: SessionKey,
        /// The vendor's own name for the event that fired, carried through
        /// because a reply that injects has to name it back.
        ///
        /// Claude Code requires `hookSpecificOutput.hookEventName` to equal the
        /// firing event and throws away the entire hook result when it does
        /// not — non-blocking, so the turn is fine and the injection simply
        /// never happens. Worse, the same failure is reported once per session
        /// and then suppressed, so a hardcoded name that drifts would show one
        /// error and go quiet forever.
        hook_event_name: String,
        /// Boxed only to keep the two variants a similar size; one of these is
        /// built per hook process and consumed immediately.
        observed: Box<Observed>,
    },
    /// A hook event the recorder has seen and deliberately does not mirror.
    Ignored { hook_event_name: String },
    /// A hook event this adapter has never heard of.
    ///
    /// Distinct from `Ignored` on purpose: a decision and a surprise are
    /// different facts, and collapsing them means a vendor adds an event, the
    /// recorder drops it, and nothing anywhere says so.
    Unknown { hook_event_name: String },
}

/// Why a payload that names no session is refused, on both sides of the socket.
///
/// An empty id is not an identity. From 2026-09-10 Cursor's worker-shaped
/// sessions sent their tool hooks with an empty conversation id, and once a
/// Cursor tool call was allowed to start a conversation, `(cursor, "")` became
/// one conversation that collected 877 tool records from two unrelated
/// sessions — while the dashboard counted zero orphans, because nothing was
/// orphaned any more. Dropping such a payload loses one record, visibly;
/// keying it merges strangers, silently, for as long as the machine runs.
pub const NO_SESSION: &str = "the hook payload names no session; an empty id would merge unrelated sessions into one conversation";

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
/// An unfamiliar harness is refused rather than guessed at. Cursor reads
/// Claude Code's settings file and invokes this hook under that name, so
/// dispatch is by payload (`cursor_version`), not by the argv harness. The
/// fields were pinned by the live drop log, not by Claude's schema.
pub fn normalize(harness: &Harness, payload: &str) -> Result<Normalized, HookError> {
    if inference::looks_like(payload) {
        return inference::normalize(payload);
    }
    if cursor::looks_like(payload) {
        return cursor::normalize(&Harness::new(Harness::CURSOR), payload);
    }
    if grok::looks_like(payload) {
        return grok::normalize(payload);
    }
    if opencode::looks_like(payload) {
        return opencode::normalize(payload);
    }
    match harness.as_str() {
        Harness::CLAUDE_CODE => claude_code::normalize(harness, payload),
        Harness::CODEX => codex::normalize(harness, payload),
        Harness::CURSOR => cursor::normalize(harness, payload),
        Harness::GROK => grok::normalize(payload),
        Harness::OPENCODE => opencode::normalize(payload),
        Harness::CLAUDE_CHAT => claude_code::normalize(harness, payload),
        other => Err(HookError(format!(
            "no adapter for harness {other:?}; conbus reads claude_code, claude_chat, codex, cursor, grok, and opencode"
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
        let err = normalize(&Harness::new("devin"), CLAUDE).expect_err("must refuse");

        assert!(err.0.contains("devin"), "unhelpful error: {}", err.0);
    }
}
