//! The thin client the vendor CLI actually runs.
//!
//! Two rules govern everything here. It must never fail the turn: a non-zero
//! exit from `UserPromptSubmit` blocks the user's prompt, and losing a log line
//! is cheaper than losing the turn. And it must never mirror our own background
//! sessions, because the hooks are global and the summariser we spawn would
//! otherwise log itself.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use conbus_core::event::Harness;
use conbus_core::paths::Paths;

use crate::hook::{self, Normalized};
use crate::wire::{Request, Response};

/// What the hook says on stdout when it has nothing to add.
///
/// The recorder never injects, so this is the whole of its output on every
/// path, success or failure. It is JSON because the vendor's hook contract
/// reads stdout as JSON control output; the two fields say "carry on" and
/// "keep this out of the transcript".
///
/// It exists because writing *nothing* is not the same as saying nothing. A
/// zero-byte stdout on a context-injecting event was observed to reach the API
/// as an empty text block carrying `cache_control`, which the API rejects —
/// taking the user's session down with it. See the quirk note in HANDOFF.md.
pub const SILENT_OK: &str = r#"{"continue":true,"suppressOutput":true}"#;

/// The environment marker that tells a hook it is inside a session we started.
pub const INTERNAL_MARKER: &str = "CONBUS_INTERNAL_SESSION";

/// The vendor's version, if it exposes one to its hook children.
///
/// `CLAUDE_CODE_VERSION` exists as a name inside the Claude Code binary but is
/// not exported to a hook child on 2.1.241, and Codex has no equivalent at all,
/// so this is `None` in practice today. It is read anyway: the moment a vendor
/// starts setting it the log gains a version stamp with no further work, and
/// reading an absent variable costs nothing.
fn vendor_version(harness: &str) -> Option<String> {
    let name = match harness {
        Harness::CLAUDE_CODE => "CLAUDE_CODE_VERSION",
        Harness::CODEX => "CODEX_VERSION",
        _ => return None,
    };
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Mirror one hook payload. Returns a diagnostic when the record did not land;
/// the caller reports it and still exits zero.
pub fn run(harness: &str, stdin: &mut impl Read) -> Result<(), String> {
    if std::env::var_os(INTERNAL_MARKER).is_some_and(|value| !value.is_empty()) {
        return Ok(());
    }

    let mut payload = String::new();
    stdin
        .read_to_string(&mut payload)
        .map_err(|err| format!("could not read the hook payload: {err}"))?;

    let normalized = hook::normalize(&Harness::new(harness), &payload).map_err(|err| err.0)?;
    let key;
    let observed;
    match normalized {
        Normalized::Event {
            key: k,
            observed: o,
        } => {
            key = k;
            observed = *o;
        }
        // A decision, so nothing to say.
        Normalized::Ignored { .. } => return Ok(()),
        // A surprise. The vendor grew an event this adapter has never seen, and
        // it is being dropped — which is the right thing to do and the wrong
        // thing to do quietly.
        Normalized::Unknown { hook_event_name } => {
            return Err(format!("no mapping for hook event {hook_event_name:?}"));
        }
    }
    let paths = Paths::resolve().map_err(|_| "no home directory to resolve".to_string())?;
    let socket = paths.socket();
    let stream = UnixStream::connect(&socket)
        .map_err(|err| format!("no daemon on {}: {err}", socket.display()))?;

    let request = Request {
        protocol_version: Some(crate::wire::PROTOCOL_VERSION),
        harness: key.harness,
        harness_version: vendor_version(harness),
        vendor_session_id: key.vendor_session_id,
        vendor_turn_id: observed.vendor_turn_id,
        vendor_agent_id: observed.vendor_agent_id,
        event: observed.event,
    };
    send(stream, &request)
}

fn send(mut stream: UnixStream, request: &Request) -> Result<(), String> {
    let line = serde_json::to_string(request).map_err(|err| err.to_string())?;
    writeln!(stream, "{line}").map_err(|err| format!("could not send the record: {err}"))?;
    stream
        .flush()
        .map_err(|err| format!("could not send the record: {err}"))?;

    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|err| format!("no reply from the daemon: {err}"))?;

    match serde_json::from_str::<Response>(reply.trim()) {
        Ok(Response::Recorded { .. } | Response::Deferred) => Ok(()),
        Ok(Response::Failed { error }) => Err(format!("the daemon refused the record: {error}")),
        // Not written, and the caller has to hear about it. An orphan is the
        // normal shape of a daemon restart mid-turn: the session is unbound, so
        // the rest of that turn belongs to no conversation. Reporting it is the
        // difference between a known gap and a turn that quietly disappears.
        Ok(Response::Orphaned) => Err(
            "no conversation for this session; the daemon has no binding for it              (a restart mid-turn looks exactly like this)"
                .to_string(),
        ),
        Ok(Response::Withheld) => {
            Err("retention withheld this record from disk".to_string())
        }
        Err(err) => Err(format!("unreadable reply {reply:?}: {err}")),
    }
}
