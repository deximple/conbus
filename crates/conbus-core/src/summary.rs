//! Rolling digest of a conversation.
//!
//! Hop-path model calls are zero. The digest written on the live path is
//! extractive: last prompt, last reply, unanswered tools — the same digest
//! the pack marks `kind="summary"`. A model-backed
//! refresh is a separate command, and it is forbidden from using the leg
//! it is protecting.

use crate::event::Harness;
use crate::pack::Chapter;

/// How many completed turns between extractive refreshes.
pub const EVERY_N_TURNS: u64 = 8;

pub const EXTRACTIVE: &str = "extractive";

/// Which backend may produce a digest for a conversation on `protecting`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Backend {
    Extractive,
    /// Opposite vendor's low-effort model. Never the protected leg.
    Opposite {
        harness: Harness,
    },
}

/// Pick a backend. The protected leg is never a candidate.
pub fn backend(protecting: &Harness, opposite_usable: bool) -> Backend {
    match opposite(protecting) {
        Some(other) if opposite_usable => Backend::Opposite { harness: other },
        _ => Backend::Extractive,
    }
}

fn opposite(protecting: &Harness) -> Option<Harness> {
    crate::route::opposite(protecting)
}

/// True when this turn count is a refresh boundary.
pub fn due(turns: u64) -> bool {
    turns > 0 && turns.is_multiple_of(EVERY_N_TURNS)
}

/// A short extractive digest. No model. Same text the pack marks `summary`.
pub fn extract(chapter: &Chapter) -> String {
    chapter.digest()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuation::Candidate;
    use crate::event::Harness;
    use crate::ids::BusId;

    fn chapter() -> Chapter {
        Chapter {
            about: Candidate {
                bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
                harness: Harness::new(Harness::CLAUDE_CODE),
                workspace: None,
                last_activity: "2026-09-06T00:00:00.000Z".parse().expect("ts"),
                last_turn: None,
                turns: 8,
                backfilled: false,
                live: true,
            },
            last_prompt: Some("ship the router".into()),
            last_said: Some("working on it".into()),
            pending_hop: None,
            code_freshness: None,
            tool_calls: 2,
            tools_unanswered: 1,
            claude_profile: None,
        }
    }

    #[test]
    fn a_refresh_is_due_every_eight_turns_not_on_zero() {
        assert!(!due(0));
        assert!(!due(7));
        assert!(due(8));
        assert!(due(16));
    }

    #[test]
    fn the_protected_leg_is_never_the_backend() {
        match backend(&Harness::new(Harness::CLAUDE_CODE), true) {
            Backend::Opposite { harness } => assert_eq!(harness.as_str(), Harness::CODEX),
            other => panic!("{other:?}"),
        }
        match backend(&Harness::new(Harness::CODEX), true) {
            Backend::Opposite { harness } => assert_eq!(harness.as_str(), Harness::CLAUDE_CODE),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            backend(&Harness::new(Harness::CODEX), false),
            Backend::Extractive
        );
    }

    #[test]
    fn extractive_names_the_tail_and_the_unresolved() {
        let text = extract(&chapter());
        assert!(text.contains("ship the router"), "{text}");
        assert!(text.contains("working on it"), "{text}");
        assert!(text.contains("never resolved"), "{text}");
        let _ = BusId::mint();
    }
}
