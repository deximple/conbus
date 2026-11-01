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
    /// This pid hangs from the root and does not lead its own process group, so
    /// what started it has exited. launchd and systemd start group leaders, and a
    /// process keeps the group it was started in when its parent goes.
    Orphan(u32),
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
}

/// From `pid` to the root. Nothing limits the depth; a pid seen twice ends the
/// walk, so `ps` answering in a loop cannot hold it.
fn walk(pid: u32) -> Listener {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut at = pid;
    let mut below_root = None;
    let stopped = loop {
        if !seen.insert(at) {
            break Some(Stopped::Lost(format!("pid {at} is its own ancestor")));
        }
        match process(at) {
            Err(why) => break Some(Stopped::Lost(why)),
            Ok(process) => {
                chain.push(process.name);
                if process.parent == 0 {
                    break below_root
                        .filter(|(child, group)| child != group)
                        .map(|(child, _)| Stopped::Orphan(child));
                }
                below_root = Some((at, process.group));
                at = process.parent;
            }
        }
    };
    Listener {
        pid,
        chain,
        stopped,
    }
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
                name: "Some App".to_string()
            })
        );
        assert_eq!(
            parse_ps_line("0 1 launchd"),
            Some(Process {
                parent: 0,
                group: 1,
                name: "launchd".to_string()
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
