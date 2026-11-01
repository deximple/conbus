//! What the bus has to say to a session opening in a checkout.
//!
//! An earlier version of this asked which conversation a new session was
//! *joining*, and answered by choosing one. That was the wrong question twice
//! over.
//!
//! It was wrong because "you are continuing conversation X" is a claim about
//! identity, and a claim can be a lie. Guarding against the lie forced the
//! precision up until the thing became a refusal engine: replayed over 3,443
//! real conversations it fired **twelve times**. A feature that reaches 0.35%
//! of sessions is not invisible; it is absent.
//!
//! And it was wrong because a session boundary is not a thing a person chose.
//! Claude Code fires several session starts per project, `/clear` makes a new
//! one, a crash makes a new one. What a person does not want to re-explain is
//! the *work*, and the work lives in the checkout.
//!
//! So this decides what to *say*, not which conversation to *be*. A briefing
//! narrates what has been happening in this checkout and never claims the
//! session reading it is one of them. That difference is the whole design: a
//! wrong identity claim is a lie that costs credibility in every session
//! afterwards, while a briefing that is not useful is noise, marked with its
//! own age, that the reader discounts. Precision stops being the binding
//! constraint, and the same rules that briefed 91 of 112 workspaces would have
//! continued twelve conversations.
//!
//! The strong claim — one conversation whose harness changed underneath it —
//! survives only where the bus opened the leg itself, because there identity is
//! known rather than inferred. That path does not come through here.
//!
//! Pure on purpose. The whole log can be replayed through it offline, so how
//! often it speaks and what it refuses is a measurement available before a word
//! is injected anywhere.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::clock::Timestamp;
use crate::event::{Entrypoint, Event, Harness, TurnState};
use crate::ids::BusId;
use crate::record::Record;

/// The session asking what it should be told.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Asking {
    pub harness: Harness,
    /// From the payload's `cwd`, which both vendors put on every event.
    pub workspace: Option<String>,
    /// True only on the prompt that minted this conversation. A later turn is
    /// never briefed: the per-prompt overhead is paid once at injection and
    /// then carried in history for every turn after it.
    pub first_prompt: bool,
    /// The harness said this session is a program, not a person — an SDK
    /// driver, an MCP server, a CI runner.
    ///
    /// Named for the direction that is knowable. The vendor labels machine
    /// origins and almost never labels human ones, so this is set only when it
    /// said so, and an unlabelled session is briefed. On the real log (by
    /// entrypoint, 2026-09-02) this one condition is the difference between
    /// speaking to 3,443 sessions and speaking to 349: the rest are explicitly
    /// somebody's script, and briefing a script is tokens spent on a reader
    /// that does not exist.
    pub unattended: bool,
    /// The opening prompt begins with a vendor's tell that a program wrote it
    /// — see [`machine_authored`]. Separate from `unattended` because the two
    /// come from different places and disagree on a real log: four scheduled
    /// runs were `claude-desktop`, attended by the entrypoint, and Codex's
    /// memory agent has no entrypoint at all.
    pub machine_authored: bool,
    /// How long this session's opening prompt is, in UTF-16 code units.
    ///
    /// A proxy for something there is no direct signal for: whether the session
    /// arrived carrying its own context. A person asking for something types
    /// tens or low hundreds of units; a scheduled task, a summariser or an
    /// agent harness opens by pasting its whole brief.
    pub prompt_units: usize,
}

/// A conversation that has happened in some checkout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Candidate {
    pub bus_id: BusId,
    pub harness: Harness,
    pub workspace: Option<String>,
    /// When anything was last recorded under it.
    pub last_activity: Timestamp,
    /// How the last turn ended, when the last thing that happened *was* a turn
    /// ending. `None` means a turn was in flight.
    pub last_turn: Option<TurnState>,
    /// When the newest turn end folded so far happened. What [`overtaken`] holds
    /// a late record against, so it outlives the next turn's start.
    pub turn_ended_at: Option<Timestamp>,
    pub turns: u64,
    /// Reconstructed from the vendor's own history rather than mirrored live.
    pub backfilled: bool,
    /// Mirrored live, at least in part. Both can be true: a session resumed
    /// after its history was recovered carries the recovery and the
    /// continuation under one id, and the briefing has to say "partly".
    pub live: bool,
}

/// How a conversation came to a stop.
///
/// Description, not permission. Under the old design this was the gate's
/// verdict and it decided whether anything happened at all; here it is
/// something the briefing says, and every value of it is sayable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stopped {
    /// The current leg hit quota. The one a person most needs told, and the
    /// only stop the router hops on.
    Displaced,
    /// The vendor refused on policy, billing, or auth. Not a quota hop.
    Refused,
    /// It finished.
    Finished,
    /// The user stopped it.
    Abandoned,
    /// A turn was in flight when the log last heard from it.
    InFlight,
}

/// Why nothing is said.
///
/// Named individually so that "it never speaks" and "the window is too tight"
/// stay different observations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Silent {
    /// The payload carried no `cwd`. Nothing can be looked up without it.
    NoWorkspace,
    /// A checkout so broad that everything on the machine is in it — `/` or the
    /// home directory, which is where a session started from a login shell
    /// reports.
    WorkspaceTooBroad,
    /// Not the prompt that started this conversation.
    NotFirstPrompt,
    /// This checkout has nothing recent enough, or long enough, to be worth
    /// the tokens.
    NothingToSay,
    /// Nobody is reading. A briefing is for orientation, and a program does not
    /// need orienting.
    Unattended,
    /// The prompt itself says a program wrote it — a scheduled task's
    /// envelope, Codex's memory agent's instructions. The entrypoint calls
    /// these attended, because a person's front end ran them; the prompt's
    /// own first line says otherwise, in the vendor's words.
    MachineAuthored,
    /// The session opened by supplying its own context, at length. Whatever is
    /// driving it already knows what it is doing.
    ArrivedWithContext,
}

