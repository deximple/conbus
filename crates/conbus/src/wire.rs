//! The newline-delimited JSON protocol between `conbus hook` and the daemon.

use conbus_core::event::{Event, Harness};
use conbus_core::ids::BusId;
use serde::{Deserialize, Serialize};

/// One observed vendor event, on its way to the log.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Request {
    pub harness: Harness,
    pub vendor_session_id: String,
    pub event: Event,
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
    Failed {
        error: String,
    },
}
