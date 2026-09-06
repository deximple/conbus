//! The normalised, vendor-agnostic event a harness reports.

use serde::{Deserialize, Serialize};

use crate::ids::BusId;

/// The harness that produced a record.
///
/// Deliberately open rather than a closed enum: the plan's leg list changes with
/// the vendor landscape, and a log written by a newer build must still be
/// readable by an older one.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Harness(String);

impl Harness {
    pub const CLAUDE_CODE: &'static str = "claude_code";
    pub const CODEX: &'static str = "codex";
    /// Cursor invokes the Claude Code hook command with its own payload.
    /// Identified by `cursor_version`, never by guessing Claude field names.
    pub const CURSOR: &'static str = "cursor";
    /// Google Antigravity CLI (Agy). Not an auto-overflow destination.
    pub const AGY: &'static str = "agy";
    pub const OPENCODE: &'static str = "opencode";
    pub const GROK: &'static str = "grok";

    pub fn new(slug: impl Into<String>) -> Self {
        Harness(slug.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The vendor's identifier for one turn: a prompt and everything after it
/// until the next prompt.
///
/// Named for whose identifier it is, following `vendor_session_id`. The name
/// matters more than it looks: Claude Code also has a field literally called
/// `turn_id` (on `MessageDisplay`) whose value is *different* from the one we
/// record here — both appear on the same payload with different UUIDs. Calling
/// ours `turn_id` would claim the vendor's word for a different meaning, and
/// would collide with it the moment that event is mirrored, because `Record`
/// flattens the event body and a duplicate key makes the line unreadable.
///
/// Opaque on purpose. `BusId` parses strictly because Claude Code takes it back
/// verbatim as `--session-id`; nothing hands this one back, and a harness whose
/// correlator is not a UUID must not lose it to a parse.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VendorTurnId(String);

impl VendorTurnId {
    pub fn new(id: impl Into<String>) -> Self {
        VendorTurnId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The vendor's identifier for the subagent a record came from.
///
/// Present only when the hook fired inside a subagent. Without it a subagent's
/// tool calls are indistinguishable from the main thread's, because they arrive
/// under the *parent's* session id — so the log would interleave two agents'
/// work under one conversation with nothing on the line to separate them.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VendorAgentId(String);

impl VendorAgentId {
    pub fn new(id: impl Into<String>) -> Self {
        VendorAgentId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The vendor's identifier for one tool call.
///
/// The join key between a `tool_requested` and the `tool_use` or `tool_denied`
/// that answers it. Both sides are mirrored, so issue order and completion
/// order both reach the log and differ visibly under parallelism.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VendorToolUseId(String);

impl VendorToolUseId {
    pub fn new(id: impl Into<String>) -> Self {
        VendorToolUseId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Who caused a turn to exist.
///
/// Open for the same reason `Harness` is: this vendor's enums grow, and a log
/// written by a newer build must stay readable by an older one.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Author(String);

impl Author {
    // The five values Claude Code actually emits, counted across every
    // transcript on a real machine — 5,501 files, 61,899 main-thread prompts:
    // `sdk` 9,184, `system` 139, `typed` 23, `queued` 1, `suggestion_accepted`
    // 1, and 52,551 with no `promptSource` at all.
    //
    // An earlier version of this list had `user`, `loop_wakeup`,
    // `schedule_wakeup` and `poll_event`. None of the four appears anywhere in
    // that corpus, and `is_human` was written against `user` — so it could
    // never return true, and a survival meter built on it would have reported
    // that no person had ever used the product. `strings` on the binary does
    // not settle this either; it found none of them. The corpus is the
    // evidence.
    /// A person typed it.
    pub const TYPED: &'static str = "typed";
    /// A person queued it while a turn was running.
    pub const QUEUED: &'static str = "queued";
    /// A person accepted something the harness offered.
    pub const SUGGESTION_ACCEPTED: &'static str = "suggestion_accepted";
    /// Driven by the SDK. Not a person.
    pub const SDK: &'static str = "sdk";
    /// Generated by the harness itself. Not a person.
    pub const SYSTEM: &'static str = "system";

    pub fn new(author: impl Into<String>) -> Self {
        Author(author.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True only when the vendor said a person originated this turn.
    ///
    /// Absence is not humanity, and here that matters more than it sounds: the
    /// vendor leaves 85% of prompts unlabelled, so this is true for only a
    /// handful of turns even on a machine somebody uses all day. Use it to
    /// confirm a person, never to count them.
    pub fn is_human(&self) -> bool {
        matches!(
            self.0.as_str(),
            Self::TYPED | Self::QUEUED | Self::SUGGESTION_ACCEPTED
        )
    }

    /// True when the vendor named an origin that is not a person.
    ///
    /// The useful half of this surface. The vendor labels machine origins
    /// explicitly and human ones almost never, so the negative is the reliable
    /// direction: `sdk` and `system` are certainly not somebody sitting there,
    /// while unlabelled means nothing either way.
    pub fn is_machine(&self) -> bool {
        matches!(self.0.as_str(), Self::SDK | Self::SYSTEM)
    }
}

#[cfg(test)]
mod author_tests {
    use super::Author;

    #[test]
    fn the_author_vocabulary_is_the_one_the_vendor_writes() {
        // Counted across every transcript on a real machine. An earlier list
        // was taken from a schema rather than from traffic, and `is_human` was
        // written against a value — `user` — that the vendor has never once
        // emitted, so it could not return true for anybody.
        for human in [Author::TYPED, Author::QUEUED, Author::SUGGESTION_ACCEPTED] {
            let author = Author::new(human);
            assert!(author.is_human(), "{human} is a person");
            assert!(!author.is_machine(), "{human} is not a script");
        }
        for machine in [Author::SDK, Author::SYSTEM] {
            let author = Author::new(machine);
            assert!(author.is_machine(), "{machine} is a script");
            assert!(!author.is_human(), "{machine} is not a person");
        }
    }

    #[test]
    fn a_value_the_vendor_has_never_emitted_is_neither() {
        // Including the four this list used to contain. An open newtype has to
        // stay open — the vendor may grow a value tomorrow — but an unknown
        // value must not be counted as a person or as a script.
        for invented in ["user", "loop_wakeup", "schedule_wakeup", "poll_event"] {
            let author = Author::new(invented);
            assert!(!author.is_human(), "{invented} was never observed");
            assert!(!author.is_machine(), "{invented} was never observed");
        }
    }
}

/// Which front end a session is running under.
///
/// Open, like the other vendor vocabularies here. The values seen on a real
/// machine, counted over its transcripts: `claude-desktop` 634,363, `sdk-cli`
/// 75,906, `cli` 6,446. The binary also carries `vscode`, `jetbrains`,
/// `sdk-ts`, `sdk-py`, `mcp`, `github-action` and `remote`.
///
/// It answers a question nothing else on this surface can: whether a person is
/// sitting there. `Author` cannot — the vendor leaves 85% of prompts
/// unlabelled — and the hook payload carries no origin at all, but
/// `CLAUDE_CODE_ENTRYPOINT` is in every hook child's environment and the
/// transcript writes it on every line.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Entrypoint(String);

impl Entrypoint {
    pub const CLI: &'static str = "cli";
    pub const CLAUDE_DESKTOP: &'static str = "claude-desktop";
    pub const SDK_CLI: &'static str = "sdk-cli";

    pub fn new(entrypoint: impl Into<String>) -> Self {
        Entrypoint(entrypoint.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when nobody is watching this session.
    ///
    /// Named for the reliable direction. An SDK driver, an MCP server and a CI
    /// runner are programs, and a value this does not recognise is not
    /// evidence of a person — it is an unknown, and the caller has to say what
    /// it does with one.
    ///
    /// `remote` is deliberately not here. A session driven from another device
    /// still has somebody at the other end of it, and calling that unattended
    /// would silence the one case where being re-oriented matters most.
    pub fn is_unattended(&self) -> bool {
        self.0.starts_with("sdk-") || matches!(self.0.as_str(), "mcp" | "github-action")
    }
}

/// Where a record came from: the vendor stamp every record carries.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Source {
    pub harness: Harness,
    /// The harness version, when it tells us. Absent is normal, not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_version: Option<String>,
    /// The vendor's own session id, which is an alias of the `bus_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_session_id: Option<String>,
    /// The working directory the harness reported for this event.
    ///
    /// On the session stamp rather than on one event because both vendors put
    /// `cwd` on the common base of *every* hook payload, and because the thing
    /// it identifies — which checkout this conversation is happening in — is
    /// what a second harness needs in order to find the conversation it is
    /// continuing. Reading it only at the session start recorded it for the
    /// conversations whose start we happened to still be holding, which in a
    /// real log was 7 out of 3,433.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The front end this session runs under, when the harness says. On the
    /// session stamp because it is a property of the session and because it is
    /// the only durable answer to "was a person there" — the question every
    /// relevance decision and the whole survival meter rest on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Entrypoint>,
    /// Set when the record was reconstructed from the vendor's own history
    /// rather than mirrored as it happened.
    ///
    /// A backfilled record is a weaker fact than a live one — it is whatever a
    /// one-shot parser could recover from a format the vendor calls internal —
    /// and a reader that cannot tell the two apart will draw conclusions the
    /// log does not support. Names the source, e.g. `claude_transcript`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfilled_from: Option<String>,
}

/// How a turn ended.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TurnState {
    Complete,
    /// The vendor refused on quota. This is the one the router acts on.
    InterruptedQuota {
        reason: String,
    },
    /// The vendor refused on policy, billing, or auth.
    InterruptedPolicy {
        reason: String,
    },
    InterruptedUser,
}

/// Why a conversation moved to another harness.
///
/// Counted separately: failover (`displaced`) is how often a limit was hit,
/// which we do not control; routing (`handed_off`) is the wedge.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hop {
    /// The current leg said it could not continue — quota, or an account that
    /// is already exhausted. The router acts on this.
    Displaced,
    /// The bus moved the work before the limit, because the window was almost
    /// gone and the reset was far enough away that staying would waste a turn.
    HandedOff,
}

/// What a harness reported, normalised across vendors.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A vendor session announced itself. No `bus_id` is minted here: some of
    /// these are ghosts with no transcript (anthropics/claude-code#78455).
    SessionCandidate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
    },
    /// The user submitted a prompt. This is where a `bus_id` is minted,
    /// whatever the author turns out to be.
    TurnStarted {
        /// Declared before `prompt` so it stays visible in the first bytes of
        /// the line; a prompt can run to the 16 KiB inline cap.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author: Option<Author>,
        prompt: Body,
    },
    /// The model asked for a tool, before it ran.
    ///
    /// Its own event rather than a third `ToolOutcome`, because this is a
    /// different moment in time — a request and its result are two facts, and
    /// the log is append-only, so a later record cannot amend an earlier one.
    /// (`PostToolUse` and `PostToolUseFailure` are different: two exclusive
    /// descriptions of the *same* moment, which is why those share one event.)
    ///
    /// The input is recorded here as well as on the result, and the two can
    /// legitimately differ: a `PreToolUse` hook is allowed to rewrite a tool's
    /// input, so "what was asked for" and "what ran" are separate facts.
    ToolRequested {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<VendorToolUseId>,
        input: Body,
    },
    /// A batch of tool calls all resolved, and the model is about to be asked
    /// again.
    ///
    /// Records the grouping and the boundary, not the calls. The vendor's
    /// payload repeats every call's name, input and response, and all of those
    /// are already on disk from the per-call events — mirroring them would put
    /// each body there a third time. What the log does *not* otherwise hold is
    /// which calls were issued together, and where the model's next request
    /// begins. So this is a list of ids, joinable to the records that carry the
    /// substance.
    ToolBatchEnded {
        /// In the order the vendor listed them.
        tool_calls: Vec<VendorToolUseId>,
        /// Entries the vendor sent that could not be named.
        ///
        /// Without this, "the batch held no calls" and "the batch held calls we
        /// could not read" are the same line, and the second is a vendor change
        /// worth noticing. The schema marks the id required, so a non-zero
        /// count here means the shape moved.
        #[serde(default, skip_serializing_if = "is_zero")]
        unreadable: usize,
    },
    /// The harness asked a person whether a tool may run.
    ///
    /// This is not a refusal and not a request-to-run. `PermissionDenied` is
    /// only the auto-mode classifier; an ordinary no at the prompt has no
    /// later event at all. Recording the ask is what makes that silence
    /// visible — a `permission_asked` with no following `tool_use` is the
    /// refusal. Mapping this to `ToolDenied` would log every approval as one.
    ///
    /// Claude Code's payload has no `tool_use_id` here (the emitter has the
    /// id and does not put it on the hook). Join is by tool name and
    /// adjacency, not by key.
    PermissionAsked { tool: String, input: Body },
    /// A vendor notification reached a hook.
    ///
    /// Claude Code only. `kind` is the vendor's `notification_type`, kept
    /// verbatim — one of those values is the only hook-channel signal that a
    /// quota limit has lifted, and guessing the spelling before a capture
    /// would repeat the `rate_limit_reached` mistake. `message` is chrome,
    /// not conversation, so it is a string rather than a `Body`.
    Notified {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        message: String,
    },
    /// A tool the harness refused to run.
    ///
    /// Its own event rather than a third `ToolOutcome`, because `ToolUse` means
    /// the tool *ran*: a call that never executed sitting in that set would
    /// skew any aggregation over executions — durations, failure rates — unless
    /// every reader remembered to filter it out. It also shares almost nothing
    /// with a result: no duration, no output, no error from the tool.
    ///
    /// Without it, mirroring the request side leaves a `tool_requested` with no
    /// resolution and nothing saying why.
    ToolDenied {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<VendorToolUseId>,
        input: Body,
        /// Why the harness refused. A plain string, not a `Body`: retention
        /// withholds tool *result* bodies, and this is the harness explaining
        /// itself, the same as a turn's interruption reason or a session's end
        /// reason. Eliding it would delete the only account of why a request
        /// never resolved, which is the whole point of recording it.
        reason: String,
    },
    /// A tool ran.
    ToolUse {
        tool: String,
        /// Declared before the bodies, which run to the 16 KiB inline cap, so
        /// the identity and the timing stay in the first bytes of the line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<VendorToolUseId>,
        /// How long the call took. Absent is not zero — but do not read it as
        /// "the vendor declined to measure" either: every `PostToolUse` path in
        /// 2.1.241 passes a measurement, so an absent duration means the vendor
        /// changed, or the value arrived in a shape we could not read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        input: Body,
        #[serde(flatten)]
        outcome: ToolOutcome,
    },
    /// The turn finished, one way or another.
    TurnEnded {
        #[serde(flatten)]
        state: TurnState,
        /// The assistant's closing text. Claude Code puts it on the payload
        /// itself precisely so a hook does not have to read the transcript —
        /// which is what removes the need for a flush barrier, not what lets
        /// the log hold the whole conversation. It is the *last* message's
        /// text blocks only, and absent when that message is pure tool use.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assistant_message: Option<Body>,
    },
    /// A briefing about this checkout was built and handed to the hook.
    ///
    /// Recorded for what is actually known. The daemon built this text and
    /// returned it; whether a model ever read it happens outside the log, in a
    /// process the daemon does not watch, so the record says "handed over" and
    /// not "delivered". Claiming delivery would put an unfalsifiable line in an
    /// append-only file.
    ///
    /// It carries no briefing text. Every word was quoted from records already
    /// here, and writing them again would double the log to say nothing new.
    Briefed {
        /// The conversations it narrated, in the order it named them.
        about: Vec<BusId>,
        /// How long it was, in the units the vendor's cap is measured in.
        units: usize,
        /// Conversations the budget would not fit.
        #[serde(default, skip_serializing_if = "is_zero")]
        dropped: usize,
    },
    /// The bus decided this conversation should continue on another harness.
    ///
    /// Written under the *predecessor*. Identity does not move: a later
    /// `continued` under the successor is the edge, and either line can be
    /// contradicted. `next` is a slug, not `harness` — that key is reserved
    /// for the envelope.
    Routed {
        next: String,
        hop: Hop,
        reason: String,
        /// How fresh the checkout looked when the hop was decided.
        ///
        /// `gh` failing must not block the hop; that case is recorded here
        /// as stale so the next pack can say so without calling `gh` again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code_freshness: Option<String>,
    },
    /// A session is a continuation of an earlier conversation.
    ///
    /// Written under the *successor*. The predecessor keeps its id. Merging
    /// two live trains of thought is forbidden; this records a relation that
    /// can be wrong.
    Continued { predecessor: BusId, hop: Hop },
    /// A rolling digest of the conversation, written in the background.
    ///
    /// Hop path model calls stay zero: this is extractive (or produced by a
    /// backend that is not the protected leg), never a call made while
    /// hopping. The digest is conversation text, so default retention keeps it.
    Summarised {
        backend: String,
        covered_turns: u64,
        digest: Body,
    },
    /// A single writer claimed this conversation for a directed hop.
    ///
    /// `holder` starting `warm:` names a Codex thread the bus opened.
    /// `warm-claude:` names a Claude session the bus opened. A later
    /// `conbus hop --apply` syncs that leg instead of refusing. Any other
    /// holder refuses until a later `session_ended` lands.
    Leased { holder: String },
    SessionEnded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl Event {
    /// The Codex thread a `leased` holder names, when the bus opened it.
    ///
    /// Identity is known on this path: the successor session is the thread we
    /// started, not a conversation the briefing inferred. `warm-claude:` is
    /// the other leg and does not alias a Codex thread. A holder that does
    /// not start `warm:` is somebody else's claim and does not alias.
    pub fn warm_thread(&self) -> Option<&str> {
        match self {
            Event::Leased { holder } if holder.starts_with("warm-claude:") => None,
            Event::Leased { holder } => holder.strip_prefix("warm:").filter(|id| !id.is_empty()),
            _ => None,
        }
    }

    /// The Claude session a `leased` holder names, when the bus opened it.
    pub fn claude_warm_session(&self) -> Option<&str> {
        match self {
            Event::Leased { holder } => holder
                .strip_prefix("warm-claude:")
                .filter(|id| !id.is_empty()),
            _ => None,
        }
    }
}

/// How a tool call ended.
///
/// One call produces `PostToolUse` **or** `PostToolUseFailure`, never both, so
/// this is one event with two endings rather than two events. Modelling it as
/// an enum is deliberate: every consumer of a tool record has to handle both,
/// and the compiler says so. A separate variant would let a reader forget the
/// failing half, and forgetting is silent.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolOutcome {
    Succeeded {
        output: Body,
    },
    Failed {
        /// The vendor's error text. A body, not a label: a failed shell command
        /// puts its output here, so retention treats it as a tool result.
        error: Body,
        /// The one signal anywhere in the hook surface that a person stopped
        /// the work. Nothing derives `TurnState::InterruptedUser` from it yet.
        #[serde(default, skip_serializing_if = "is_false")]
        interrupted: bool,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Text captured from a session, carrying whether it actually reached disk.
///
/// The variant is on the record itself rather than implied by configuration, so
/// that a reader can tell "the model said nothing" from "retention dropped it".
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// Written verbatim.
    Kept { text: String },
    /// Written, but clipped at the inline cap.
    Clipped { text: String, original_bytes: usize },
    /// Withheld by the workspace's retention level. Only its size survives.
    Elided { bytes: usize },
}

impl Body {
    pub fn kept(text: impl Into<String>) -> Self {
        Body::Kept { text: text.into() }
    }

    /// Bytes the body occupied before any clipping or elision.
    pub fn original_bytes(&self) -> usize {
        match self {
            Body::Kept { text } => text.len(),
            Body::Clipped { original_bytes, .. }
            | Body::Elided {
                bytes: original_bytes,
            } => *original_bytes,
        }
    }
}
