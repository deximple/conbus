//! The newline-delimited JSON protocol between `conbus hook` and the daemon.

use conbus_core::event::{Entrypoint, Event, Harness, VendorAgentId, VendorTurnId};
use conbus_core::ids::BusId;
use serde::{Deserialize, Serialize};

/// The shape this build speaks over the socket.
///
/// It exists because the hook and the daemon are the same binary but not the
/// same *process*: upgrading `conbus` leaves the old daemon running — `bind`
/// deliberately refuses to displace it — so a newer hook talks to an older
/// daemon until someone restarts it. Without a version, an envelope field the
/// old daemon does not know is dropped in silence, which is the failure mode
/// this project keeps finding. With one it is a refusal the hook can report.
///
/// It protects upgrades from v1 onward, and not this one: a daemon built before
/// this field existed ignores it, so the first deploy still needs the daemon
/// restarted by hand. `the_wire_shape_is_pinned_to_the_protocol_version` below
/// is what stops the constant falling behind the struct.
pub const PROTOCOL_VERSION: u32 = 6;

/// One observed vendor event, on its way to the log.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Request {
    /// Absent means a hook older than this field, which is by definition no
    /// newer than the daemon reading it.
    #[serde(default, rename = "pv", skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    pub harness: Harness,
    /// The vendor's own version, when it tells us. Absent is the normal case
    /// today: neither Claude Code nor Codex puts its version in a hook payload
    /// or in the hook child's environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_version: Option<String>,
    pub vendor_session_id: String,
    /// Optional so a newer hook and an older daemon still understand each other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_turn_id: Option<VendorTurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_agent_id: Option<VendorAgentId>,
    /// The checkout the harness reported. Both vendors put it on the common
    /// base of every payload; it is the key a second harness needs to find the
    /// conversation it is continuing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The front end the session runs under, from the hook child's own
    /// environment rather than from the payload, which carries no origin at
    /// all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Entrypoint>,
    /// Whether this session may be briefed. The hook reads the person's
    /// `CONBUS_BRIEFING=off` from its own environment and carries it here so
    /// the daemon neither injects nor records a briefing — off means no
    /// briefing, not no recording. Absent means yes, which is what every hook
    /// before this field meant.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub brief: bool,
    /// What the run declares about itself, for the same reason `brief` rides
    /// here: the daemon writes the `routed` line and the daemon does not run
    /// in the probe's environment. Absent is not a claim of anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<conbus_core::event::HopProvenance>,
    /// The Claude Code profile the hook child runs under, from its own
    /// `CLAUDE_CONFIG_DIR` — the payload does not carry it either. Claude
    /// Code only, and absent on the default profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The hook's own id for this request — a UUIDv7, so a name sorts in
    /// time order. The daemon remembers the last few thousand and answers a
    /// re-sent request Recorded rather than writing it twice; the spool file
    /// is named for it. Absent on hooks older than the spool.
    #[serde(default, rename = "hid", skip_serializing_if = "Option::is_none")]
    pub hook_id: Option<String>,
    /// When the hook first tried to send this — set on a spooled file so a
    /// drained request keeps its own time rather than the daemon's.
    #[serde(default, rename = "spooled", skip_serializing_if = "Option::is_none")]
    pub spooled_at: Option<String>,
    pub event: Event,
}

fn yes() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_yes(brief: &bool) -> bool {
    *brief
}

/// What the daemon did with it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    /// Written under this conversation, at these sequence numbers. More than
    /// one when a remembered session start was flushed behind the prompt that
    /// proved the session real.
    Recorded {
        bus_id: BusId,
        seqs: Vec<u64>,
    },
    /// Held as a session candidate. Nothing was written, and that is correct:
    /// this session may still turn out to be a ghost.
    Deferred,
    /// Belongs to no conversation and cannot start one.
    Orphaned,
    /// Retention withheld it from disk.
    Withheld,
    /// Written, and this session should be told what has been happening in its
    /// checkout.
    ///
    /// The text is already budgeted to the vendor's cap by the daemon, which is
    /// where the log lives; the hook only frames and prints it.
    Briefed {
        bus_id: BusId,
        seqs: Vec<u64>,
        /// Goes to the model, inside the vendor's context channel.
        context: String,
        /// Goes to the person, and costs no model tokens.
        message: String,
    },
    Failed {
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::event::{Body, Event};

    #[test]
    fn the_wire_shape_is_pinned_to_the_protocol_version() {
        // If this fails because you added or renamed a field on `Request`,
        // bump `PROTOCOL_VERSION` and update the list. The version is only a
        // guard if it moves when the shape does, and nothing else makes it.
        let request = Request {
            protocol_version: Some(PROTOCOL_VERSION),
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: Some("2.1.241".into()),
            vendor_session_id: "s".into(),
            vendor_turn_id: Some(conbus_core::event::VendorTurnId::new("T")),
            vendor_agent_id: Some(conbus_core::event::VendorAgentId::new("A")),
            workspace: Some("/repo".into()),
            entrypoint: Some(Entrypoint::new(Entrypoint::CLI)),
            // Off, so the field is on the wire: it is skipped when it says
            // yes, which is what every hook before it meant.
            brief: false,
            provenance: None,
            profile: Some("work".into()),
            hook_id: Some("01a19463-cbd4-7b50-b819-1c9ecf85ef18".into()),
            spooled_at: Some("2026-11-01T00:00:00.000Z".into()),
            event: Event::TurnStarted {
                author: None,
                prompt: Body::kept("p"),
            },
        };

        let value: serde_json::Value = serde_json::to_value(&request).expect("serialise");
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();

        assert_eq!(
            keys,
            vec![
                "brief",
                "entrypoint",
                "event",
                "harness",
                "harness_version",
                "hid",
                "profile",
                "pv",
                "spooled",
                "vendor_agent_id",
                "vendor_session_id",
                "vendor_turn_id",
                "workspace",
            ],
            "the wire shape changed; bump PROTOCOL_VERSION (now {PROTOCOL_VERSION})"
        );
        assert_eq!(PROTOCOL_VERSION, 6);
    }
}
