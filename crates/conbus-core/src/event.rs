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
/// Recorded because it is the call's identity, not because anything joins on it
/// yet. Today the log holds one record per *successful* call, already carrying
/// its own input and output on the same line, so nothing here needs the id to
/// attribute a result to a request.
///
/// It becomes a join key when `PreToolUse` and `PostToolUseFailure` are
/// mirrored, which they are not. Note what that costs meanwhile: a tool call
/// that **fails** emits `PostToolUseFailure`, not `PostToolUse`, so it produces
/// no record at all.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
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
        /// itself precisely so a hook does not have to read the transcript,
        /// which is what lets the log hold the conversation without breaking
        /// the live-path rule.
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
