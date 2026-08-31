//! Whether a session the user opened is continuing a conversation.
//!
//! The question "which conversation does this session join" has no safe answer
//! when three harnesses are open on one checkout, and asking it that way was
//! the mistake. `bus_id` answers *whose turns are these* and keeps answering
//! only that. What a continuation needs is a second, weaker relation — *what
//! came before this* — recorded as an edge under the successor. An edge can be
//! wrong and contradicted later; a merged identity cannot, because in an
//! append-only log the identity *is* the write.
//!
//! Read honestly, what follows is a refusal engine with a single positive
//! signal: a predecessor that ended on quota or policy *announced* that it
//! could not continue. Everything else is a short recency window. That is what
//! makes it safe to run without asking anyone anything — the failure the user
//! sees is "it did not help", never "it lied".
//!
//! Pure on purpose. The whole log can be replayed through it offline, so "over
//! N real conversations it fires this often and picks this one" is a
//! measurement available before a single word is injected anywhere.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::clock::Timestamp;
use crate::event::{Event, Harness, TurnState};
use crate::ids::BusId;
use crate::record::Record;

/// The session asking whether it is continuing something.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Asking {
    pub harness: Harness,
    /// From the payload's `cwd`, which both vendors put on every event.
    pub workspace: Option<String>,
    /// True only on the prompt that minted this conversation. A later turn is
    /// never hydrated: the plan's per-prompt overhead is paid once at injection
    /// and then carried in history for every turn after it.
    pub first_prompt: bool,
}

/// A conversation that might be the one being continued.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Candidate {
    pub bus_id: BusId,
    pub harness: Harness,
    pub workspace: Option<String>,
    /// When anything was last recorded under it.
    pub last_activity: Timestamp,
    /// How the last turn ended, when the last thing that happened *was* a turn
    /// ending. `None` covers a conversation stopped mid-turn or mid-tool-loop,
    /// which is not a thing to continue from — it is a thing still happening.
    pub last_turn: Option<TurnState>,
    pub turns: u64,
    /// Reconstructed from the vendor's own history rather than mirrored live.
    pub backfilled: bool,
}

/// Why one conversation is believed to precede another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Evidence {
    /// The predecessor said it could not continue. The one strong signal.
    Displaced,
    /// The predecessor finished and the user moved harness. A recency guess.
    HandedOff,
}

/// Why no edge was drawn. Every reason is named, because without them "it never
/// fires" and "the gate is too tight" are the same observation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Declined {
    /// The payload carried no `cwd`. Nothing can be looked up without it.
    NoWorkspace,
    /// A workspace so broad that everything would match it — `/` or the home
    /// directory, which is where a session started from a login shell reports.
    WorkspaceTooBroad,
    /// Not the prompt that started this conversation.
    NotFirstPrompt,
    /// Nothing in this workspace survived the filters.
    NoCandidate,
    /// More than one did. Never broken by recency: a confidently wrong edge is
    /// the cross-vendor injection path the threat model calls T4.
    Ambiguous { candidates: usize },
    /// Someone else is working in this checkout right now.
    WorkspaceBusy,
}

/// What the gate decided.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Decision {
    Continue { bus_id: BusId, evidence: Evidence },
    Declined(Declined),
}

/// How long a predecessor that announced it could not continue stays eligible.
///
/// Generous because the signal is strong and the situation is the product: a
/// five-hour vendor limit is the case the whole design was drawn for, and a
/// person who hits one at lunch comes back after it.
pub const DISPLACED_WINDOW_SECS: i64 = 8 * 60 * 60;

/// How long a predecessor that simply finished stays eligible.
///
/// Short because the signal is weak. "I finished in Claude and opened Codex" is
/// only a continuation while the work is still in the person's head.
pub const HANDED_OFF_WINDOW_SECS: i64 = 20 * 60;

/// How recently another conversation must have moved for this checkout to count
/// as somebody else's right now.
pub const BUSY_WINDOW_SECS: i64 = 90;

/// The fewest turns a conversation must have before it is worth continuing.
///
/// Two, and it removes nine tenths of the candidates on its own: 3,179 of the
/// 3,434 conversations in the real log are a single turn with nothing to carry.
pub const MIN_TURNS: u64 = 2;

