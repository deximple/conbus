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
    /// The user submitted a prompt. This is where a `bus_id` is minted.
    TurnStarted { prompt: Body },
    /// A tool ran.
    ToolUse {
        tool: String,
        input: Body,
        output: Body,
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