/// What to tell a session opening here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Decision {
    Silent(Silent),
    /// Conversations to narrate, most recent first. Facts about the checkout.
    Brief {
        recent: Vec<BusId>,
    },
}

/// How long ago a conversation can have stopped and still be worth mentioning.
///
/// A week, chosen off the measurement rather than from taste. On the real log,
/// counting only the sessions a person was actually at, a week briefed 13 of 78
/// on the first measurement (2026-08-31) and 40 of 349 by entrypoint (2026-09-02),
/// and a month briefs 15 — four times the window for two more briefings. The
/// knee is inside the first week, and a week is also the span a person can
/// reason about: "what I was doing here recently" rather than archaeology.
pub const BRIEF_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

/// The most conversations one briefing names.
///
/// Three. Past that it stops being orientation and becomes a directory
/// listing, and the reader learns to skip it — an ignored briefing is worse
/// than none, because being noticed was the whole argument for it.
pub const BRIEF_MAX: usize = 3;

/// An opening prompt longer than this means the session brought its own brief.
///
/// The last gate, and the one that stops this being chrome. Measured on the
/// sessions a briefing would actually reach: the median opening prompt among
/// them is 2,273 units and the p75 is 3,142 — those are not questions, they are
/// scheduled tasks, summarisers and agent harnesses pasting their whole
/// instructions. Real typed asks in the same corpus run tens to low hundreds,
/// and 38% of all prompts are 200 units or under.
///
/// A person who does paste something long has, by doing so, just supplied the
/// context a briefing would have added — so the rule reads the same either way:
/// a session that arrives carrying its own context does not need ours. Cutting
/// here drops 70% of the briefings the earlier gates let through.
pub const SELF_SUPPLIED_UNITS: usize = 1_200;

/// The fewest turns a conversation must have to be worth mentioning.
///
/// Two, and it removes nine tenths of everything on its own: 3,179 of the 3,434
/// conversations in the real log are a single turn with nothing to carry.
pub const MIN_TURNS: u64 = 2;

/// Whether a record that waited on its way to the log happened before the
/// conversation's last turn end, and so says nothing about now.
///
/// A record's place in the log is when it landed, and it can land behind a turn
/// end that came after it: through the spool, parked for the prompt that binds its
/// session, or overtaken by a live hook. The tool ran, or the prompt came, and that
/// turn is over. A record that did not wait has no `spooled_at`, and is news.
fn overtaken(record: &Record, turn_ended_at: Option<Timestamp>) -> bool {
    record
        .spooled_at
        .zip(turn_ended_at)
        .is_some_and(|(waited_from, ended)| waited_from < ended)
}

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
            turn_ended_at: None,
            turns: 0,
            backfilled: false,
            live: false,
        });

        let says_nothing_now = overtaken(record, candidate.turn_ended_at);
        candidate.harness = record.source.harness.clone();
        candidate.last_activity = record.ts;
        if record.source.workspace.is_some() {
            candidate.workspace.clone_from(&record.source.workspace);
        }
        candidate.backfilled |= record.source.backfilled_from.is_some();
        candidate.live |= record.source.backfilled_from.is_none();

        match &record.event {
            Event::TurnStarted { .. } => {
                candidate.turns += 1;
                if !says_nothing_now {
                    candidate.last_turn = None;
                }
            }
            Event::TurnEnded { state, .. } => {
                candidate.last_turn = Some(state.clone());
                let happened = record.spooled_at.unwrap_or(record.ts);
                candidate.turn_ended_at = candidate.turn_ended_at.max(Some(happened));
            }
            // A session opening or closing does not disturb how the last turn
            // ended: a conversation that finished and then had its window
            // closed is still a finished conversation. Neither does a briefing,
            // which is something the bus did rather than something the
            // conversation did — and reading our own output back as activity is
            // how a feature starts feeding on itself.
            Event::SessionCandidate { .. }
            | Event::SessionEnded { .. }
            | Event::Notified { .. }
            | Event::Briefed { .. }
            | Event::Routed { .. }
            | Event::Continued { .. }
            | Event::Summarised { .. }
            | Event::Leased { .. }
            | Event::Aliased { .. } => {}
            // Tool traffic means a turn is in flight, whatever came before, as
            // long as it is about now.
            Event::ToolRequested { .. }
            | Event::PermissionAsked { .. }
            | Event::ToolDenied { .. }
            | Event::ToolUse { .. }
            | Event::ToolBatchEnded { .. } => {
                if !says_nothing_now {
                    candidate.last_turn = None;
                }
            }
        }
    }
}

/// What the briefing would have said, replayed over a log.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Replay {
    /// One count per outcome, named the way the log names it.
    pub verdicts: BTreeMap<&'static str, u64>,
    /// How many conversations a briefing named, summed. Divided by the number
    /// of briefings it is the average width, which is what says whether
    /// `BRIEF_MAX` is doing anything.
    pub named: u64,
}

impl Replay {
    pub fn total(&self) -> u64 {
        self.verdicts.values().sum()
    }
}