impl Candidate {
    /// Build one candidate per conversation from the log.
    ///
    /// Derived state, so it has to be rebuildable from the segments — the same
    /// rule the registry's bindings follow, and for the same reason: a daemon
    /// that restarts with an empty index would refuse every continuation until
    /// the log happened to be read again.
    ///
    /// Records must arrive in log order; `SegmentLog::scan` returns them that
    /// way.
    pub fn index(records: &[Record]) -> Vec<Candidate> {
        let mut order: Vec<BusId> = Vec::new();
        let mut by_id: HashMap<BusId, Candidate> = HashMap::new();

        for record in records {
            if !by_id.contains_key(&record.bus_id) {
                order.push(record.bus_id);
            }
            Candidate::apply(&mut by_id, record);
        }

        order
            .into_iter()
            .filter_map(|bus_id| by_id.remove(&bus_id))
            .collect()
    }

    /// Fold one record into a running index.
    ///
    /// Split out so the offline replay does not rebuild the whole index per
    /// conversation, which on the real log is 3,434 rebuilds over 24,000
    /// records — and so there is exactly one definition of what a record does
    /// to a conversation's state.
    pub fn apply(by_id: &mut HashMap<BusId, Candidate>, record: &Record) {
        let candidate = by_id.entry(record.bus_id).or_insert_with(|| Candidate {
            bus_id: record.bus_id,
            harness: record.source.harness.clone(),
            workspace: None,
            last_activity: record.ts,
            last_turn: None,
            turns: 0,
            backfilled: false,
        });

        candidate.harness = record.source.harness.clone();
        candidate.last_activity = record.ts;
        if record.source.workspace.is_some() {
            candidate.workspace.clone_from(&record.source.workspace);
        }
        candidate.backfilled |= record.source.backfilled_from.is_some();

        match &record.event {
            Event::TurnStarted { .. } => {
                candidate.turns += 1;
                candidate.last_turn = None;
            }
            Event::TurnEnded { state, .. } => candidate.last_turn = Some(state.clone()),
            // A session opening or closing does not disturb how the last turn
            // ended: a conversation that finished and then had its window
            // closed is still a finished conversation.
            Event::SessionCandidate { .. } | Event::SessionEnded { .. } => {}
            // Tool traffic means a turn is in flight, whatever came before.
            Event::ToolRequested { .. }
            | Event::ToolDenied { .. }
            | Event::ToolUse { .. }
            | Event::ToolBatchEnded { .. } => candidate.last_turn = None,
        }
    }
}

/// Decide whether this session continues one of these conversations.
pub fn decide(
    asking: &Asking,
    candidates: &[Candidate],
    home: Option<&str>,
    now: Timestamp,
) -> Decision {
    if !asking.first_prompt {
        return Decision::Declined(Declined::NotFirstPrompt);
    }
    let Some(workspace) = asking.workspace.as_deref() else {
        return Decision::Declined(Declined::NoWorkspace);
    };
    if workspace == "/" || Some(workspace) == home {
        return Decision::Declined(Declined::WorkspaceTooBroad);
    }

    let here: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.workspace.as_deref() == Some(workspace))
        .collect();

    let eligible: Vec<(&Candidate, Evidence)> = here
        .iter()
        .filter_map(|candidate| eligibility(asking, candidate, now).map(|why| (*candidate, why)))
        .collect();

    let chosen = match eligible.as_slice() {
        [] => return Decision::Declined(Declined::NoCandidate),
        [(candidate, evidence)] => (*candidate, *evidence),
        many => {
            return Decision::Declined(Declined::Ambiguous {
                candidates: many.len(),
            });
        }
    };

    // Everything else in this checkout, including conversations that were never
    // candidates. A one-turn session or a same-harness one cannot be chosen
    // wrongly, but it can still mean a person is sitting in this repo working on
    // something else, and claiming a continuation over their shoulder is the
    // failure that costs credibility rather than tokens.
    let busy = here.iter().any(|other| {
        other.bus_id != chosen.0.bus_id && now.seconds_since(other.last_activity) < BUSY_WINDOW_SECS
    });
    if busy {
        return Decision::Declined(Declined::WorkspaceBusy);
    }

    Decision::Continue {
        bus_id: chosen.0.bus_id,
        evidence: chosen.1,
    }
}

/// What the gate would have done, replayed over a log.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Replay {
    /// One count per verdict, named the way the log would name it.
    pub verdicts: BTreeMap<&'static str, u64>,
}

