//! Whether conbus is above whatever a page calls back to.
//!
//! `no` needs positive evidence: a listener on a port of this machine, walked up
//! to something that vouches for itself, with no conbus on the way. `yes` needs a
//! conbus command above that listener and a page of a vendor conbus runs, because
//! an address can name any port. Anything less is `cannot tell`, and says what would
//! tell. Even a `no` covers the ancestry and nothing more, so it carries what a walk
//! cannot see.

use super::ancestry::{self, ConbusProcess, Holders, Listener, Stopped};
use super::url::Callback;

/// What was measured about the vendor programs conbus starts on its own, and
/// what was not. `conbus auth` prints it wherever a claim leans on it.
pub(super) const MEASURED: &str = "on Codex 0.166, `initialize`, `account/rateLimits/read` and \
`thread/list` spawned no `npx` or `uvx` in 8 s, and `claude -p` with no login exits 1 with `Not \
logged in` and no `open`. `thread/start` and `thread/resume` (a warm hop) and `thread/turns/list` \
(gap fill) are unmeasured";

const LIMIT: &str = "this walk says nothing about an app a hop opened with `open` or `osascript`: \
any app hangs from launchd, whoever opened it. Every conbus read starts `codex app-server`, and \
`conbus status` lists the hops";

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

/// The `process` lines and the verdict for a page's callbacks. `conbus_runs_it`:
/// the page belongs to a vendor conbus starts as its own child.
pub(super) fn describe(callbacks: &[Callback], conbus_runs_it: bool) -> Vec<String> {
    let looked: Vec<Looked> = callbacks.iter().map(look).collect();
    let mut lines = process_lines(&looked);
    lines.extend(verdict_lines(&looked, conbus_runs_it));
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
                    Some(Stopped::HeldByManager { .. }) => " (a manager, not a client)".to_string(),
                    Some(Stopped::Unvouched { pid, .. }) => {
                        format!(" (nothing says who started pid {pid})")
                    }
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

fn verdict_lines(looked: &[Looked], conbus_runs_it: bool) -> Vec<String> {
    let found: Vec<Above> = looked
        .iter()
        .flat_map(|looked| conbus_above(looked, conbus_runs_it))
        .collect();
    if let Some(yes) = found.iter().find_map(Above::yes) {
        return vec![yes];
    }
    if let Some(why) = found.first().and_then(Above::why_not_yes) {
        return vec![format!("conbus    cannot tell: {why}")];
    }
    if let Some(why) = why_it_cannot_tell(looked) {
        return vec![format!("conbus    cannot tell: {why}")];
    }
    vec![
        "conbus    no: every listener lsof sees on the callback's port was walked up to an app or a session, and none has conbus above it"
            .into(),
        format!("limit     {LIMIT}"),
        format!("measured  {MEASURED}"),
    ]
}

/// A listener with a process named conbus in its chain, and what that comes to.
struct Above {
    port: u16,
    pid: u32,
    kind: AboveKind,
}

enum AboveKind {
    /// The conbus command it names, over a page of a vendor conbus runs.
    Command(String),
    /// `conbus run`: the agent under it is the person's own.
    TheirAgent,
    /// The listener is conbus: `conbus hook --inference-listen` on a known port.
    ItsOwnListener,
    /// `ps` would not say which command the conbus process is.
    CommandUnread,
    /// conbus runs nothing that asks for this page.
    PageNotOurs,
}

fn conbus_above(looked: &Looked, conbus_runs_it: bool) -> Vec<Above> {
    let Looked::Port(port, Holders::Listeners(listeners)) = looked else {
        return Vec::new();
    };
    listeners
        .iter()
        .filter_map(|listener| above(*port, listener, conbus_runs_it))
        .collect()
}

fn above(port: u16, listener: &Listener, conbus_runs_it: bool) -> Option<Above> {
    let nearest = listener.conbus.first()?;
    let run = |process: &&ConbusProcess| process.command.as_deref() == Some("run");
    let unread = |process: &&ConbusProcess| process.command.is_none();
    let (pid, kind) = if nearest.pid == listener.pid {
        (nearest.pid, AboveKind::ItsOwnListener)
    } else if let Some(process) = listener.conbus.iter().find(run) {
        (process.pid, AboveKind::TheirAgent)
    } else if let Some(process) = listener.conbus.iter().find(unread) {
        (process.pid, AboveKind::CommandUnread)
    } else if !conbus_runs_it {
        (nearest.pid, AboveKind::PageNotOurs)
    } else {
        let command = nearest.command.clone().unwrap_or_default();
        (nearest.pid, AboveKind::Command(command))
    };
    Some(Above { port, pid, kind })
}

impl Above {
    fn yes(&self) -> Option<String> {
        let AboveKind::Command(command) = &self.kind else {
            return None;
        };
        Some(format!(
            "conbus    yes: `conbus {command}` (pid {}) is above what listens on port {}, and it runs the vendor this page belongs to",
            self.pid, self.port
        ))
    }

    fn why_not_yes(&self) -> Option<String> {
        let (port, pid) = (self.port, self.pid);
        match self.kind {
            AboveKind::Command(_) => None,
            AboveKind::TheirAgent => Some(format!(
                "what listens on port {port} runs under `conbus run` (pid {pid}), so it is your own agent, not conbus, and conbus does not know what it asked for"
            )),
            AboveKind::ItsOwnListener => Some(format!(
                "conbus itself listens on port {port} (pid {pid}), and it has no sign-in of its own to call back to. An address can name any port, so this one says nothing about who opened the page"
            )),
            AboveKind::CommandUnread => Some(format!(
                "a process named conbus (pid {pid}) is above what listens on port {port}, and `ps` would not say which command it is. Looking again while the page still waits would tell"
            )),
            AboveKind::PageNotOurs => Some(format!(
                "a conbus process (pid {pid}) is above what listens on port {port}, but conbus runs nothing that asks for this page. An address can name any port, so the port alone says nothing about who opened it; {HOPS}"
            )),
        }
    }
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
        Stopped::HeldByManager { pid, name } => Some(format!(
            "{name} (pid {pid}) holds the port itself, which says nothing about who asked; {HOPS}"
        )),
        Stopped::Unvouched { pid, name, over } => Some(format!(
            "{name} (pid {pid}) hangs directly from {over}, so nothing above it says who started it: {over} may run it as a service, or what started it may have exited. `ps -o lstart= -p {pid}` says when it started, to hold against the hops `conbus status` lists; every conbus read starts `codex app-server`"
        )),
    }
}
