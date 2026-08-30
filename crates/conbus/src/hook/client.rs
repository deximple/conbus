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

use crate::hook::claude_code::{self, Normalized};
use crate::wire::{Request, Response};

/// The environment marker that tells a hook it is inside a session we started.
pub const INTERNAL_MARKER: &str = "CONBUS_INTERNAL_SESSION";

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

    let normalized = claude_code::normalize(&payload).map_err(|err| err.0)?;
    let Normalized::Event { key, observed } = normalized else {
        return Ok(());
    };

    let paths = Paths::resolve().map_err(|_| "no home directory to resolve".to_string())?;
    let socket = paths.socket();
    let stream = UnixStream::connect(&socket)
        .map_err(|err| format!("no daemon on {}: {err}", socket.display()))?;

    let request = Request {
        harness: Harness::new(harness),
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
