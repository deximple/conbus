//! `conbus backfill` — recovering the conversations that happened before the
//! recorder was installed.
//!
//! The corpus is the one asset that cannot be reconstructed later, and the
//! clock only starts when the recorder is running. Backfill is how the months
//! already on disk join it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use conbus_core::backfill::{
    CLAUDE_TRANSCRIPT, CODEX_THREAD, Recovered, parse_claude_transcript, parse_codex_thread,
};

use crate::appserver::AppServer;
use conbus_core::event::{Body, Event, Harness};
use conbus_core::ids::BusId;
use conbus_core::paths::Paths;
use conbus_core::record::{Entry, Record};
use conbus_core::retention::Retention;
use conbus_core::segment::SegmentLog;

/// What one run found, whether or not it wrote any of it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Outcome {
    /// Conversations recovered whole.
    pub sessions: u64,
    pub turns: u64,
    pub records: u64,
    /// In the log from an earlier run already, and complete.
    pub already_backfilled: u64,
    /// In the log from an earlier run, and given the session stamp that run
    /// did not record.
    pub completed: u64,
    /// In the log because the recorder saw them as they happened. Left alone:
    /// the live records are the stronger fact, and a copy would be a second
    /// conversation for the same session.
    pub recorded_live: u64,
    /// Lines and files not read, counted by why.
    pub unread: BTreeMap<String, u64>,
    /// Codex threads recovered whole through the app-server.
    pub codex_threads: u64,
    pub codex_turns: u64,
    pub codex_records: u64,
    /// Codex threads already in the log from an earlier run.
    pub codex_already: u64,
    /// Codex threads the recorder saw as they happened, left alone.
    pub codex_live: u64,
    /// Why the Codex source could not be read at all, when it could not.
    pub codex_unavailable: Option<String>,
    /// Live conversations that had at least one turn the official source
    /// held and the log did not.
    pub gap_sessions: u64,
    pub gap_turns: u64,
    pub gap_records: u64,
}

/// How long the app-server gets for the whole enumeration. Hundreds of
/// threads at two calls each ran in well under a minute on this machine.
const CODEX_DEADLINE: std::time::Duration = std::time::Duration::from_secs(600);

/// Recover Claude Code history into the log.
pub fn run(paths: &Paths, apply: bool) -> io::Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("no HOME to read history from"))?;
    let transcripts = home.join(".claude").join("projects");

    let outcome = recover(paths, &transcripts, apply, true)?;

    println!("history   {}", transcripts.display());
    println!(
        "recovered {} conversation(s), {} turn(s), {} record(s){}",
        outcome.sessions,
        outcome.turns,
        outcome.records,
        if outcome.already_backfilled > 0 {
            format!("; {} already backfilled", outcome.already_backfilled)
        } else {
            String::new()
        }
    );
    if outcome.completed > 0 {
        println!(
            "completed {} conversation(s) recovered before the session stamp held everything \
             it holds now",
            outcome.completed
        );
    }
    if outcome.recorded_live > 0 {
        println!(
            "live      {} conversation(s) the recorder saw as they happened, left alone",
            outcome.recorded_live
        );
    }
    match &outcome.codex_unavailable {
        Some(why) => println!("codex     history unavailable — {why}"),
        None => println!(
            "codex     recovered {} thread(s), {} turn(s), {} record(s); {} already backfilled; {} the recorder saw live",
            outcome.codex_threads,
            outcome.codex_turns,
            outcome.codex_records,
            outcome.codex_already,
            outcome.codex_live
        ),
    }
    println!("not read  {}", summarise_unread(&outcome.unread));
    if apply {
        println!("written to {}", paths.log_dir().display());
    } else {
        println!("\nNothing was written. Re-run with --apply to record it.");
    }
    Ok(())
}

