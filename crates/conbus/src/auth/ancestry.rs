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
    /// Why the walk ended before the root, when it did.
    pub stopped: Option<String>,
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
        Ok(pids) => {
            let unique: HashSet<u32> = pids.iter().copied().collect();
            let mut unique: Vec<u32> = unique.into_iter().collect();
            unique.sort_unstable();
            Holders::Listeners(unique.into_iter().map(walk).collect())
        }
    }
}

/// From `pid` to the root. Nothing limits the depth; a pid seen twice ends the
/// walk, so `ps` answering in a loop cannot hold it.
fn walk(pid: u32) -> Listener {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut at = pid;
    let stopped = loop {
        if !seen.insert(at) {
            break Some(format!("pid {at} is its own ancestor"));
        }
        match process(at) {
            Err(why) => break Some(why),
            Ok((parent, name)) => {
                chain.push(name);
                if parent == 0 {
                    break None;
                }
                at = parent;
            }
        }
    };
    Listener {
        pid,
        chain,
        stopped,
    }
}

/// The parent and the shown name of `pid`, or why they are not known.
fn process(pid: u32) -> Result<(u32, String), String> {
    let mut ps = Command::new("ps");
    ps.args(["-o", "ppid=,comm=", "-p", &pid.to_string()]);
    let output = crate::code::output_within(ps, ASK_WITHIN)?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_ps_line(text.trim()).ok_or_else(|| format!("`ps` no longer finds pid {pid}"))
}

/// `  501 /Applications/Some App.app/Contents/MacOS/Some App`.
fn parse_ps_line(line: &str) -> Option<(u32, String)> {
    let (parent, comm) = line.split_once(char::is_whitespace)?;
    let comm = comm.trim();
    let name = comm.rsplit('/').next().filter(|name| !name.is_empty())?;
    Some((parent.parse().ok()?, super::shown(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ps_line_gives_the_parent_and_the_binary_name() {
        assert_eq!(
            parse_ps_line("501 /Applications/Some App.app/Contents/MacOS/Some App"),
            Some((501, "Some App".to_string()))
        );
        assert_eq!(parse_ps_line("0 launchd"), Some((0, "launchd".to_string())));
        assert_eq!(parse_ps_line("7"), None);
        assert_eq!(parse_ps_line("x name"), None);
        assert_eq!(parse_ps_line("7 /"), None);
        assert_eq!(parse_ps_line(""), None);
    }

    #[test]
    fn a_name_cannot_write_to_the_terminal() {
        assert_eq!(
            parse_ps_line("7 /tmp/a\u{1b}[2Kb"),
            Some((7, "a?[2Kb".to_string()))
        );
    }
}
