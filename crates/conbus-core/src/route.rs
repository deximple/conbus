//! Which harness should serve the next turn.
//!
//! Pure. The daemon records a `routed` line when a turn already died of
//! quota; `conbus hop` asks before the next turn, because that is when the
//! Codex account signal exists — Codex hooks have no `StopFailure`.

use crate::attach;
use crate::event::{Harness, Hop, TurnState};

/// Move before the window is gone, not after. The plan's example: 80% used
/// and more than two hours to reset.
pub const PREDICTIVE_USED_PERCENT: u64 = 80;
pub const PREDICTIVE_RESET_MINUTES: i64 = 120;

/// One leg as the router sees it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Leg {
    pub harness: Harness,
    /// How the last mirrored turn ended. `None` means in flight or never.
    pub last_turn: Option<TurnState>,
    /// Primary window, when a probe or StatusLine observation has read it.
    pub used_percent: Option<u64>,
    pub minutes_to_reset: Option<i64>,
    /// The account is already refusing work.
    pub reached: bool,
}

/// What to do with the next turn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Decision {
    Stay,
    Hop {
        to: Harness,
        hop: Hop,
        reason: String,
    },
    /// Both legs are unusable. Wait for a reset rather than hop into a wall.
    Wait {
        reason: String,
    },
}

/// The other dual-warm leg, if there is one.
pub fn opposite(harness: &Harness) -> Option<Harness> {
    match harness.as_str() {
        Harness::CLAUDE_CODE => Some(Harness::new(Harness::CODEX)),
        Harness::CODEX => Some(Harness::new(Harness::CLAUDE_CODE)),
        _ => None,
    }
}

/// Where auto overflow may land. Directed hop may name a catalog leg this
/// does not return.
pub fn overflow_target(harness: &Harness) -> Option<Harness> {
    let dest = attach::overflow(harness.as_str())?;
    let caps = attach::caps(dest)?;
    caps.usable_as_target.then(|| Harness::new(dest))
}

/// Decide the next leg from two snapshots. No I/O.
pub fn decide(current: &Leg, other: &Leg) -> Decision {
    let Some(to) = overflow_target(&current.harness) else {
        return Decision::Stay;
    };
    if other.harness != to {
        return Decision::Stay;
    }

    let current_down =
        current.reached || matches!(current.last_turn, Some(TurnState::InterruptedQuota { .. }));
    if current_down {
        return if other.reached {
            Decision::Wait {
                reason: "both legs exhausted".into(),
            }
        } else {
            Decision::Hop {
                to,
                hop: Hop::Displaced,
                reason: if current.reached {
                    "current account exhausted".into()
                } else {
                    "current turn hit quota".into()
                },
            }
        };
    }

    if other.reached {
        return Decision::Stay;
    }

    let used = current.used_percent.unwrap_or(0);
    let reset = current.minutes_to_reset.unwrap_or(0);
    if used >= PREDICTIVE_USED_PERCENT && reset > PREDICTIVE_RESET_MINUTES {
        return Decision::Hop {
            to,
            hop: Hop::HandedOff,
            reason: format!("{used}% used, reset in {reset}m"),
        };
    }

    Decision::Stay
}

impl Decision {
    pub fn describe(&self, current: &Harness) -> String {
        match self {
            Decision::Stay => format!("stay on {}", current.as_str()),
            Decision::Hop { to, hop, reason } => {
                let kind = match hop {
                    Hop::Displaced => "displaced",
                    Hop::HandedOff => "handed_off",
                };
                format!(
                    "hop {kind} {} → {} ({reason})",
                    current.as_str(),
                    to.as_str()
                )
            }
            Decision::Wait { reason } => format!("wait ({reason})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> Leg {
        Leg {
            harness: Harness::new(Harness::CLAUDE_CODE),
            last_turn: Some(TurnState::Complete),
            used_percent: None,
            minutes_to_reset: None,
            reached: false,
        }
    }

    fn codex() -> Leg {
        Leg {
            harness: Harness::new(Harness::CODEX),
            last_turn: Some(TurnState::Complete),
            used_percent: Some(22),
            minutes_to_reset: Some(180),
            reached: false,
        }
    }

    #[test]
    fn a_quota_stop_hops_displaced_to_the_other_leg() {
        let mut current = claude();
        current.last_turn = Some(TurnState::InterruptedQuota {
            reason: "rate_limit".into(),
        });

        match decide(&current, &codex()) {
            Decision::Hop { to, hop, .. } => {
                assert_eq!(to.as_str(), Harness::CODEX);
                assert_eq!(hop, Hop::Displaced);
            }
            other => panic!("expected a hop, got {other:?}"),
        }
    }

    #[test]
    fn an_exhausted_account_is_the_same_signal_without_a_turn() {
        // Codex: no StopFailure. The probe is the turn.
        let mut current = codex();
        current.reached = true;
        current.used_percent = Some(100);

        match decide(&current, &claude()) {
            Decision::Hop { to, hop, reason } => {
                assert_eq!(to.as_str(), Harness::CLAUDE_CODE);
                assert_eq!(hop, Hop::Displaced);
                assert!(reason.contains("exhausted"), "{reason}");
            }
            other => panic!("expected a hop, got {other:?}"),
        }
    }

    #[test]
    fn eighty_percent_and_a_long_reset_hands_off_before_the_wall() {
        let mut current = codex();
        current.used_percent = Some(80);
        current.minutes_to_reset = Some(121);

        match decide(&current, &claude()) {
            Decision::Hop { hop, .. } => assert_eq!(hop, Hop::HandedOff),
            other => panic!("expected a predictive hop, got {other:?}"),
        }
    }

    #[test]
    fn eighty_percent_with_a_reset_soon_stays() {
        let mut current = codex();
        current.used_percent = Some(90);
        current.minutes_to_reset = Some(PREDICTIVE_RESET_MINUTES);

        assert_eq!(decide(&current, &claude()), Decision::Stay);
    }

    #[test]
    fn hopping_into_an_exhausted_leg_waits_instead() {
        let mut current = claude();
        current.last_turn = Some(TurnState::InterruptedQuota {
            reason: "rate_limit".into(),
        });
        let mut other = codex();
        other.reached = true;

        match decide(&current, &other) {
            Decision::Wait { reason } => assert!(reason.contains("both"), "{reason}"),
            other => panic!("expected a wait, got {other:?}"),
        }
    }

    #[test]
    fn a_healthy_current_does_not_leave_because_the_other_is_down() {
        let mut other = codex();
        other.reached = true;
        assert_eq!(decide(&claude(), &other), Decision::Stay);
    }

    #[test]
    fn a_policy_stop_is_not_a_quota_hop() {
        let mut current = claude();
        current.last_turn = Some(TurnState::InterruptedPolicy {
            reason: "billing_error".into(),
        });
        assert_eq!(decide(&current, &codex()), Decision::Stay);
    }

    #[test]
    fn an_unknown_harness_has_no_opposite() {
        assert!(opposite(&Harness::new("cursor")).is_none());
    }

    #[test]
    fn a_quota_stop_on_an_attach_leg_overflows_to_codex() {
        let current = Leg {
            harness: Harness::new(Harness::CURSOR),
            last_turn: Some(TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            }),
            used_percent: None,
            minutes_to_reset: None,
            reached: false,
        };
        match decide(&current, &codex()) {
            Decision::Hop { to, hop, .. } => {
                assert_eq!(to.as_str(), Harness::CODEX);
                assert_eq!(hop, Hop::Displaced);
            }
            other => panic!("expected overflow to Codex, got {other:?}"),
        }
    }
}