/// Turns missed while the daemon was down, written under the live `bus_id`.
///
/// Ordinary backfill leaves live sessions alone so it cannot twin them.
/// Gap fill is the other job: the official transcript (the same
/// fixture-protected parser, not the live hook path) is read only to find
/// turns the log does not already hold, and those are appended to the
/// conversation the recorder already bound.
pub fn run_gaps(paths: &Paths, apply: bool) -> io::Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("no HOME to read history from"))?;
    let transcripts = home.join(".claude").join("projects");
    let outcome = fill_gaps(paths, &transcripts, apply)?;

    println!("history   {}", transcripts.display());
    println!(
        "gaps      {} conversation(s), {} turn(s), {} record(s)",
        outcome.gap_sessions, outcome.gap_turns, outcome.gap_records
    );
    println!("not read  {}", summarise_unread(&outcome.unread));
    if apply {
        println!("written to {}", paths.log_dir().display());
    } else {
        println!("\nNothing was written. Re-run with --apply to record it, or restart the daemon.");
    }
    Ok(())
}

/// Daemon start: fill gaps into a log the daemon already owns.
///
/// The socket is bound but the accept loop has not started, so this is not
/// racing hook writes. A failure here must not stop the daemon recording.
pub fn append_gaps_at_start(
    _paths: &Paths,
    held: &[Record],
    log: &mut SegmentLog,
) -> io::Result<u64> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Ok(0);
    };
    let transcripts = home.join(".claude").join("projects");
    if !transcripts.is_dir() {
        return Ok(0);
    }
    let outcome = append_gaps(&transcripts, held, Some(log))?;
    Ok(outcome.gap_records)
}

/// Read live sessions' official transcripts and, when `apply`, append the
/// turns the log does not already hold. Refuses to write while a daemon is
/// listening — the daemon fills the same gaps itself on restart.
pub fn fill_gaps(paths: &Paths, transcripts: &Path, apply: bool) -> io::Result<Outcome> {
    if apply && std::os::unix::net::UnixStream::connect(paths.socket()).is_ok() {
        return Err(io::Error::other(format!(
            "a daemon is listening on {}; it fills gaps on restart, and writing \
             beside it would interleave the same segment",
            paths.socket().display()
        )));
    }
    let held = SegmentLog::scan(paths.log_dir())?.records;
    let mut log = apply
        .then(|| SegmentLog::open(paths.log_dir(), Retention::default()))
        .transpose()?;
    append_gaps(transcripts, &held, log.as_mut())
}

/// Turns the official transcript has that the live log does not.
///
/// Reconstruction never outranks a live record: an already-seen turn stays
/// the live one. A turn that happened while the daemon was down is written
/// under the live `bus_id` so a restart does not fork the conversation.
fn missing_from<'a>(
    held: impl IntoIterator<Item = &'a Record>,
    recovered: Recovered,
) -> Vec<Entry> {
    let mut seen_ids = HashSet::new();
    let mut seen_prompts = HashSet::new();
    let mut bus_id = None;
    for record in held {
        bus_id = Some(record.bus_id);
        if let Some(id) = &record.vendor_turn_id {
            seen_ids.insert(id.as_str().to_string());
        }
        if let Event::TurnStarted { prompt, .. } = &record.event
            && let Some(text) = prompt_text(prompt)
        {
            seen_prompts.insert(text);
        }
    }
    let Some(bus_id) = bus_id else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut keep_open = false;
    for mut entry in recovered.entries {
        if matches!(entry.event, Event::SessionCandidate { .. }) {
            continue;
        }
        if let Event::TurnStarted { prompt, .. } = &entry.event {
            let by_id = entry
                .vendor_turn_id
                .as_ref()
                .is_some_and(|id| seen_ids.contains(id.as_str()));
            let by_prompt = prompt_text(prompt).is_some_and(|text| seen_prompts.contains(&text));
            keep_open = !by_id && !by_prompt;
        }
        if !keep_open {
            continue;
        }
        entry.bus_id = bus_id;
        if let Some(id) = &entry.vendor_turn_id {
            seen_ids.insert(id.as_str().to_string());
        }
        if let Event::TurnStarted { prompt, .. } = &entry.event
            && let Some(text) = prompt_text(prompt)
        {
            seen_prompts.insert(text);
        }
        out.push(entry);
    }
    out
}

