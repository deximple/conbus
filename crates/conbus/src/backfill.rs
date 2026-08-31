//! `conbus backfill` — recovering the conversations that happened before the
//! recorder was installed.
//!
//! The corpus is the one asset that cannot be reconstructed later, and the
//! clock only starts when the recorder is running. Backfill is how the months
//! already on disk join it.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use conbus_core::backfill::{CLAUDE_TRANSCRIPT, parse_claude_transcript};
use conbus_core::event::Event;
use conbus_core::ids::BusId;
use conbus_core::paths::Paths;
use conbus_core::retention::Retention;
use conbus_core::segment::SegmentLog;

/// Recover Claude Code history into the log.
pub fn run(paths: &Paths, apply: bool) -> io::Result<()> {
    // Backfill appends to the segments directly, so it must not race the daemon
    // for the same file. Refusing is the honest answer; interleaved writes would
    // corrupt the one thing the whole design calls the source of truth.
    if std::os::unix::net::UnixStream::connect(paths.socket()).is_ok() {
        return Err(io::Error::other(format!(
            "a daemon is listening on {}; stop it before backfilling, because both would \
             append to the same segment",
            paths.socket().display()
        )));
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("no HOME to read history from"))?;
    let transcripts = home.join(".claude").join("projects");

    let already = already_backfilled(paths)?;
    let mut log = SegmentLog::open(paths.log_dir(), Retention::default())?;

    let mut sessions = 0u64;
    let mut skipped_sessions = 0u64;
    let mut completed_sessions = 0u64;
    let mut turns = 0u64;
    let mut records = 0u64;
    let mut unread: BTreeMap<String, u64> = BTreeMap::new();

    for file in session_files(&transcripts)? {
        // The file's own name is the vendor session id, and it is a UUID — so
        // it can *be* the `bus_id`. That makes a second run produce identical
        // ids for the same conversations rather than a second copy of history.
        let Some(bus_id) = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<BusId>().ok())
        else {
            *unread
                .entry("file name is not a session id".into())
                .or_default() += 1;
            continue;
        };
        // Already complete: it is in the log and it knows where it happened.
        if already.get(&bus_id) == Some(&true) {
            skipped_sessions += 1;
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            *unread.entry("unreadable file".into()).or_default() += 1;
            continue;
        };

        let mut recovered = parse_claude_transcript(&text, bus_id);
        for (reason, count) in &recovered.skipped {
            *unread.entry(reason.clone()).or_default() += count;
        }
        if recovered.entries.is_empty() {
            continue;
        }
        // Already in the log, but recovered by a build that did not record the
        // workspace. Append the session record alone: the turns are already
        // there, and appending them again would put the conversation in the log
        // twice, which an append-only log cannot take back.
        let completing = already.contains_key(&bus_id);
        if completing {
            recovered
                .entries
                .retain(|entry| matches!(entry.event, Event::SessionCandidate { .. }));
            if recovered.entries.is_empty() || recovered.workspace.is_none() {
                skipped_sessions += 1;
                continue;
            }
            // Counted apart from `sessions`: nothing was recovered here, an
            // existing conversation only learned where it happened, and
            // reporting the two as one would overstate what a re-run found.
            completed_sessions += 1;
        } else {
            sessions += 1;
        }
        turns += recovered
            .entries
            .iter()
            .filter(|e| matches!(e.event, conbus_core::event::Event::TurnStarted { .. }))
            .count() as u64;
        records += recovered.entries.len() as u64;

        if apply {
            for entry in recovered.entries {
                log.append(entry)?;
            }
        }
    }

    println!("history   {}", transcripts.display());
    println!(
        "recovered {sessions} conversation(s), {turns} turn(s), {records} record(s)\
         {}",
        if skipped_sessions > 0 {
            format!("; {skipped_sessions} already backfilled")
        } else {
            String::new()
        }
    );
    if completed_sessions > 0 {
        println!(
            "completed {completed_sessions} conversation(s) that were recovered before the \
             workspace was recorded"
        );
    }
    println!("not read  {}", summarise_unread(&unread));
    if apply {
        println!("written to {}", paths.log_dir().display());
    } else {
        println!("\nNothing was written. Re-run with --apply to record it.");
    }
    Ok(())
}

/// Conversations already recovered, and whether each one knows its workspace.
///
/// Two answers rather than one because a second run has two jobs. It must not
/// double a conversation it already has — that is what the set was for — and it
/// must be able to finish a conversation recovered by a build that did not
/// record the workspace, which is every conversation on disk before this one.
fn already_backfilled(paths: &Paths) -> io::Result<HashMap<BusId, bool>> {
    let mut known: HashMap<BusId, bool> = HashMap::new();
    for record in SegmentLog::scan(paths.log_dir())?.records {
        if record.source.backfilled_from.as_deref() != Some(CLAUDE_TRANSCRIPT) {
            continue;
        }
        let has_workspace = record.source.workspace.is_some();
        known
            .entry(record.bus_id)
            .and_modify(|seen| *seen |= has_workspace)
            .or_insert(has_workspace);
    }
    Ok(known)
}

/// Top-level session files only.
///
/// The nested `subagents/` and `subagents/workflows/` trees are deliberately
/// left out: those files are named for an agent rather than a session, so they
/// have no conversation identity to attach to, and folding them into the parent
/// would interleave several agents' work under one conversation.
fn session_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let projects = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(files),
        Err(err) => return Err(err),
    };
    for project in projects.filter_map(Result::ok) {
        let Ok(sessions) = fs::read_dir(project.path()) else {
            continue;
        };
        for session in sessions.filter_map(Result::ok) {
            let path = session.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn summarise_unread(unread: &BTreeMap<String, u64>) -> String {
    if unread.is_empty() {
        return "nothing".to_string();
    }
    let mut by_count: Vec<(&String, &u64)> = unread.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1));
    by_count
        .iter()
        .take(6)
        .map(|(reason, count)| format!("{count} {reason}"))
        .collect::<Vec<_>>()
        .join(", ")
}
