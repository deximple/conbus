//! The normalised, vendor-agnostic event a harness reports.

use serde::{Deserialize, Serialize};

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
    pub const USER: &'static str = "user";
    pub const SDK: &'static str = "sdk";
    pub const SYSTEM: &'static str = "system";
    pub const LOOP_WAKEUP: &'static str = "loop_wakeup";
    pub const SCHEDULE_WAKEUP: &'static str = "schedule_wakeup";
    pub const POLL_EVENT: &'static str = "poll_event";

    pub fn new(author: impl Into<String>) -> Self {
        Author(author.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True only when the vendor said a person typed this turn.
    ///
    /// Absence is not humanity. An `Option<Author>` of `None` means the vendor
    /// did not say, and belongs in its own bucket — counting it as human is how
    /// a survival meter reports a scheduled wakeup as a person.
    pub fn is_human(&self) -> bool {
        self.0 == Self::USER
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
    SessionEnded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
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