fn prompt_text(body: &Body) -> Option<String> {
    match body {
        Body::Kept { text } | Body::Clipped { text, .. } => Some(text.clone()),
        Body::Elided { .. } => None,
    }
}

fn append_gaps(
    transcripts: &Path,
    held: &[Record],
    mut log: Option<&mut SegmentLog>,
) -> io::Result<Outcome> {
    let known = known_from(held.iter());
    let mut outcome = Outcome::default();
    if !known.unreadable_live.is_empty() {
        outcome.unread.insert(
            "live session id is not a uuid".into(),
            known.unreadable_live.len() as u64,
        );
    }
    for file in session_files(transcripts)? {
        let Some(session) = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<BusId>().ok())
        else {
            continue;
        };
        let Some(&live_bus) = known.live_bus.get(&session) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&file) else {
            *outcome.unread.entry("unreadable file".into()).or_default() += 1;
            continue;
        };
        let recovered = parse_claude_transcript(&text, live_bus);
        for (reason, count) in &recovered.skipped {
            *outcome.unread.entry(reason.clone()).or_default() += count;
        }
        let missing = missing_from(held.iter().filter(|r| r.bus_id == live_bus), recovered);
        if missing.is_empty() {
            continue;
        }
        outcome.gap_sessions += 1;
        outcome.gap_turns += missing
            .iter()
            .filter(|e| matches!(e.event, Event::TurnStarted { .. }))
            .count() as u64;
        outcome.gap_records += missing.len() as u64;
        if let Some(log) = log.as_deref_mut() {
            for entry in missing {
                log.append(entry)?;
            }
        }
    }
    Ok(outcome)
}

/// Read every transcript under `transcripts` — and, when `codex`, every
/// thread the app-server lists — and, when `apply`, append what the log does
/// not already hold. `codex` is a parameter so a test of the transcript path
/// never starts the real app-server underneath it.
pub fn recover(paths: &Paths, transcripts: &Path, apply: bool, codex: bool) -> io::Result<Outcome> {
    // Backfill appends to the segments directly, so it must not race the daemon
    // for the same file. Refusing is the honest answer; interleaved writes would
    // corrupt the one thing the whole design calls the source of truth. A dry
    // run only reads, which the daemon's own `doctor` does beside it on every
    // call, so it is allowed through — and it opens nothing, so that it leaves
    // nothing behind either.
    if apply && std::os::unix::net::UnixStream::connect(paths.socket()).is_ok() {
        return Err(io::Error::other(format!(
            "a daemon is listening on {}; stop it before backfilling, because both would \
             append to the same segment",
            paths.socket().display()
        )));
    }

    let known = known_sessions(paths)?;
    let mut log = apply
        .then(|| SegmentLog::open(paths.log_dir(), Retention::default()))
        .transpose()?;
    let mut outcome = Outcome::default();
    if !known.unreadable_live.is_empty() {
        outcome.unread.insert(
            "live session id is not a uuid".into(),
            known.unreadable_live.len() as u64,
        );
    }

    for file in session_files(transcripts)? {
        // The file's own name is the vendor session id, and it is a UUID — so
        // it can *be* the `bus_id`. That makes a second run produce identical
        // ids for the same conversations rather than a second copy of history.
        let Some(bus_id) = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<BusId>().ok())
        else {
            *outcome
                .unread
                .entry("file name is not a session id".into())
                .or_default() += 1;
            continue;
        };
        // The recorder saw this one as it happened. Its live records are the
        // conversation, and they are the stronger fact; a copy under the
        // file's UUID would be a second conversation for the same session,
        // and the one a restart used to bind the session to.
        if known.live.contains(&bus_id) {
            outcome.recorded_live += 1;
            continue;
        }
        let already = &known.backfilled;
        // Already complete: it is in the log and it knows where it happened.
        if already.get(&bus_id) == Some(&true) {
            outcome.already_backfilled += 1;
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            *outcome.unread.entry("unreadable file".into()).or_default() += 1;
            continue;
        };

        let mut recovered = parse_claude_transcript(&text, bus_id);
        for (reason, count) in &recovered.skipped {
            *outcome.unread.entry(reason.clone()).or_default() += count;
        }
        if recovered.entries.is_empty() {
            continue;
        }
        // Already in the log, but recovered by a build that did not record
        // everything the session stamp now holds. Append the session record
        // alone: the turns are already there, and appending them again would
        // put the conversation in the log twice, which an append-only log
        // cannot take back.
        let completing = already.contains_key(&bus_id);
        if completing {
            recovered
                .entries
                .retain(|entry| matches!(entry.event, Event::SessionCandidate { .. }));
            let nothing_new = recovered.workspace.is_none() && recovered.entrypoint.is_none();
            if recovered.entries.is_empty() || nothing_new {
                outcome.already_backfilled += 1;
                continue;
            }
            // Counted apart from `sessions`: nothing was recovered here, an
            // existing conversation only learned something about itself, and
            // reporting the two as one would overstate what a re-run found.
            outcome.completed += 1;
        } else {
            outcome.sessions += 1;
        }
        outcome.turns += recovered
            .entries
            .iter()
            .filter(|e| matches!(e.event, Event::TurnStarted { .. }))
            .count() as u64;
        outcome.records += recovered.entries.len() as u64;

        if let Some(log) = log.as_mut() {
            for entry in recovered.entries {
                log.append(entry)?;
            }
        }
    }
    if codex {
        recover_codex(&known, log.as_mut(), &mut outcome)?;
    }
    Ok(outcome)
}