/// Ask, at every conversation's first prompt, what it would have been told.
///
/// The point of a pure decision: this runs over the whole log offline, so "it
/// speaks this often, about this many conversations, and stays quiet for these
/// reasons" is a measurement rather than a hope — available before a word is
/// injected anywhere. Faithful to the moment: each conversation is judged
/// against only what had already happened when its first prompt arrived.
///
/// Records are read in timestamp order, not append order. Backfill appends
/// months-old conversations to the end of the log, so the two are not the same;
/// in append order the older conversation would be briefed about the newer one.
///
/// The counterfactual that used to sit beside this is gone with the rules it
/// existed for. It was there because the same-harness and live-authored rules
/// refused a single-vendor corpus outright, leaving the real replay unable to
/// say anything. Neither rule survives — a briefing about another Claude
/// session in this checkout is as useful as one about a Codex session, and a
/// reconstructed conversation is a fine thing to narrate as long as it is
/// marked — so the measurement is now informative on its own.
pub fn replay(records: &[Record], home: Option<&str>) -> Replay {
    let mut by_time: Vec<&Record> = records.iter().collect();
    by_time.sort_unstable_by_key(|record| (record.ts, record.seq));

    // Which checkout each conversation happened in, read from the whole log
    // rather than incrementally.
    //
    // This is the one place the replay looks ahead, and it looks ahead at a
    // fact the live path never had to guess: `cwd` is on every hook payload, so
    // a running daemon knows the workspace of the session in front of it and of
    // every session it has already seen. What it does *not* reconstruct is a
    // conversation's state — turn count, how the last turn ended, when it last
    // moved — which stays strictly as-of, because that is what is being judged.
    // Records written before the workspace was recorded carry it only on a
    // session record appended later, at the conversation's own opening time but
    // with a much larger sequence number, so an incremental read puts it after
    // the prompt it belongs to and answers `no_workspace` for all 3,433 of them.
    let mut workspace_of: HashMap<BusId, String> = HashMap::new();
    let mut entrypoint_of: HashMap<BusId, Entrypoint> = HashMap::new();
    for record in records {
        if let Some(workspace) = &record.source.workspace {
            workspace_of.insert(record.bus_id, workspace.clone());
        }
        // Same reasoning as the workspace, and the same reason it has to be
        // read from the whole log: a conversation recovered before the session
        // stamp held the entrypoint carries it only on a session record
        // appended later, so an incremental read finds nothing on the prompt
        // being judged.
        if let Some(entrypoint) = &record.source.entrypoint {
            entrypoint_of.insert(record.bus_id, entrypoint.clone());
        }
    }

    let mut out = Replay::default();
    let mut live: HashMap<BusId, Candidate> = HashMap::new();
    // `live` only grows — `Candidate::apply` inserts, never removes — so a
    // bus's membership is fixed at first sight and the workspace it indexes
    // by is the whole-log one, not the candidate's own. Without it, every
    // opening prompt scans every conversation seen so far.
    let mut live_by_workspace: HashMap<String, HashSet<BusId>> = HashMap::new();
    let mut asked: HashSet<BusId> = HashSet::new();

    for record in by_time {
        if matches!(record.event, Event::TurnStarted { .. }) && asked.insert(record.bus_id) {
            let workspace = workspace_of.get(&record.bus_id).cloned();
            // Only this checkout. Everything else would be filtered anyway, and
            // handing over every conversation on the machine is what turns a
            // replay into a quadratic one.
            let candidates: Vec<Candidate> = match workspace.as_deref() {
                Some(workspace) => live_by_workspace
                    .get(workspace)
                    .into_iter()
                    .flatten()
                    .filter(|bus_id| **bus_id != record.bus_id)
                    .map(|bus_id| Candidate {
                        workspace: Some(workspace.to_string()),
                        ..live[bus_id].clone()
                    })
                    .collect(),
                None => Vec::new(),
            };
            let asking = Asking {
                harness: record.source.harness.clone(),
                workspace,
                first_prompt: true,
                unattended: unattended(entrypoint_of.get(&record.bus_id)),
                machine_authored: match &record.event {
                    Event::TurnStarted { prompt, .. } => machine_authored(prompt),
                    _ => false,
                },
                prompt_units: match &record.event {
                    Event::TurnStarted { prompt, .. } => opening_units(prompt),
                    _ => 0,
                },
            };
            let name = match decide(&asking, &candidates, home, record.ts) {
                Decision::Brief { recent } => {
                    out.named += recent.len() as u64;
                    "brief"
                }
                Decision::Silent(reason) => match reason {
                    Silent::NoWorkspace => "silent/no_workspace",
                    Silent::WorkspaceTooBroad => "silent/workspace_too_broad",
                    Silent::NotFirstPrompt => "silent/not_first_prompt",
                    Silent::NothingToSay => "silent/nothing_to_say",
                    Silent::Unattended => "silent/unattended",
                    Silent::ArrivedWithContext => "silent/arrived_with_context",
                    Silent::MachineAuthored => "silent/machine_authored",
                },
            };
            *out.verdicts.entry(name).or_default() += 1;
        }
        let fresh = !live.contains_key(&record.bus_id);
        Candidate::apply(&mut live, record);
        if fresh && let Some(workspace) = workspace_of.get(&record.bus_id) {
            live_by_workspace
                .entry(workspace.clone())
                .or_default()
                .insert(record.bus_id);
        }
    }
    out
}

/// Whether an entrypoint means nobody is reading.
///
/// The entrypoint, and nothing else. The vendor's `promptSource` looked like a
/// second route to the same fact and turned out to be a trap: `sdk` is stamped
/// on hand-typed prompts throughout this corpus — including one that still
/// carries the stray leading keystroke of somebody typing — so a gate that read
/// it as "not a person" would go silent on precisely the sessions worth
/// speaking to. It is 9,354 of 12,053 turns, so the mistake would not have been
/// a small one.
///
/// An unknown is treated as a person. That is the safe direction: the cost of
/// briefing a program is some tokens nobody reads, and the cost of staying
/// silent on a person is the whole feature.
pub fn unattended(entrypoint: Option<&Entrypoint>) -> bool {
    entrypoint.is_some_and(Entrypoint::is_unattended)
}