impl Replay {
    pub fn total(&self) -> u64 {
        self.verdicts.values().sum()
    }
}

/// Ask the gate what it would have done at every conversation's first prompt.
///
/// The point of a pure gate: this runs over the whole log offline, so "it fires
/// this often and refuses for these reasons" is a measurement rather than a
/// hope, available before a single word is injected anywhere. Faithful to the
/// moment — each conversation is judged against only what had already happened
/// when its first prompt arrived.
///
/// Records are read in timestamp order, not append order. Backfill appends
/// months-old conversations to the end of the log, so the two are not the same.
pub fn replay(records: &[Record], home: Option<&str>) -> Replay {
    replay_inner(records, home, false)
}

/// The same replay with the two rules a single-vendor corpus cannot exercise
/// lifted. A measurement only.
///
/// Every conversation in a log built from Claude Code transcripts is Claude
/// Code, and every one of them is reconstructed. Those two rules therefore
/// refuse all of it before any other rule is consulted, so the shipped replay
/// proves there are no false positives and says nothing about whether the rest
/// of the gate discriminates.
///
/// Lifting them asks the question the corpus can answer: given these
/// workspaces, these turn counts, these endings and these times, how often
/// would the gate have fired, and how often would it have had to choose between
/// conversations? That matters here more than anywhere, because one workspace
/// in this corpus holds 3,031 conversations — if the ambiguity refusal did not
/// work, this is where it would show.
///
/// It is a counterfactual and must be labelled as one wherever it is shown. It
/// is not the gate.
pub fn replay_as_if_cross_vendor(records: &[Record], home: Option<&str>) -> Replay {
    replay_inner(records, home, true)
}

fn replay_inner(records: &[Record], home: Option<&str>, counterfactual: bool) -> Replay {
    let mut by_time: Vec<&Record> = records.iter().collect();
    by_time.sort_by_key(|record| (record.ts, record.seq));

    // Which checkout each conversation happened in, read from the whole log
    // rather than incrementally.
    //
    // This is the one place the replay looks ahead, and it is looking ahead at
    // a fact the live path never had to guess: `cwd` is on every hook payload,
    // so a running daemon knows the workspace of the session in front of it and
    // of every session it has already seen. What it does *not* reconstruct is
    // the conversation's state — turn count, how the last turn ended, when it
    // last moved — which stays strictly as-of, because that is what the gate
    // actually judges. Records written before the workspace was recorded carry
    // it only on a session record appended later, at the conversation's own
    // opening time but with a much larger sequence number, so an incremental
    // read puts it after the prompt it belongs to and answers `no_workspace`
    // for all 3,433 of them.
    let mut workspace_of: HashMap<BusId, String> = HashMap::new();
    for record in records {
        if let Some(workspace) = &record.source.workspace {
            workspace_of.insert(record.bus_id, workspace.clone());
        }
    }

    let mut out = Replay::default();
    let mut live: HashMap<BusId, Candidate> = HashMap::new();
    let mut asked: HashSet<BusId> = HashSet::new();

    for record in by_time {
        if matches!(record.event, Event::TurnStarted { .. }) && asked.insert(record.bus_id) {
            // The workspace of the *conversation*, not of this one record.
            let workspace = workspace_of.get(&record.bus_id).cloned();
            // Only this checkout. The gate would reject the rest anyway, and
            // handing it every conversation on the machine is what turns a
            // replay into a quadratic one.
            let candidates: Vec<Candidate> = match workspace.as_deref() {
                Some(workspace) => live
                    .values()
                    .filter(|candidate| {
                        candidate.bus_id != record.bus_id
                            && workspace_of.get(&candidate.bus_id).map(String::as_str)
                                == Some(workspace)
                    })
                    .map(|candidate| Candidate {
                        workspace: Some(workspace.to_string()),
                        backfilled: candidate.backfilled && !counterfactual,
                        harness: if counterfactual {
                            Harness::new("other")
                        } else {
                            candidate.harness.clone()
                        },
                        ..candidate.clone()
                    })
                    .collect(),
                None => Vec::new(),
            };
            let asking = Asking {
                harness: record.source.harness.clone(),
                workspace,
                first_prompt: true,
            };
            let name = match decide(&asking, &candidates, home, record.ts) {
                Decision::Continue {
                    evidence: Evidence::Displaced,
                    ..
                } => "continue/displaced",
                Decision::Continue {
                    evidence: Evidence::HandedOff,
                    ..
                } => "continue/handed_off",
                Decision::Declined(reason) => match reason {
                    Declined::NoWorkspace => "declined/no_workspace",
                    Declined::WorkspaceTooBroad => "declined/workspace_too_broad",
                    Declined::NotFirstPrompt => "declined/not_first_prompt",
                    Declined::NoCandidate => "declined/no_candidate",
                    Declined::Ambiguous { .. } => "declined/ambiguous",
                    Declined::WorkspaceBusy => "declined/workspace_busy",
                },
            };
            *out.verdicts.entry(name).or_default() += 1;
        }
        Candidate::apply(&mut live, record);
    }
    out
}