/// Codex history, through the app-server's thread APIs.
///
/// A source that cannot be read is reported and does not fail the run: the
/// Claude transcripts above were already recovered, and a machine without
/// Codex is an ordinary machine.
fn recover_codex(
    known: &Known,
    mut log: Option<&mut SegmentLog>,
    outcome: &mut Outcome,
) -> io::Result<()> {
    let mut server = match AppServer::start(CODEX_DEADLINE) {
        Ok(server) => server,
        Err(err) => {
            outcome.codex_unavailable = Some(err.to_string());
            return Ok(());
        }
    };
    let mut cursor: Option<String> = None;
    loop {
        let mut params = serde_json::json!({"limit": 100});
        if let Some(cursor) = &cursor {
            params["cursor"] = serde_json::json!(cursor);
        }
        let page = match server.call("thread/list", params) {
            Ok(page) => page,
            Err(err) => {
                outcome.codex_unavailable = Some(err.to_string());
                return Ok(());
            }
        };
        for thread in page["data"].as_array().into_iter().flatten() {
            let Some(id) = thread["id"]
                .as_str()
                .and_then(|id| id.parse::<BusId>().ok())
            else {
                *outcome
                    .unread
                    .entry("thread id is not a uuid".into())
                    .or_default() += 1;
                continue;
            };
            // The same two rules as the transcripts: the recorder's own
            // records outrank a reconstruction, and a thread already
            // recovered is not recovered twice.
            if known.live_codex.contains(&id) {
                outcome.codex_live += 1;
                continue;
            }
            if known.backfilled.contains_key(&id) {
                outcome.codex_already += 1;
                continue;
            }
            let turns = match list_turns(&mut server, &id.to_string()) {
                Ok(turns) => turns,
                Err(err) => {
                    *outcome
                        .unread
                        .entry(format!("thread/turns/list failed: {err}"))
                        .or_default() += 1;
                    continue;
                }
            };
            let recovered = parse_codex_thread(thread, &turns);
            for (reason, count) in &recovered.skipped {
                *outcome.unread.entry(reason.clone()).or_default() += count;
            }
            if recovered.entries.is_empty() {
                continue;
            }
            outcome.codex_threads += 1;
            outcome.codex_turns += recovered
                .entries
                .iter()
                .filter(|e| matches!(e.event, Event::TurnStarted { .. }))
                .count() as u64;
            outcome.codex_records += recovered.entries.len() as u64;
            if let Some(log) = log.as_deref_mut() {
                for entry in recovered.entries {
                    log.append(entry)?;
                }
            }
        }
        cursor = page["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(())
}

/// Every turn of one thread, page by page.
fn list_turns(server: &mut AppServer, thread_id: &str) -> io::Result<Vec<serde_json::Value>> {
    let mut turns = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut params = serde_json::json!({"threadId": thread_id, "limit": 100});
        if let Some(cursor) = &cursor {
            params["cursor"] = serde_json::json!(cursor);
        }
        let page = server.call("thread/turns/list", params)?;
        turns.extend(page["data"].as_array().into_iter().flatten().cloned());
        cursor = page["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            return Ok(turns);
        }
    }
}

/// What the log already holds, keyed the two ways a transcript can be in it.
struct Known {
    /// Conversations an earlier run recovered, and whether each one knows its
    /// workspace.
    ///
    /// Two answers rather than one because a second run has two jobs. It must
    /// not double a conversation it already has — that is what the set was
    /// for — and it must be able to finish a conversation recovered by a build
    /// that did not record the workspace, which is every conversation on disk
    /// before this one.
    backfilled: HashMap<BusId, bool>,
    /// Claude Code sessions the recorder mirrored as they happened, by the
    /// vendor's session id — which is the transcript's file name. Keyed by
    /// what the recorder saw rather than by what backfill wrote, because the
    /// first run of this that keyed only on its own output recovered every
    /// live session a second time.
    live: HashSet<BusId>,
    /// The live `bus_id` the recorder minted for each Claude session. Last
    /// write wins so a restart binds the same way the registry does.
    live_bus: HashMap<BusId, BusId>,
    /// Codex sessions the recorder mirrored, by the vendor's session id —
    /// which is the thread id the app-server lists.
    live_codex: HashSet<BusId>,
    /// Live Claude Code session ids that are not UUIDs, so cannot be matched
    /// to a transcript and cannot be protected from a copy either. None have
    /// been seen; counted so that the first one is not a silent gap.
    unreadable_live: HashSet<String>,
}

fn known_sessions(paths: &Paths) -> io::Result<Known> {
    Ok(known_from(
        SegmentLog::scan(paths.log_dir())?.records.iter(),
    ))
}

fn known_from<'a>(records: impl IntoIterator<Item = &'a Record>) -> Known {
    let mut known = Known {
        backfilled: HashMap::new(),
        live: HashSet::new(),
        live_bus: HashMap::new(),
        live_codex: HashSet::new(),
        unreadable_live: HashSet::new(),
    };
    for record in records {
        if record.source.backfilled_from.is_none() {
            let Some(session) = record.source.vendor_session_id.as_deref() else {
                continue;
            };
            match (record.source.harness.as_str(), session.parse::<BusId>()) {
                (Harness::CLAUDE_CODE, Ok(session)) => {
                    known.live.insert(session);
                    known.live_bus.insert(session, record.bus_id);
                }
                (Harness::CLAUDE_CODE, Err(_)) => {
                    known.unreadable_live.insert(session.to_string());
                }
                (Harness::CODEX, Ok(session)) => {
                    known.live_codex.insert(session);
                }
                _ => {}
            }
            continue;
        }
        if record.source.backfilled_from.as_deref() == Some(CODEX_THREAD) {
            known.backfilled.insert(record.bus_id, true);
            continue;
        }
        if record.source.backfilled_from.as_deref() != Some(CLAUDE_TRANSCRIPT) {
            continue;
        }
        // Complete means it knows both where it happened and what it ran
        // under. Each arrived in its own release, so a log that has been
        // through both holds conversations missing one, the other, or neither.
        let complete = record.source.workspace.is_some() && record.source.entrypoint.is_some();
        known
            .backfilled
            .entry(record.bus_id)
            .and_modify(|seen| *seen |= complete)
            .or_insert(complete);
    }
    known
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::net::UnixListener;

    use conbus_core::event::{Body, Harness, Source};
    use conbus_core::record::Entry;

    /// A vendor session id, which is also the transcript's file name.
    const SESSION: &str = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";

    /// A conbus root, a transcript tree, and the paths that name them.
    struct Machine {
        _root: tempfile::TempDir,
        paths: Paths,
        transcripts: PathBuf,
    }

    fn machine() -> Machine {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(
            Some(root.path().join("conbus").to_str().expect("utf-8")),
            None,
            None,
        )
        .expect("paths");
        fs::create_dir_all(paths.log_dir()).expect("log dir");
        let transcripts = root.path().join("projects");
        Machine {
            _root: root,
            paths,
            transcripts,
        }
    }

    /// One Claude Code transcript for `session`, shaped like the real ones:
    /// a prompt line carrying the checkout and the front end, and a reply.
    fn transcript(machine: &Machine, session: &str) {
        let project = machine.transcripts.join("-Applications-conbus");
        fs::create_dir_all(&project).expect("project dir");
        fs::write(
            project.join(format!("{session}.jsonl")),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:33:02.222Z\",\
                 \"version\":\"2.1.241\",\"cwd\":\"/Applications/conbus\",\"entrypoint\":\"cli\",\
                 \"promptId\":\"P1\",\"message\":{{\"role\":\"user\",\"content\":\"Reply with OK\"}}}}\n\
                 {{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:33:03.000Z\",\
                 \"version\":\"2.1.241\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\
                 \"text\":\"OK\"}}]}}}}\n"
            ),
        )
        .expect("write transcript");
    }

    /// What the recorder writes when it sees `session`'s first prompt live.
    fn recorded_live(machine: &Machine, session: &str) {
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");
        log.append(Entry {
            bus_id: BusId::mint(),
            observed_at: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some(session.into()),
                workspace: Some("/Applications/conbus".into()),
                entrypoint: None,
                backfilled_from: None,
            },
            vendor_turn_id: None,
            vendor_agent_id: None,
            event: Event::TurnStarted {
                author: None,
                prompt: Body::kept("Reply with OK"),
            },
        })
        .expect("append");
    }

    fn records_on_disk(machine: &Machine) -> usize {
        SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records
            .len()
    }

    #[test]
    fn a_conversation_nobody_recorded_is_recovered() {
        let machine = machine();
        transcript(&machine, SESSION);

        let outcome = recover(&machine.paths, &machine.transcripts, true, false).expect("recover");

        assert_eq!(outcome.sessions, 1);
        assert_eq!(outcome.turns, 1);
        assert_eq!(
            outcome.records, 3,
            "a session record, a prompt, and its reply"
        );
        assert_eq!(records_on_disk(&machine), 3);
    }

    #[test]
    fn a_conversation_the_recorder_saw_live_is_left_alone() {
        // The twin defect. The recorder mirrored this session as it happened;
        // its transcript is on disk like every other; and backfill, keyed only
        // by what *backfill* had written before, recovered it again under the
        // file's own UUID. One session, two conversations — and the copy is
        // what a restart used to bind the session to. Thirty live sessions on
        // a real machine were one `--apply` away from that.
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript(&machine, SESSION);

        let outcome = recover(&machine.paths, &machine.transcripts, true, false).expect("recover");

        assert_eq!(outcome.recorded_live, 1);
        assert_eq!(outcome.sessions, 0, "the live records are the conversation");
        assert_eq!(
            records_on_disk(&machine),
            1,
            "nothing may be appended for a session the recorder already holds"
        );
    }

    #[test]
    fn a_live_session_whose_id_is_not_a_uuid_is_counted_not_forgotten() {
        // Such a session cannot match a transcript's file name, so it cannot
        // be twinned either — but a live id this code could not read is a
        // fact about the log, and this codebase counts what it does not
        // understand rather than dropping it in silence.
        let machine = machine();
        recorded_live(&machine, "not-a-uuid-at-all");
        transcript(&machine, SESSION);

        let outcome = recover(&machine.paths, &machine.transcripts, false, false).expect("recover");

        assert_eq!(
            outcome.unread.get("live session id is not a uuid"),
            Some(&1)
        );
        assert_eq!(outcome.sessions, 1, "the transcript is still recovered");
    }

    #[test]
    fn a_dry_run_reads_beside_a_running_daemon() {
        // A dry run only reads, and reading a segment the daemon is appending
        // to is what `doctor` does on every call. Making the report wait for
        // the daemon to stop is what stopped anyone from measuring what a
        // backfill would do before doing it.
        let machine = machine();
        transcript(&machine, SESSION);
        let _daemon = UnixListener::bind(machine.paths.socket()).expect("listen");

        let outcome =
            recover(&machine.paths, &machine.transcripts, false, false).expect("a dry run");

        assert_eq!(outcome.sessions, 1);
        assert_eq!(records_on_disk(&machine), 0, "a dry run writes nothing");
    }

    #[test]
    fn applying_refuses_while_a_daemon_is_listening() {
        // Two writers on one segment would interleave lines in the source of
        // truth. This is the refusal that must survive the dry run learning to
        // read beside the daemon.
        let machine = machine();
        transcript(&machine, SESSION);
        let _daemon = UnixListener::bind(machine.paths.socket()).expect("listen");

        let refused = recover(&machine.paths, &machine.transcripts, true, false);

        assert!(refused.is_err(), "two writers on one segment");
        assert_eq!(records_on_disk(&machine), 0);
    }

    fn transcript_two_turns(machine: &Machine, session: &str) {
        let project = machine.transcripts.join("-Applications-conbus");
        fs::create_dir_all(&project).expect("project dir");
        fs::write(
            project.join(format!("{session}.jsonl")),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:33:02.222Z\",\
                 \"version\":\"2.1.241\",\"cwd\":\"/Applications/conbus\",\"entrypoint\":\"cli\",\
                 \"promptId\":\"P1\",\"message\":{{\"role\":\"user\",\"content\":\"Reply with OK\"}}}}\n\
                 {{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:33:03.000Z\",\
                 \"version\":\"2.1.241\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\
                 \"text\":\"OK\"}}]}}}}\n\
                 {{\"type\":\"user\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:34:00.000Z\",\
                 \"version\":\"2.1.241\",\"cwd\":\"/Applications/conbus\",\"entrypoint\":\"cli\",\
                 \"promptId\":\"P2\",\"message\":{{\"role\":\"user\",\"content\":\"the turn the daemon missed\"}}}}\n\
                 {{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:34:01.000Z\",\
                 \"version\":\"2.1.241\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\
                 \"text\":\"caught up\"}}]}}}}\n"
            ),
        )
        .expect("write transcript");
    }

    fn live_bus(machine: &Machine) -> BusId {
        SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records[0]
            .bus_id
    }

    #[test]
    fn a_turn_missed_while_the_daemon_was_down_joins_the_live_bus() {
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript_two_turns(&machine, SESSION);
        let bus = live_bus(&machine);

        let outcome = fill_gaps(&machine.paths, &machine.transcripts, true).expect("fill");

        assert_eq!(outcome.gap_sessions, 1);
        assert_eq!(outcome.gap_turns, 1);
        assert_eq!(outcome.gap_records, 2, "the missed prompt and its reply");
        let records = SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records;
        assert_eq!(records.len(), 3);
        assert!(
            records.iter().all(|r| r.bus_id == bus),
            "same conversation, not a twin"
        );
        assert_eq!(
            records[1].source.backfilled_from.as_deref(),
            Some(CLAUDE_TRANSCRIPT)
        );
        match &records[1].event {
            Event::TurnStarted { prompt, .. } => {
                assert_eq!(prompt, &Body::kept("the turn the daemon missed"));
            }
            other => panic!("expected the missed prompt, got {other:?}"),
        }
    }

    #[test]
    fn a_live_session_with_no_missing_turn_is_left_alone() {
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript(&machine, SESSION);

        let outcome = fill_gaps(&machine.paths, &machine.transcripts, true).expect("fill");

        assert_eq!(outcome.gap_sessions, 0);
        assert_eq!(records_on_disk(&machine), 1);
    }

    #[test]
    fn gap_apply_refuses_while_a_daemon_is_listening() {
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript_two_turns(&machine, SESSION);
        let _daemon = UnixListener::bind(machine.paths.socket()).expect("listen");

        let refused = fill_gaps(&machine.paths, &machine.transcripts, true);

        assert!(refused.is_err());
        assert_eq!(records_on_disk(&machine), 1);
    }
}