/// The size of an opening prompt, in the units everything else here counts in.
///
/// An elided body still counts: retention took the text, not the fact that it
/// was long, and a session that opened with four kilobytes of its own brief is
/// the same session whether or not we kept the words.
pub fn opening_units(prompt: &crate::event::Body) -> usize {
    match prompt {
        crate::event::Body::Kept { text } => crate::pack::units(text),
        // A clipped body knows what it was before clipping, in bytes. Bytes
        // over-count non-ASCII, which errs towards silence — the safe side.
        crate::event::Body::Clipped { original_bytes, .. }
        | crate::event::Body::Elided {
            bytes: original_bytes,
        }
        | crate::event::Body::Sealed { original_bytes, .. } => *original_bytes,
    }
}

/// Whether an opening prompt says, in the vendor's own words, that a program
/// wrote it.
///
/// Two tells, both verbatim from the vendors: Claude Code wraps a scheduled
/// task's run in `<scheduled-task name="…">`, and Codex's memory agent opens
/// with `## Memory Writing Agent`. Nothing looser — a person asking about a
/// scheduled task is still a person, and an elided body is retention's doing,
/// not a machine's.
pub fn machine_authored(prompt: &crate::event::Body) -> bool {
    const TELLS: [&str; 2] = ["<scheduled-task", "## Memory Writing Agent"];
    match prompt {
        crate::event::Body::Kept { text } => {
            let head = text.trim_start();
            TELLS.iter().any(|tell| head.starts_with(tell))
        }
        crate::event::Body::Clipped { .. }
        | crate::event::Body::Elided { .. }
        | crate::event::Body::Sealed { .. } => false,
    }
}

/// How a conversation stopped, as the briefing would put it.
pub fn stopped(candidate: &Candidate) -> Stopped {
    match candidate.last_turn.as_ref() {
        None => Stopped::InFlight,
        Some(TurnState::InterruptedQuota { .. }) => Stopped::Displaced,
        Some(TurnState::InterruptedPolicy { .. }) => Stopped::Refused,
        Some(TurnState::Complete) => Stopped::Finished,
        Some(TurnState::InterruptedUser) => Stopped::Abandoned,
    }
}

