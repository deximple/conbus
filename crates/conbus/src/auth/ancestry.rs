//! Who holds a port: `lsof` for the listeners, `ps` for what stands above each.
//!
//! Names only. A process's arguments can hold anything, so they are never asked
//! for, and a directory is dropped from a name because it can hold a person's
//! home.

use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

/// A listener's whole ancestry is a handful of these, each a few milliseconds;
/// the limit is for a machine where they wedge.
const ASK_WITHIN: Duration = Duration::from_secs(10);

/// One process that listens on the port, and everything above it.
pub(super) struct Listener {
    pub pid: u32,
    /// Process names, from the listener up.
    pub chain: Vec<String>,
    /// Why the walk did not reach the root with the ancestry whole, when it did not.
    pub stopped: Option<Stopped>,
}

/// Why a walk cannot say that nothing above a listener is conbus.
pub(super) enum Stopped {
    /// `ps` could not say, or said something impossible.
    Lost(String),
    /// A root or a service manager holds the port itself.
    HeldByManager { pid: u32, name: String },
    /// The last process before a root or a service manager is nothing that vouches
    /// for itself, so what started it may be gone. A process whose parent exits is
    /// handed to one of those, and it may lead its own group: Codex starts its MCP
    /// servers that way.
    Unvouched {
        pid: u32,
        name: String,
        over: String,
    },
}

impl Listener {
    pub fn under_conbus(&self) -> bool {
        self.chain.iter().any(|name| is_conbus(name))
    }
}

/// What `lsof` said about a port.
pub(super) enum Holders {
    Listeners(Vec<Listener>),
    Nothing,
    CouldNotAsk(String),
}

/// What adopts a process whose parent has gone, besides the root itself. `systemd`
/// is the user manager too: the recorder runs under one on Linux.
const MANAGERS: &[&str] = &["launchd", "init", "systemd"];

/// What a person's session hangs from, on a machine with no app bundles to go by.
/// conbus starts none of these, and anything else at the top of a chain reads
/// `cannot tell`, so this list can only be too short in the safe direction.
const SESSION_HOSTS: &[&str] = &[
    "sshd",
    "login",
    "tmux",
    "tmux: server",
    "screen",
    "gnome-terminal-",
    "konsole",
    "xterm",
];

/// An app's own main binary. A tool kept in the bundle's resources is not the app,
/// and Python's framework build runs every script from a bundle of its own.
const APP_MAIN_BINARY: &str = ".app/Contents/MacOS/";
const NOT_AN_APP: &[&str] = &["Python"];

/// A binary called conbus, or one of its staged and renamed builds. Only the
/// binary's own name counts: a directory named conbus does not make a page ours.
fn is_conbus(name: &str) -> bool {
    name == "conbus" || name.starts_with("conbus.") || name.starts_with("conbus-")
}