/// Why this one conversation could be continued, if it could.
fn eligibility(asking: &Asking, candidate: &Candidate, now: Timestamp) -> Option<Evidence> {
    // Same harness is already covered by the vendor's own session id plus its
    // resume, compact and fork sources. And `/clear` is a deliberate "forget
    // this", so declining to cross it is the correct answer, not a gap.
    if candidate.harness == asking.harness {
        return None;
    }
    // An edge is a live-path fact. A backfilled conversation is whatever a
    // one-shot parser could recover from a format the vendor calls internal,
    // and hydrating a live session from one would inject a reconstruction.
    if candidate.backfilled {
        return None;
    }
    if candidate.turns < MIN_TURNS {
        return None;
    }
    let age = now.seconds_since(candidate.last_activity);
    if age < 0 {
        return None;
    }
    match candidate.last_turn.as_ref()? {
        TurnState::InterruptedQuota { .. } | TurnState::InterruptedPolicy { .. } => {
            (age <= DISPLACED_WINDOW_SECS).then_some(Evidence::Displaced)
        }
        TurnState::Complete => (age <= HANDED_OFF_WINDOW_SECS).then_some(Evidence::HandedOff),
        // The user stopped it themselves. That is a decision to abandon the
        // turn, and reading it as an invitation to carry the work elsewhere
        // would be putting words in their mouth.
        TurnState::InterruptedUser => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "/Applications/conbus";
    const NOW: &str = "2026-08-31T12:00:00.000Z";

    fn now() -> Timestamp {
        NOW.parse().expect("ts")
    }

    /// `seconds` before `NOW`. Rendered through the log's own format rather
    /// than RFC 3339, which drops `.000` and then will not parse back.
    fn ago(seconds: i64) -> Timestamp {
        const SHAPE: &[time::format_description::BorrowedFormatItem<'_>] = time::macros::format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        );
        let base = time::PrimitiveDateTime::parse(NOW, SHAPE).expect("parse");
        (base - time::Duration::seconds(seconds))
            .format(SHAPE)
            .expect("format")
            .parse()
            .expect("ts")
    }

    fn asking() -> Asking {
        Asking {
            harness: Harness::new(Harness::CODEX),
            workspace: Some(WORKSPACE.into()),
            first_prompt: true,
        }
    }

    fn candidate() -> Candidate {
        Candidate {
            bus_id: BusId::mint(),
            harness: Harness::new(Harness::CLAUDE_CODE),
            workspace: Some(WORKSPACE.into()),
            last_activity: ago(120),
            last_turn: Some(TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            }),
            turns: 6,
            backfilled: false,
        }
    }

    fn decide_one(candidate: Candidate) -> Decision {
        decide(&asking(), &[candidate], Some("/Users/someone"), now())
    }

    #[test]
    fn a_predecessor_that_ran_out_of_quota_is_the_one_strong_signal() {
        let subject = candidate();
        let bus_id = subject.bus_id;

        assert_eq!(
            decide_one(subject),
            Decision::Continue {
                bus_id,
                evidence: Evidence::Displaced
            }
        );
    }

    #[test]
    fn a_policy_refusal_counts_the_same_because_it_says_the_same_thing() {
        let subject = Candidate {
            last_turn: Some(TurnState::InterruptedPolicy {
                reason: "billing_error".into(),
            }),
            ..candidate()
        };
        let bus_id = subject.bus_id;

        assert_eq!(
            decide_one(subject),
            Decision::Continue {
                bus_id,
                evidence: Evidence::Displaced
            }
        );
    }

    #[test]
    fn a_finished_conversation_is_a_weaker_signal_on_a_much_shorter_window() {
        let finished = Candidate {
            last_turn: Some(TurnState::Complete),
            ..candidate()
        };
        let bus_id = finished.bus_id;

        assert_eq!(
            decide_one(finished.clone()),
            Decision::Continue {
                bus_id,
                evidence: Evidence::HandedOff
            }
        );
        assert_eq!(
            decide_one(Candidate {
                last_activity: ago(HANDED_OFF_WINDOW_SECS + 1),
                ..finished
            }),
            Decision::Declined(Declined::NoCandidate),
            "a finished conversation goes cold long before a displaced one"
        );
    }

    #[test]
    fn a_displaced_conversation_stays_eligible_across_a_vendor_limit() {
        // The five-hour limit is the case the design was drawn for. A window
        // shorter than one would refuse exactly the hop the product exists to
        // make.
        const { assert!(DISPLACED_WINDOW_SECS > 5 * 60 * 60) };
        assert!(matches!(
            decide_one(Candidate {
                last_activity: ago(5 * 60 * 60),
                ..candidate()
            }),
            Decision::Continue { .. }
        ));
        assert_eq!(
            decide_one(Candidate {
                last_activity: ago(DISPLACED_WINDOW_SECS + 1),
                ..candidate()
            }),
            Decision::Declined(Declined::NoCandidate)
        );
    }

    #[test]
    fn a_turn_the_user_stopped_is_not_an_invitation() {
        assert_eq!(
            decide_one(Candidate {
                last_turn: Some(TurnState::InterruptedUser),
                ..candidate()
            }),
            Decision::Declined(Declined::NoCandidate),
            "the user stopping a turn is a decision to abandon it"
        );
    }

    #[test]
    fn a_conversation_still_mid_turn_is_not_a_thing_to_continue_from() {
        assert_eq!(
            decide_one(Candidate {
                last_turn: None,
                ..candidate()
            }),
            Decision::Declined(Declined::NoCandidate),
            "it is still happening"
        );
    }

    #[test]
    fn one_turn_conversations_are_removed_before_anything_else_is_asked() {
        // On the real log this single condition removes 3,179 of 3,434.
        assert_eq!(
            decide_one(Candidate {
                turns: 1,
                ..candidate()
            }),
            Decision::Declined(Declined::NoCandidate)
        );
    }

    #[test]
    fn the_same_harness_is_the_vendors_own_resume_and_not_ours() {
        assert_eq!(
            decide_one(Candidate {
                harness: Harness::new(Harness::CODEX),
                ..candidate()
            }),
            Decision::Declined(Declined::NoCandidate)
        );
    }

    #[test]
    fn a_reconstructed_conversation_is_not_evidence_of_a_hop() {
        assert_eq!(
            decide_one(Candidate {
                backfilled: true,
                ..candidate()
            }),
            Decision::Declined(Declined::NoCandidate),
            "an edge is a live-path fact"
        );
    }

    #[test]
    fn another_checkout_is_another_piece_of_work() {
        // Worktrees of one repository are deliberately distinct: this project
        // is being written in one, and the branch in it is not the branch in
        // the main checkout.
        assert_eq!(
            decide_one(Candidate {
                workspace: Some(format!("{WORKSPACE}/.claude/worktrees/other")),
                ..candidate()
            }),
            Decision::Declined(Declined::NoCandidate)
        );
    }

    #[test]
    fn two_candidates_are_never_broken_by_recency() {
        // The case that killed the previous design: one checkout, several
        // harnesses, genuinely different content. A confidently wrong edge is
        // worse than none, so the tie is simply refused.
        let older = candidate();
        let newer = Candidate {
            bus_id: BusId::mint(),
            harness: Harness::new(Harness::CLAUDE_CODE),
            last_activity: ago(100),
            ..candidate()
        };

        assert_eq!(
            decide(&asking(), &[older, newer], None, now()),
            Decision::Declined(Declined::Ambiguous { candidates: 2 })
        );
    }

    #[test]
    fn a_checkout_somebody_is_working_in_right_now_is_left_alone() {
        let chosen = candidate();
        // Not a candidate itself — one turn, and the same harness as the asking
        // session — but a sign that a person is in this repo doing something
        // else, which is the failure that costs credibility rather than tokens.
        let busy = Candidate {
            bus_id: BusId::mint(),
            harness: Harness::new(Harness::CODEX),
            last_activity: ago(BUSY_WINDOW_SECS - 1),
            turns: 1,
            last_turn: None,
            ..candidate()
        };

        assert_eq!(
            decide(&asking(), &[chosen.clone(), busy.clone()], None, now()),
            Decision::Declined(Declined::WorkspaceBusy)
        );
        assert!(
            matches!(
                decide(
                    &asking(),
                    &[
                        chosen,
                        Candidate {
                            last_activity: ago(BUSY_WINDOW_SECS + 1),
                            ..busy
                        }
                    ],
                    None,
                    now()
                ),
                Decision::Continue { .. }
            ),
            "once it goes quiet the hop is allowed again"
        );
    }

    #[test]
    fn the_conversation_being_continued_does_not_make_the_checkout_busy() {
        // The hop the product is for takes seconds: the limit lands, the user
        // opens the other harness. Counting the predecessor's own last breath
        // as somebody working here would refuse precisely that.
        let just_stopped = Candidate {
            last_activity: ago(5),
            ..candidate()
        };

        assert!(matches!(
            decide_one(just_stopped),
            Decision::Continue { .. }
        ));
    }

    #[test]
    fn a_later_turn_is_never_hydrated() {
        assert_eq!(
            decide(
                &Asking {
                    first_prompt: false,
                    ..asking()
                },
                &[candidate()],
                None,
                now()
            ),
            Decision::Declined(Declined::NotFirstPrompt)
        );
    }

    #[test]
    fn a_workspace_broad_enough_to_match_everything_is_refused() {
        for broad in ["/", "/Users/someone"] {
            assert_eq!(
                decide(
                    &Asking {
                        workspace: Some(broad.into()),
                        ..asking()
                    },
                    &[candidate()],
                    Some("/Users/someone"),
                    now()
                ),
                Decision::Declined(Declined::WorkspaceTooBroad),
                "{broad} would collect every conversation on the machine"
            );
        }
    }

    #[test]
    fn without_a_workspace_nothing_can_be_looked_up() {
        assert_eq!(
            decide(
                &Asking {
                    workspace: None,
                    ..asking()
                },
                &[candidate()],
                None,
                now()
            ),
            Decision::Declined(Declined::NoWorkspace)
        );
    }

    fn logged(bus_id: BusId, at: Timestamp, event: Event) -> Record {
        Record {
            schema_version: crate::record::SCHEMA_VERSION,
            ts: at,
            seq: 0,
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: crate::event::Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some("s".into()),
                workspace: Some(WORKSPACE.into()),
                backfilled_from: None,
            },
            event,
        }
    }

    fn prompt() -> Event {
        Event::TurnStarted {
            author: None,
            prompt: crate::event::Body::kept("p"),
        }
    }

    #[test]
    fn the_index_reads_a_conversation_out_of_its_own_records() {
        let bus_id = BusId::mint();
        let records = vec![
            logged(bus_id, ago(300), Event::SessionCandidate { trigger: None }),
            logged(bus_id, ago(280), prompt()),
            logged(
                bus_id,
                ago(260),
                Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                },
            ),
            logged(bus_id, ago(200), prompt()),
            logged(
                bus_id,
                ago(120),
                Event::TurnEnded {
                    state: TurnState::InterruptedQuota {
                        reason: "rate_limit".into(),
                    },
                    assistant_message: None,
                },
            ),
            logged(
                bus_id,
                ago(100),
                Event::SessionEnded {
                    reason: Some("other".into()),
                },
            ),
        ];

        let index = Candidate::index(&records);

        assert_eq!(index.len(), 1);
        let only = &index[0];
        assert_eq!(only.turns, 2);
        assert_eq!(only.workspace.as_deref(), Some(WORKSPACE));
        assert_eq!(only.last_activity, ago(100), "activity, not the last turn");
        assert_eq!(
            only.last_turn,
            Some(TurnState::InterruptedQuota {
                reason: "rate_limit".into()
            }),
            "closing the window does not undo how the conversation ended"
        );
        assert!(!only.backfilled);
    }

    #[test]
    fn a_conversation_in_flight_is_indexed_as_in_flight() {
        // The distinction the gate turns on: a tool call after the last turn
        // ending means work is still happening, and a conversation still
        // happening is not one to continue from.
        let bus_id = BusId::mint();
        let records = vec![
            logged(bus_id, ago(300), prompt()),
            logged(
                bus_id,
                ago(280),
                Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                },
            ),
            logged(bus_id, ago(200), prompt()),
            logged(
                bus_id,
                ago(100),
                Event::ToolRequested {
                    tool: "Read".into(),
                    tool_use_id: None,
                    input: crate::event::Body::kept("i"),
                },
            ),
        ];

        assert_eq!(Candidate::index(&records)[0].last_turn, None);
    }

    #[test]
    fn the_replay_judges_each_conversation_against_only_what_came_before_it() {
        // The measurement the gate exists to make possible. Two conversations
        // in one checkout: the first runs out of quota, the second starts in
        // the other harness two minutes later. Replayed, that is one hop.
        let first = BusId::mint();
        let second = BusId::mint();
        let codex = |record: Record| Record {
            source: crate::event::Source {
                harness: Harness::new(Harness::CODEX),
                ..record.source
            },
            ..record
        };
        let records = vec![
            logged(first, ago(600), prompt()),
            logged(
                first,
                ago(500),
                Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                },
            ),
            logged(first, ago(400), prompt()),
            logged(
                first,
                ago(300),
                Event::TurnEnded {
                    state: TurnState::InterruptedQuota {
                        reason: "rate_limit".into(),
                    },
                    assistant_message: None,
                },
            ),
            codex(logged(second, ago(180), prompt())),
        ];

        let replay = replay(&records, None);

        assert_eq!(replay.total(), 2, "one verdict per conversation");
        assert_eq!(replay.verdicts.get("continue/displaced"), Some(&1));
        assert_eq!(
            replay.verdicts.get("declined/no_candidate"),
            Some(&1),
            "the first conversation had nothing before it"
        );
    }

    #[test]
    fn the_replay_reads_the_log_by_time_and_not_by_append_order() {
        // Backfill appends months-old conversations to the end of the log, so
        // append order and chronology disagree. Judged in append order, the
        // older conversation would be asked to continue the newer one.
        let old = BusId::mint();
        let new = BusId::mint();
        let mut records = vec![
            logged(new, ago(100), prompt()),
            logged(old, ago(10_000), prompt()),
        ];
        for (seq, record) in records.iter_mut().enumerate() {
            record.seq = seq as u64;
        }

        let replay = replay(&records, None);

        assert_eq!(replay.verdicts.get("declined/no_candidate"), Some(&2));
    }

    #[test]
    fn the_counterfactual_actually_lifts_the_rules_it_claims_to() {
        // The first version of it lifted only the reconstruction rule, which on
        // a single-vendor corpus changes nothing — it printed numbers identical
        // to the real gate and looked like a measurement. A counterfactual that
        // silently equals the thing it is contrasted with is worse than none.
        let first = BusId::mint();
        let second = BusId::mint();
        let backfilled = |record: Record| Record {
            source: crate::event::Source {
                backfilled_from: Some("claude_transcript".into()),
                ..record.source
            },
            ..record
        };
        let records = vec![
            backfilled(logged(first, ago(600), prompt())),
            backfilled(logged(
                first,
                ago(500),
                Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                },
            )),
            backfilled(logged(first, ago(400), prompt())),
            backfilled(logged(
                first,
                ago(300),
                Event::TurnEnded {
                    state: TurnState::Complete,
                    assistant_message: None,
                },
            )),
            // Same harness as the one before it, and reconstructed: refused
            // twice over by the real gate.
            backfilled(logged(second, ago(120), prompt())),
        ];

        assert_eq!(
            replay(&records, None).verdicts.get("continue/handed_off"),
            None,
            "the shipped gate refuses a same-harness, reconstructed predecessor"
        );
        assert_eq!(
            replay_as_if_cross_vendor(&records, None)
                .verdicts
                .get("continue/handed_off"),
            Some(&1),
            "and the counterfactual has to actually lift both rules, or it is \
             measuring the same thing twice"
        );
    }

    #[test]
    fn a_conversation_from_the_future_is_refused_rather_than_treated_as_fresh() {
        // Backfilled records carry the time they happened, and a wall clock can
        // step backwards. A negative age read as "very recent" would make the
        // stalest possible candidate the most attractive one.
        assert_eq!(
            decide_one(Candidate {
                last_activity: ago(-60),
                ..candidate()
            }),
            Decision::Declined(Declined::NoCandidate)
        );
    }
}
