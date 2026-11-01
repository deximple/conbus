//! Whether conbus is above whatever a page calls back to.
//!
//! `no` needs positive evidence: a listener on a port of this machine, walked
//! to the root, with no conbus on the way. Anything less is `cannot tell`, and
//! says what would tell. Even a `no` covers the ancestry and nothing more, so it
//! carries what a walk cannot see.

use super::ancestry::{self, Holders, Listener, Stopped};
use super::url::Callback;

/// What was measured about the vendor programs conbus starts on its own, and
/// what was not. `conbus auth` prints it wherever a claim leans on it.
pub(super) const MEASURED: &str = "on Codex 0.166, `initialize`, `account/rateLimits/read` and \
`thread/list` spawned no `npx` or `uvx` in 8 s, and `claude -p` with no login exits 1 with `Not \
logged in` and no `open`. `thread/start` and `thread/resume` (a warm hop) and `thread/turns/list` \
(gap fill) are unmeasured";

const LIMIT: &str = "this walk cannot see a server that outlived what started it and leads its own \
process group, or an app a hop opened with `open` or `osascript`: both hang from the root. Every \
conbus read starts `codex app-server`, and `conbus status` lists the hops";

const HOPS: &str = "`conbus status` lists the hops, which can open an app";

/// The looking-up of one callback.
enum Looked {
    Port(u16, Holders),
    NoPort,
    Remote,
}

fn look(callback: &Callback) -> Looked {
    match callback {
        Callback::Local(port) => Looked::Port(*port, ancestry::holders(*port)),
        Callback::LocalNoPort => Looked::NoPort,
        Callback::Remote => Looked::Remote,
    }
}

/// The `process` lines and the verdict for a page's callbacks.
pub(super) fn describe(callbacks: &[Callback]) -> Vec<String> {
    let looked: Vec<Looked> = callbacks.iter().map(look).collect();
    let mut lines = process_lines(&looked);
    lines.extend(verdict_lines(&looked));
    lines
}

fn process_lines(looked: &[Looked]) -> Vec<String> {
    if looked.is_empty() {
        return vec!["process   the URL names no callback to look up".into()];
    }
    looked.iter().flat_map(process_line).collect()
}

fn process_line(looked: &Looked) -> Vec<String> {
    match looked {
        Looked::Remote => vec![
            "process   its callback is on another host, so this machine's processes say nothing about it"
                .into(),
        ],
        Looked::NoPort => vec!["process   its callback names this machine but no port".into()],
        Looked::Port(port, Holders::Nothing) => {
            vec![format!("process   lsof sees nothing listening on port {port}")]
        }
        Looked::Port(port, Holders::CouldNotAsk(why)) => {
            vec![format!("process   could not look up port {port}: {why}")]
        }
        Looked::Port(port, Holders::Listeners(listeners)) => listeners
            .iter()
            .map(|listener| {
                let stopped = match &listener.stopped {
                    Some(Stopped::Lost(why)) => format!(" (stopped: {why})"),
                    Some(Stopped::Orphan(pid)) => format!(" (pid {pid} lost its parent)"),
                    None => String::new(),
                };
                format!(
                    "process   port {port}: pid {}: {}{stopped}",
                    listener.pid,
                    listener.chain.join(" > ")
                )
            })
            .collect(),
    }
}

fn verdict_lines(looked: &[Looked]) -> Vec<String> {
    if let Some((port, pid)) = looked.iter().find_map(under_conbus) {
        return vec![format!(
            "conbus    yes: a process named conbus is in the ancestry of pid {pid}, which listens on port {port}"
        )];
    }
    if let Some(why) = why_it_cannot_tell(looked) {
        return vec![format!("conbus    cannot tell: {why}")];
    }
    vec![
        "conbus    no: every listener lsof sees on the callback's port was walked to the root, and none has conbus above it"
            .into(),
        format!("limit     {LIMIT}"),
        format!("measured  {MEASURED}"),
    ]
}

fn under_conbus(looked: &Looked) -> Option<(u16, u32)> {
    let Looked::Port(port, Holders::Listeners(listeners)) = looked else {
        return None;
    };
    listeners
        .iter()
        .find(|listener| listener.under_conbus())
        .map(|listener| (*port, listener.pid))
}

/// The first thing that keeps this from being a `no`.
fn why_it_cannot_tell(looked: &[Looked]) -> Option<String> {
    if looked.is_empty() {
        return Some(format!(
            "the URL names no callback, so there is no process to walk. A local port in its `redirect_uri`, `redirect_url`, `callback` or `mcpRelayUrl` would tell; {HOPS}"
        ));
    }
    looked.iter().find_map(|looked| match looked {
        Looked::Remote => Some(format!(
            "its callback is on another host, so no process here can be walked. Only that host would tell; {HOPS}"
        )),
        Looked::NoPort => Some(format!(
            "its callback names no port, so no process can be picked out. A callback with a port would tell; {HOPS}"
        )),
        Looked::Port(port, Holders::Nothing) => Some(format!(
            "nothing lsof sees listens on port {port}, so there is no process to walk. Looking again while the page still waits would tell; lsof does not show another account's processes"
        )),
        Looked::Port(port, Holders::CouldNotAsk(why)) => Some(format!(
            "port {port} could not be looked up ({why}). `lsof` and `ps` on PATH would tell"
        )),
        Looked::Port(_, Holders::Listeners(listeners)) => {
            listeners.iter().find_map(stopped_short)
        }
    })
}

fn stopped_short(listener: &Listener) -> Option<String> {
    match listener.stopped.as_ref()? {
        Stopped::Lost(why) => Some(format!(
            "the walk from pid {} ended before the root: {why}. Looking again while the page still waits would tell",
            listener.pid
        )),
        Stopped::Orphan(pid) => Some(format!(
            "pid {pid} hangs from the root without leading its own process group, so what started it has exited and no walk can reach it. `ps -o lstart= -p {pid}` says when it started, to hold against the hops `conbus status` lists; every conbus read starts `codex app-server`"
        )),
    }
}