/// Every listener `lsof` sees on `port`, each walked up. It does not show
/// another account's processes.
pub(super) fn holders(port: u16) -> Holders {
    let mut lsof = Command::new("lsof");
    lsof.args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"]);
    let output = match crate::code::output_within(lsof, ASK_WITHIN) {
        Ok(output) => output,
        Err(why) => return Holders::CouldNotAsk(why),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let pids: Result<Vec<u32>, _> = text
        .lines()
        .map(|line| line.trim().parse::<u32>())
        .collect();
    match pids {
        Err(_) => Holders::CouldNotAsk("lsof printed something that is not a pid".into()),
        Ok(pids) if pids.is_empty() => Holders::Nothing,
        Ok(mut pids) => {
            pids.sort_unstable();
            pids.dedup();
            Holders::Listeners(pids.into_iter().map(walk).collect())
        }
    }
}

/// One line of what `ps` says.
#[derive(Debug, PartialEq, Eq)]
struct Process {
    parent: u32,
    group: u32,
    name: String,
    /// Run from an app bundle's main binary.
    app: bool,
}

impl Process {
    fn adopts(&self) -> bool {
        self.parent == 0 || MANAGERS.contains(&self.name.as_str())
    }

    /// Something conbus does not start, so a chain that ends here has a real top.
    /// It must lead its own group as well: what launchd and systemd start does, and
    /// a process keeps the group it was started in when its parent goes.
    fn vouches_for_itself(&self, pid: u32) -> bool {
        self.group == pid
            && ((self.app && !NOT_AN_APP.contains(&self.name.as_str()))
                || SESSION_HOSTS.contains(&self.name.as_str()))
    }
}

/// From `pid` to the root. Nothing limits the depth; a pid seen twice ends the
/// walk, so `ps` answering in a loop cannot hold it.
fn walk(pid: u32) -> Listener {
    let mut steps = Vec::new();
    let mut seen = HashSet::new();
    let mut at = pid;
    let lost = loop {
        if !seen.insert(at) {
            break Some(format!("pid {at} is its own ancestor"));
        }
        match process(at) {
            Err(why) => break Some(why),
            Ok(process) => {
                let parent = process.parent;
                steps.push((at, process));
                if parent == 0 {
                    break None;
                }
                at = parent;
            }
        }
    };
    let stopped = match lost {
        Some(why) => Some(Stopped::Lost(why)),
        None => top_of_the_chain(&steps),
    };
    Listener {
        pid,
        chain: steps.into_iter().map(|(_, process)| process.name).collect(),
        stopped,
    }
}

/// `no` needs a chain that ends in something that vouches for itself. What hangs
/// straight from a root or a service manager may be a service or may be an orphan,
/// and from here the two look the same.
fn top_of_the_chain(steps: &[(u32, Process)]) -> Option<Stopped> {
    let manager = steps.iter().position(|(_, process)| process.adopts())?;
    let Some((pid, top)) = manager.checked_sub(1).and_then(|top| steps.get(top)) else {
        let (pid, holder) = &steps[manager];
        return Some(Stopped::HeldByManager {
            pid: *pid,
            name: holder.name.clone(),
        });
    };
    (manager == 1 || !top.vouches_for_itself(*pid)).then(|| Stopped::Unvouched {
        pid: *pid,
        name: top.name.clone(),
        over: steps[manager].1.name.clone(),
    })
}

/// What `ps` says of `pid`, or why it says nothing.
fn process(pid: u32) -> Result<Process, String> {
    let mut ps = Command::new("ps");
    ps.args(["-o", "ppid=,pgid=,comm=", "-p", &pid.to_string()]);
    let output = crate::code::output_within(ps, ASK_WITHIN)?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_ps_line(text.trim()).ok_or_else(|| format!("`ps` no longer finds pid {pid}"))
}

/// `  501   501 /Applications/Some App.app/Contents/MacOS/Some App`.
fn parse_ps_line(line: &str) -> Option<Process> {
    let (parent, rest) = line.split_once(char::is_whitespace)?;
    let (group, comm) = rest.trim_start().split_once(char::is_whitespace)?;
    let comm = comm.trim();
    let name = comm.rsplit('/').next().filter(|name| !name.is_empty())?;
    Some(Process {
        parent: parent.parse().ok()?,
        group: group.parse().ok()?,
        name: super::shown(name),
        app: comm.contains(APP_MAIN_BINARY),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ps_line_gives_the_parent_the_group_and_the_binary_name() {
        assert_eq!(
            parse_ps_line("501  77 /Applications/Some App.app/Contents/MacOS/Some App"),
            Some(Process {
                parent: 501,
                group: 77,
                name: "Some App".to_string(),
                app: true,
            })
        );
        assert_eq!(
            parse_ps_line("0 1 launchd"),
            Some(Process {
                parent: 0,
                group: 1,
                name: "launchd".to_string(),
                app: false,
            })
        );
        assert_eq!(parse_ps_line("7"), None);
        assert_eq!(parse_ps_line("7 8"), None);
        assert_eq!(parse_ps_line("x 8 name"), None);
        assert_eq!(parse_ps_line("7 x name"), None);
        assert_eq!(parse_ps_line("7 8 /"), None);
        assert_eq!(parse_ps_line(""), None);
    }

    #[test]
    fn a_name_cannot_write_to_the_terminal() {
        assert_eq!(
            parse_ps_line("7 8 /tmp/a\u{1b}[2Kb").map(|process| process.name),
            Some("a?[2Kb".to_string())
        );
    }
}