/// Decide what this session should be told about its checkout.
pub fn decide(
    asking: &Asking,
    candidates: &[Candidate],
    home: Option<&str>,
    now: Timestamp,
) -> Decision {
    if !asking.first_prompt {
        return Decision::Silent(Silent::NotFirstPrompt);
    }
    if asking.unattended {
        return Decision::Silent(Silent::Unattended);
    }
    if asking.machine_authored {
        return Decision::Silent(Silent::MachineAuthored);
    }
    if asking.prompt_units > SELF_SUPPLIED_UNITS {
        return Decision::Silent(Silent::ArrivedWithContext);
    }
    let Some(workspace) = asking.workspace.as_deref() else {
        return Decision::Silent(Silent::NoWorkspace);
    };
    if workspace == "/" || Some(workspace) == home {
        return Decision::Silent(Silent::WorkspaceTooBroad);
    }

    let mut worth: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| {
            // This checkout only. Worktrees of one repository stay distinct,
            // deliberately: the branch in one is not the branch in another, and
            // this project is being written in one.
            candidate.workspace.as_deref() == Some(workspace) && candidate.turns >= MIN_TURNS && {
                let age = now.seconds_since(candidate.last_activity);
                // Negative means the record is stamped later than the
                // question, which happens: backfilled records carry the
                // time they happened and a wall clock can step backwards.
                // Read as "very recent" it would make the stalest candidate
                // the most attractive one.
                (0..=BRIEF_WINDOW_SECS).contains(&age)
            }
        })
        .collect();

    if worth.is_empty() {
        return Decision::Silent(Silent::NothingToSay);
    }

    // Most recent first, and a stable tie-break so the same log always briefs
    // the same way — a briefing that reorders itself between runs cannot be
    // tested and cannot be trusted.
    worth.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.bus_id.cmp(&b.bus_id))
    });
    // A conversation that stopped because it could not continue goes first
    // whatever its age within the window. It is the only stop that was not
    // somebody's choice, and it is the one a person opening another harness is
    // most likely to be here about.
    if let Some(displaced) = worth
        .iter()
        .position(|candidate| stopped(candidate) == Stopped::Displaced)
    {
        let first = worth.remove(displaced);
        worth.insert(0, first);
    }

    Decision::Brief {
        recent: worth
            .iter()
            .take(BRIEF_MAX)
            .map(|candidate| candidate.bus_id)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Body, Source};

    const WORKSPACE: &str = "/Applications/conbus";
    const NOW: &str = "2026-09-01T12:00:00.000Z";
    const DAY: i64 = 24 * 60 * 60;

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
            unattended: false,
            machine_authored: false,
            prompt_units: 40,
        }
    }

    fn candidate() -> Candidate {
        Candidate {
            bus_id: BusId::mint(),
            harness: Harness::new(Harness::CLAUDE_CODE),
            workspace: Some(WORKSPACE.into()),
            last_activity: ago(120),
            last_turn: Some(TurnState::Complete),
            turn_ended_at: None,
            turns: 6,
            backfilled: false,
            live: false,
        }
    }

    fn brief(candidates: &[Candidate]) -> Decision {
        decide(&asking(), candidates, Some("/Users/someone"), now())
    }

    fn named(decision: &Decision) -> Vec<BusId> {
        match decision {
            Decision::Brief { recent } => recent.clone(),
            Decision::Silent(reason) => panic!("expected a briefing, got {reason:?}"),
        }
    }

    #[test]
    fn a_checkout_with_recent_work_gets_told_about_it() {
        let subject = candidate();

        assert_eq!(
            named(&brief(std::slice::from_ref(&subject))),
            vec![subject.bus_id]
        );
    }

    #[test]
    fn the_same_harness_is_briefed_about_too() {
        // The old design refused this, on the argument that the vendor's own
        // resume covers it. It does not: resume covers *that session*, not the
        // other three the same person left open in this repo yesterday.
        let same = Candidate {
            harness: Harness::new(Harness::CODEX),
            ..candidate()
        };

        assert_eq!(
            named(&brief(std::slice::from_ref(&same))),
            vec![same.bus_id]
        );
    }

    #[test]
    fn a_reconstructed_conversation_is_briefed_about_too() {
        // Also refused before, because an *edge* is a live-path fact. A
        // briefing makes no such claim — it narrates, and a narration can carry
        // its own provenance.
        let recovered = Candidate {
            backfilled: true,
            live: false,
            ..candidate()
        };

        assert_eq!(
            named(&brief(std::slice::from_ref(&recovered))),
            vec![recovered.bus_id]
        );
    }

    #[test]
    fn several_conversations_are_a_briefing_and_not_a_refusal() {
        // The case that killed the previous design outright. Three threads in
        // one checkout was `Ambiguous`, and the answer was silence. It is not
        // ambiguous at all once nothing is being claimed: it is three facts.
        let old = Candidate {
            last_activity: ago(3 * DAY),
            ..candidate()
        };
        let middle = Candidate {
            bus_id: BusId::mint(),
            last_activity: ago(2 * DAY),
            ..candidate()
        };
        let recent = Candidate {
            bus_id: BusId::mint(),
            last_activity: ago(60),
            ..candidate()
        };

        assert_eq!(
            named(&brief(&[old.clone(), recent.clone(), middle.clone()])),
            vec![recent.bus_id, middle.bus_id, old.bus_id],
            "most recent first"
        );
    }

    #[test]
    fn a_conversation_that_could_not_continue_is_named_first() {
        // The only stop that was not somebody's choice, and the likeliest
        // reason a person is opening another harness at all — so it leads,
        // whatever its age inside the window.
        let displaced = Candidate {
            bus_id: BusId::mint(),
            last_activity: ago(4 * DAY),
            last_turn: Some(TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            }),
            ..candidate()
        };
        let newer = Candidate {
            bus_id: BusId::mint(),
            last_activity: ago(60),
            ..candidate()
        };

        assert_eq!(
            named(&brief(&[newer.clone(), displaced.clone()])),
            vec![displaced.bus_id, newer.bus_id]
        );
    }

    #[test]
    fn a_briefing_is_capped_so_it_stays_orientation() {
        let many: Vec<Candidate> = (0..10)
            .map(|n| Candidate {
                bus_id: BusId::mint(),
                last_activity: ago(60 * (n + 1)),
                ..candidate()
            })
            .collect();

        assert_eq!(
            named(&brief(&many)).len(),
            BRIEF_MAX,
            "past a few names it is a directory listing, and gets skipped"
        );
    }

    #[test]
    fn the_order_does_not_depend_on_the_order_it_was_handed() {
        // A briefing that reorders itself between runs cannot be tested and
        // cannot be trusted. Two conversations stopped in the same millisecond
        // is the case a naive sort leaves to chance.
        let tie = |bus_id| Candidate {
            bus_id,
            last_activity: ago(60),
            ..candidate()
        };
        let a = tie(BusId::mint());
        let b = tie(BusId::mint());

        assert_eq!(
            named(&brief(&[a.clone(), b.clone()])),
            named(&brief(&[b, a]))
        );
    }

    #[test]
    fn one_turn_conversations_are_removed_before_anything_else_is_asked() {
        // On the real log this single condition removes 3,179 of 3,434.
        assert_eq!(
            brief(&[Candidate {
                turns: 1,
                ..candidate()
            }]),
            Decision::Silent(Silent::NothingToSay)
        );
    }

    #[test]
    fn work_older_than_the_window_stops_paying_for_its_tokens() {
        assert!(matches!(
            brief(&[Candidate {
                last_activity: ago(BRIEF_WINDOW_SECS - 60),
                ..candidate()
            }]),
            Decision::Brief { .. }
        ));
        assert_eq!(
            brief(&[Candidate {
                last_activity: ago(BRIEF_WINDOW_SECS + 60),
                ..candidate()
            }]),
            Decision::Silent(Silent::NothingToSay)
        );
    }

    #[test]
    fn another_checkout_is_another_piece_of_work() {
        // Worktrees of one repository are deliberately distinct: this project
        // is being written in one, and the branch in it is not the branch in
        // the main checkout.
        assert_eq!(
            brief(&[Candidate {
                workspace: Some(format!("{WORKSPACE}/.claude/worktrees/other")),
                ..candidate()
            }]),
            Decision::Silent(Silent::NothingToSay)
        );
    }

    #[test]
    fn nobody_is_briefed_when_nobody_is_reading() {
        // The single condition that separates a feature from chrome. On the
        // real log all but 78 of 3,443 conversations are explicitly somebody's
        // script (3,094 of 3,443 by entrypoint, 2026-09-02); briefing those is
        // injections nobody reads.
        assert_eq!(
            decide(
                &Asking {
                    unattended: true,
                    machine_authored: false,
                    ..asking()
                },
                &[candidate()],
                None,
                now()
            ),
            Decision::Silent(Silent::Unattended)
        );
    }

    #[test]
    fn an_unlabelled_session_is_briefed_rather_than_assumed_to_be_a_script() {
        // The vendor labels machine origins and almost never labels human
        // ones, so treating "unlabelled" as unattended would silence nearly
        // every real person.
        assert!(matches!(
            decide(
                &Asking {
                    unattended: false,
                    machine_authored: false,
                    ..asking()
                },
                &[candidate()],
                None,
                now()
            ),
            Decision::Brief { .. }
        ));
    }

    #[test]
    fn a_session_that_brought_its_own_brief_is_left_alone() {
        // The gate that stops this being chrome. Among the sessions the earlier
        // gates let through, the median opening prompt is 2,273 units — a
        // scheduled task or a summariser pasting its whole instructions, not
        // somebody asking for something. And a person who does paste something
        // long has, by doing so, supplied the context a briefing would add.
        assert_eq!(
            decide(
                &Asking {
                    prompt_units: SELF_SUPPLIED_UNITS + 1,
                    ..asking()
                },
                &[candidate()],
                None,
                now()
            ),
            Decision::Silent(Silent::ArrivedWithContext)
        );
        assert!(matches!(
            decide(
                &Asking {
                    prompt_units: SELF_SUPPLIED_UNITS,
                    ..asking()
                },
                &[candidate()],
                None,
                now()
            ),
            Decision::Brief { .. }
        ));
    }

    #[test]
    fn an_opening_that_was_withheld_is_still_sized() {
        // Retention takes the text, not the fact that it was long. A session
        // that opened with four kilobytes of its own brief is the same session
        // whether or not the words were kept.
        assert_eq!(opening_units(&Body::kept("hello")), 5);
        assert_eq!(opening_units(&Body::Elided { bytes: 4_096 }), 4_096);
        assert_eq!(
            opening_units(&Body::Clipped {
                text: "head".into(),
                original_bytes: 9_000
            }),
            9_000
        );
    }

    #[test]
    fn a_later_turn_is_never_briefed() {
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
            Decision::Silent(Silent::NotFirstPrompt)
        );
    }

    #[test]
    fn a_checkout_broad_enough_to_hold_everything_is_refused() {
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
                Decision::Silent(Silent::WorkspaceTooBroad),
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
            Decision::Silent(Silent::NoWorkspace)
        );
    }

    #[test]
    fn a_conversation_stamped_after_the_question_is_not_read_as_fresh() {
        // Backfilled records carry the time they happened and a wall clock can
        // step backwards. A negative age read as "very recent" would put the
        // stalest thing in the log at the top of the briefing.
        assert_eq!(
            brief(&[Candidate {
                last_activity: ago(-60),
                ..candidate()
            }]),
            Decision::Silent(Silent::NothingToSay)
        );
    }

    #[test]
    fn how_a_conversation_stopped_is_described_rather_than_judged() {
        // Every value is sayable. Under the old design two of these four were
        // silence.
        let with = |last_turn| Candidate {
            last_turn,
            ..candidate()
        };

        assert_eq!(
            stopped(&with(Some(TurnState::InterruptedQuota {
                reason: "rate_limit".into()
            }))),
            Stopped::Displaced
        );
        assert_eq!(
            stopped(&with(Some(TurnState::InterruptedPolicy {
                reason: "billing_error".into()
            }))),
            Stopped::Refused
        );
        assert_eq!(stopped(&with(Some(TurnState::Complete))), Stopped::Finished);
        assert_eq!(
            stopped(&with(Some(TurnState::InterruptedUser))),
            Stopped::Abandoned
        );
        assert_eq!(stopped(&with(None)), Stopped::InFlight);
    }

    #[test]
    fn only_the_entrypoint_decides_whether_anybody_is_there() {
        assert!(
            !unattended(None),
            "an unknown is treated as a person, because the cost of briefing a \
             program is tokens and the cost of silencing a person is the feature"
        );
        assert!(unattended(Some(&Entrypoint::new(Entrypoint::SDK_CLI))));
        assert!(!unattended(Some(&Entrypoint::new(
            Entrypoint::CLAUDE_DESKTOP
        ))));
        assert!(!unattended(Some(&Entrypoint::new(Entrypoint::CLI))));
    }

    #[test]
    fn the_vendors_own_authorship_label_is_not_consulted() {
        // It looked like a second route to the same fact and is a trap. `sdk`
        // is stamped on hand-typed prompts throughout the real corpus — 9,354
        // of 12,053 turns — including one that still carries the stray leading
        // keystroke of somebody typing. Reading it as "not a person" would go
        // silent on exactly the sessions worth speaking to.
        let by_hand_but_labelled_sdk = Record {
            event: Event::TurnStarted {
                author: Some(crate::event::Author::new(crate::event::Author::SDK)),
                prompt: Body::kept("각 워크스페이스가 하고있는 역할 정리해줘"),
            },
            ..logged(BusId::mint(), ago(60), prompt())
        };

        let replayed = replay(std::slice::from_ref(&by_hand_but_labelled_sdk), None);

        assert_eq!(
            replayed.verdicts.get("silent/unattended"),
            None,
            "a person typed this, whatever the vendor labelled it"
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
            hook_id: None,
            spooled_at: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some("s".into()),
                workspace: Some(WORKSPACE.into()),
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            },
            event,
        }
    }

    fn prompt() -> Event {
        Event::TurnStarted {
            author: None,
            prompt: Body::kept("p"),
        }
    }

    fn ended(state: TurnState) -> Event {
        Event::TurnEnded {
            state,
            assistant_message: None,
            tokens: None,
        }
    }

    #[test]
    fn the_index_reads_a_conversation_out_of_its_own_records() {
        let bus_id = BusId::mint();
        let records = vec![
            logged(bus_id, ago(300), Event::SessionCandidate { trigger: None }),
            logged(bus_id, ago(280), prompt()),
            logged(bus_id, ago(260), ended(TurnState::Complete)),
            logged(bus_id, ago(200), prompt()),
            logged(
                bus_id,
                ago(120),
                ended(TurnState::InterruptedQuota {
                    reason: "rate_limit".into(),
                }),
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
            stopped(only),
            Stopped::Displaced,
            "closing the window does not undo how the conversation ended"
        );
        assert!(!only.backfilled);
    }

    #[test]
    fn a_prompt_that_says_a_machine_wrote_it_is_not_briefed() {
        // Four scheduled-task runs on a real log were `claude-desktop`
        // sessions — attended, by the entrypoint rule — and Codex's memory
        // agent opens with its own instruction prompt under a person's hooks.
        // Both begin with the vendor's own tell. A briefing is for a person
        // who was not there; these are programs that were.
        let asking = Asking {
            harness: Harness::new(Harness::CLAUDE_CODE),
            workspace: Some(WORKSPACE.into()),
            first_prompt: true,
            unattended: false,
            machine_authored: true,
            prompt_units: 40,
        };

        assert_eq!(
            decide(&asking, &[], None, now()),
            Decision::Silent(Silent::MachineAuthored)
        );
    }

    #[test]
    fn the_machine_tells_are_the_vendors_own_and_nothing_looser() {
        assert!(machine_authored(&Body::kept(
            "<scheduled-task name=\"nightly-report\" file=\"/x\">run it</scheduled-task>"
        )));
        assert!(machine_authored(&Body::kept(
            "## Memory Writing Agent: Phase 2 (Consolidation)\n\nYou are…"
        )));
        assert!(
            !machine_authored(&Body::kept("fix the scheduled task that broke")),
            "a person talking about a scheduled task is still a person"
        );
        assert!(
            !machine_authored(&Body::Elided { bytes: 900 }),
            "retention took the text; absent is not a machine"
        );
    }

    #[test]
    fn a_conversation_with_live_and_reconstructed_records_knows_both() {
        // The pack labelled a conversation with one backfilled record among
        // 1,786 live ones "reconstructed from the vendor's history, not
        // observed live" — a first-hand fact dressed as hearsay. Both flags,
        // so the label can say "partly".
        let bus_id = BusId::mint();
        let mut reconstructed = logged(bus_id, ago(300), prompt());
        reconstructed.source.backfilled_from = Some("claude_transcript".into());
        let records = vec![reconstructed, logged(bus_id, ago(100), prompt())];

        let index = Candidate::index(&records);

        assert!(index[0].backfilled);
        assert!(index[0].live);
    }

    #[test]
    fn a_conversation_in_flight_is_indexed_as_in_flight() {
        let bus_id = BusId::mint();
        let records = vec![
            logged(bus_id, ago(300), prompt()),
            logged(bus_id, ago(280), ended(TurnState::Complete)),
            logged(bus_id, ago(200), prompt()),
            logged(
                bus_id,
                ago(100),
                Event::ToolRequested {
                    tool: "Read".into(),
                    tool_use_id: None,
                    input: Body::kept("i"),
                },
            ),
        ];

        assert_eq!(stopped(&Candidate::index(&records)[0]), Stopped::InFlight);
    }

    fn tool_call() -> Event {
        Event::ToolRequested {
            tool: "Read".into(),
            tool_use_id: None,
            input: Body::kept("i"),
        }
    }

    /// A record that landed `at` and first tried to at `from`.
    fn waited(bus_id: BusId, at: Timestamp, from: Timestamp, event: Event) -> Record {
        Record {
            spooled_at: Some(from),
            ..logged(bus_id, at, event)
        }
    }

    /// Two turns, the last ended 100 seconds ago with `last`, and then what landed.
    fn after_the_last_turn(bus_id: BusId, last: TurnState, landed: Record) -> Candidate {
        let records = vec![
            logged(bus_id, ago(300), prompt()),
            logged(bus_id, ago(280), ended(TurnState::Complete)),
            logged(bus_id, ago(200), prompt()),
            logged(bus_id, ago(100), ended(last)),
            landed,
        ];
        Candidate::index(&records).remove(0)
    }

    #[test]
    fn a_tool_event_that_landed_behind_the_turn_end_does_not_reopen_the_turn() {
        // Parked for the prompt that bound its session, or overtaken by a live
        // hook. The turn it belonged to is over, and saying "still mid-turn" to
        // the next session would stay true for as long as nothing else happens.
        let bus_id = BusId::mint();
        let late = waited(bus_id, ago(60), ago(150), tool_call());

        let candidate = after_the_last_turn(bus_id, TurnState::Complete, late);

        assert_eq!(stopped(&candidate), Stopped::Finished);
    }

    #[test]
    fn a_tool_event_that_landed_late_does_not_clear_a_quota_stop() {
        // The one stop a person has to be told about, the one `decide` leads with
        // and the one the next leg's `continued` edge depends on.
        let bus_id = BusId::mint();
        let quota = TurnState::InterruptedQuota {
            reason: "rate_limit".into(),
        };
        let late = waited(bus_id, ago(60), ago(150), tool_call());

        let candidate = after_the_last_turn(bus_id, quota, late);

        assert_eq!(stopped(&candidate), Stopped::Displaced);
        let other = Candidate {
            bus_id: BusId::mint(),
            last_activity: ago(10),
            last_turn: Some(TurnState::Complete),
            ..candidate.clone()
        };
        assert_eq!(
            named(&brief(&[other, candidate.clone()]))[0],
            candidate.bus_id,
            "it still leads"
        );
    }

    #[test]
    fn a_prompt_its_own_turn_end_overtook_does_not_reopen_the_turn() {
        // The same order from a spooled prompt: its hook missed the daemon, the
        // turn's end did not, and the drain came after both.
        let bus_id = BusId::mint();
        let late = waited(bus_id, ago(60), ago(150), prompt());

        let candidate = after_the_last_turn(bus_id, TurnState::Complete, late);

        assert_eq!(stopped(&candidate), Stopped::Finished);
        assert_eq!(candidate.turns, 3, "it still happened");
    }

    #[test]
    fn a_tool_event_that_waited_but_came_after_the_turn_end_is_news() {
        // Its prompt hook never fired, and it is the only sign a turn is running.
        let bus_id = BusId::mint();
        let later = waited(bus_id, ago(50), ago(60), tool_call());

        let candidate = after_the_last_turn(bus_id, TurnState::Complete, later);

        assert_eq!(stopped(&candidate), Stopped::InFlight);
    }

    #[test]
    fn a_tool_event_that_did_not_wait_is_news() {
        let bus_id = BusId::mint();
        let live = logged(bus_id, ago(50), tool_call());

        let candidate = after_the_last_turn(bus_id, TurnState::Complete, live);

        assert_eq!(stopped(&candidate), Stopped::InFlight);
    }

    #[test]
    fn a_turn_end_that_waited_is_held_against_when_it_happened() {
        // It landed after everything here, and it happened before the tool call
        // did, so that tool call is a new turn running.
        let bus_id = BusId::mint();
        let records = vec![
            logged(bus_id, ago(300), prompt()),
            waited(bus_id, ago(50), ago(200), ended(TurnState::Complete)),
            waited(bus_id, ago(40), ago(100), tool_call()),
        ];

        assert_eq!(stopped(&Candidate::index(&records)[0]), Stopped::InFlight);
    }

    #[test]
    fn an_older_turn_end_that_landed_late_does_not_move_the_line_back() {
        let bus_id = BusId::mint();
        let records = vec![
            logged(bus_id, ago(300), prompt()),
            logged(bus_id, ago(100), ended(TurnState::Complete)),
            waited(bus_id, ago(90), ago(250), ended(TurnState::Complete)),
            waited(bus_id, ago(80), ago(150), tool_call()),
        ];

        assert_eq!(stopped(&Candidate::index(&records)[0]), Stopped::Finished);
    }

    #[test]
    fn a_tool_event_that_waited_still_reads_as_late_through_the_cache() {
        // What the daemon folds at start, and what `hop` reads: closed segments
        // come back from their sidecars, and a fold that lost the field there
        // would turn a finished conversation back into a running one at a restart.
        use crate::retention::Retention;
        use crate::segment::SegmentLog;
        let home = tempfile::tempdir().expect("tempdir");
        let dir = home.path().join("log");
        let cache = home.path().join("cache");
        let bus_id = BusId::mint();
        let entry = |record: Record| crate::record::Entry {
            bus_id: record.bus_id,
            observed_at: Some(record.ts),
            source: record.source,
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: record.spooled_at,
            event: record.event,
        };
        {
            let mut log =
                SegmentLog::open_with_segment_size(&dir, Retention::default(), 1).expect("open");
            for record in [
                logged(bus_id, ago(300), prompt()),
                logged(bus_id, ago(100), ended(TurnState::Complete)),
                waited(bus_id, ago(60), ago(150), tool_call()),
                logged(bus_id, ago(50), Event::SessionEnded { reason: None }),
            ] {
                log.append(entry(record)).expect("append");
            }
        }
        // Once to write the sidecars, and once to read them back.
        for read in ["fresh", "from the sidecars"] {
            let mut folded = HashMap::new();
            SegmentLog::scan_meta_each(&dir, Some(&cache), |record| {
                Candidate::apply(&mut folded, &record);
            })
            .expect("scan");
            assert_eq!(stopped(&folded[&bus_id]), Stopped::Finished, "{read}");
        }
        assert!(cache.join("meta").is_dir(), "the sidecars were really used");
    }

    #[test]
    fn the_replay_judges_each_conversation_against_only_what_came_before_it() {
        let first = BusId::mint();
        let second = BusId::mint();
        let records = vec![
            logged(first, ago(600), prompt()),
            logged(first, ago(500), ended(TurnState::Complete)),
            logged(first, ago(400), prompt()),
            logged(
                first,
                ago(300),
                ended(TurnState::InterruptedQuota {
                    reason: "rate_limit".into(),
                }),
            ),
            logged(second, ago(180), prompt()),
        ];

        let replay = replay(&records, None);

        assert_eq!(replay.total(), 2, "one verdict per conversation");
        assert_eq!(replay.verdicts.get("brief"), Some(&1));
        assert_eq!(replay.named, 1);
        assert_eq!(
            replay.verdicts.get("silent/nothing_to_say"),
            Some(&1),
            "the first conversation had nothing before it"
        );
    }

    #[test]
    fn the_replay_reads_the_log_by_time_and_not_by_append_order() {
        // Backfill appends months-old conversations to the end of the log, so
        // append order and chronology disagree. Judged in append order, the
        // older conversation would be briefed about the newer one.
        let old = BusId::mint();
        let new = BusId::mint();
        let mut records = vec![
            logged(new, ago(100), prompt()),
            logged(new, ago(90), ended(TurnState::Complete)),
            logged(new, ago(80), prompt()),
            logged(old, ago(10_000), prompt()),
            logged(old, ago(9_900), ended(TurnState::Complete)),
            logged(old, ago(9_800), prompt()),
        ];
        for (seq, record) in records.iter_mut().enumerate() {
            record.seq = seq as u64;
        }

        let replay = replay(&records, None);

        // Read by time: the older conversation had nothing before it, the newer
        // one is briefed about it. Read in append order the newer one comes
        // first and finds nothing, and the older one then sees a conversation
        // stamped *after* the question it is answering — a negative age, which
        // is filtered — so both would be silent. One brief and one silence is
        // the shape only chronology produces.
        assert_eq!(replay.verdicts.get("brief"), Some(&1));
        assert_eq!(replay.verdicts.get("silent/nothing_to_say"), Some(&1));
    }
}
