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
    CLAUDE_CHAT_EXPORT, CLAUDE_TRANSCRIPT, CODEX_THREAD, Recovered, parse_claude_chat_export,
    parse_claude_transcript, parse_codex_thread,
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

/// Official Claude.ai / Desktop data export. Keeps the Chat conversation
/// uuid. `import-conversations` hashes that id into a Code session; this
/// path does not.
pub fn run_chat_export(paths: &Paths, export: &str, apply: bool) -> io::Result<()> {
    if apply && std::os::unix::net::UnixStream::connect(paths.socket()).is_ok() {
        return Err(io::Error::other(format!(
            "a daemon is listening on {}; stop it before backfilling, because both would \
             append to the same segment",
            paths.socket().display()
        )));
    }
    let path = PathBuf::from(export);
    let text = read_chat_export(&path)?;
    let held = SegmentLog::scan(paths.log_dir())?.records;
    let mut log = apply
        .then(|| SegmentLog::open(paths.log_dir(), Retention::default()))
        .transpose()?;
    let outcome = ingest_chat_text(&held, &text, log.as_mut())?;

    println!("export    {}", path.display());
    println!(
        "recovered {} conversation(s), {} turn(s), {} record(s){}",
        outcome.sessions,
        outcome.turns,
        outcome.records,
        if outcome.already_backfilled > 0 {
            format!("; {} already complete", outcome.already_backfilled)
        } else {
            String::new()
        }
    );
    if outcome.gap_records > 0 {
        println!(
            "filled    {} conversation(s), {} new turn(s), {} record(s)",
            outcome.gap_sessions, outcome.gap_turns, outcome.gap_records
        );
    }
    if outcome.recorded_live > 0 {
        println!(
            "live      {} conversation(s) the recorder already saw, left alone",
            outcome.recorded_live
        );
    }
    println!("not read  {}", summarise_unread(&outcome.unread));
    if apply {
        println!("written to {}", paths.log_dir().display());
    } else {
        println!("\nNothing was written. Re-run with --apply to record it.");
    }
    Ok(())
}

/// Official Chat export: first sighting is a new conversation; a later
/// export of the same uuid appends only the turns the log does not hold.
fn ingest_chat_text(
    held: &[Record],
    text: &str,
    mut log: Option<&mut SegmentLog>,
) -> io::Result<Outcome> {
    let known = known_from(held.iter());
    let mut outcome = Outcome::default();
    for recovered in parse_claude_chat_export(text) {
        for (reason, count) in &recovered.skipped {
            *outcome.unread.entry(reason.clone()).or_default() += count;
        }
        let Some(session) = recovered
            .vendor_session_id
            .as_deref()
            .and_then(|id| id.parse::<BusId>().ok())
        else {
            continue;
        };
        let already = known.live.contains(&session) || known.backfilled.contains_key(&session);
        if already {
            let sid = session.to_string();
            let mine: Vec<&Record> = held
                .iter()
                .filter(|record| {
                    record.bus_id == session
                        || record.source.vendor_session_id.as_deref() == Some(sid.as_str())
                })
                .collect();
            let missing = missing_from(mine, recovered);
            if missing.is_empty() {
                if known.live.contains(&session) {
                    outcome.recorded_live += 1;
                } else {
                    outcome.already_backfilled += 1;
                }
                continue;
            }
            outcome.gap_sessions += 1;
            outcome.gap_turns += missing
                .iter()
                .filter(|entry| matches!(entry.event, Event::TurnStarted { .. }))
                .count() as u64;
            outcome.gap_records += missing.len() as u64;
            if let Some(log) = log.as_mut() {
                for entry in missing {
                    log.append(entry)?;
                }
            }
            continue;
        }
        if recovered.entries.is_empty() {
            continue;
        }
        outcome.sessions += 1;
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
    Ok(outcome)
}

fn read_chat_export(path: &Path) -> io::Result<String> {
    if path.is_dir() {
        return fs::read_to_string(path.join("conversations.json"));
    }
    let ext = path.extension().and_then(|ext| ext.to_str());
    if ext == Some("json")
        || path.file_name().and_then(|n| n.to_str()) == Some("conversations.json")
    {
        return fs::read_to_string(path);
    }
    if ext == Some("zip") {
        return unzip_conversations(path);
    }
    Err(io::Error::other(
        "chat export must be conversations.json, a directory containing it, or a zip",
    ))
}

/// Official Privacy export zip puts `conversations.json` at the root.
/// Some mailed archives nest it one folder down. First match only.
fn unzip_conversations(path: &Path) -> io::Result<String> {
    let listing = std::process::Command::new("/usr/bin/unzip")
        .args(["-Z", "-1"])
        .arg(path)
        .output()?;
    if !listing.status.success() {
        return Err(io::Error::other("zip is unreadable"));
    }
    let names = String::from_utf8_lossy(&listing.stdout);
    let member = names.lines().find(|name| {
        Path::new(name)
            .file_name()
            .is_some_and(|n| n == "conversations.json")
            && !name.contains("..")
    });
    let Some(member) = member else {
        return Err(io::Error::other("zip has no conversations.json"));
    };
    let out = std::process::Command::new("/usr/bin/unzip")
        .args(["-p"])
        .arg(path)
        .arg(member)
        .output()?;
    if out.status.success() && !out.stdout.is_empty() {
        return String::from_utf8(out.stdout)
            .map_err(|err| io::Error::other(format!("conversations.json is not utf-8: {err}")));
    }
    Err(io::Error::other("zip conversations.json is empty"))
}

/// Daemon start: fill gaps into a log the daemon already owns.
///
/// The socket is bound but the accept loop has not started, so this is not
/// racing hook writes. A failure here must not stop the daemon recording.
pub fn append_gaps_at_start(
    paths: &Paths,
    held: &[Record],
    log: &mut SegmentLog,
) -> io::Result<u64> {
    // Only what changed after the log's newest record can hold a turn that
    // happened while no daemon was listening. Reading every transcript the log
    // had ever bound took ten seconds with a warm cache and 71 after a reboot
    // on a machine with 6,190 of them (6.3 GB) — with the socket already
    // bound, so every hook in that window timed out. Earlier downtimes were
    // filled by earlier starts; `conbus backfill --gaps` still reads them all.
    let since = newest_unix(held).map(|newest| newest - GAP_MARGIN_SECS);
    let mut n = 0;
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let transcripts = home.join(".claude").join("projects");
        if transcripts.is_dir() {
            n += append_gaps(&transcripts, held, Some(log), since)?.gap_records;
        }
    }
    // Codex.app burns the same account and never hits CLI hooks. Official
    // thread/turns/list on threads we already bound fills those turns under
    // the live `bus_id` — not a second conversation.
    n += fill_live_codex_gaps(held, log, since)?;
    n += ingest_chat_exports_since(&paths.chat_export(), held, log, since)?;
    Ok(n)
}

/// How long before the log's newest record a file may have last changed and
/// still hold a turn this daemon missed: vendors buffer their writes, and a
/// clock can step.
pub(crate) const GAP_MARGIN_SECS: i64 = 15 * 60;

/// The newest record's time, as unix seconds.
fn newest_unix(held: &[Record]) -> Option<i64> {
    let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0)?;
    held.iter()
        .map(|record| record.ts)
        .max()
        .map(|newest| newest.seconds_since(epoch))
}

/// A file's modification time, as unix seconds.
fn modified_unix(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let seconds = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(seconds).ok()
}

/// Whether a file is old enough that it cannot hold anything after `since`.
fn untouched_since(path: &Path, since: Option<i64>) -> bool {
    match (since, modified_unix(path)) {
        (Some(since), Some(modified)) => modified < since,
        // No cutoff, or a file whose time cannot be read: read it.
        _ => false,
    }
}

/// Every record, grouped by conversation, so a lookup per file is not a pass
/// over the whole log per file.
fn by_bus(held: &[Record]) -> HashMap<BusId, Vec<&Record>> {
    let mut grouped: HashMap<BusId, Vec<&Record>> = HashMap::new();
    for record in held {
        grouped.entry(record.bus_id).or_default().push(record);
    }
    grouped
}

/// Vendor Privacy → Export data names the zip `data-export-*.zip`.
/// Desktop session export uses `claude-desktop-export-*.zip`.
/// The drop folder also accepts a bare `conversations.json`.
pub fn is_official_privacy_export(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name == "conversations.json" {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".zip")
        && (lower.starts_with("data-export") || lower.starts_with("claude-desktop-export"))
}

/// Names and sizes of drop-folder files. The daemon skips a full log scan
/// when this has not changed.
pub fn chat_export_signature(dir: &Path) -> Vec<(String, u64, u64)> {
    chat_export_signature_filtered(dir, false)
}

pub fn chat_export_signature_filtered(dir: &Path, official_only: bool) -> Vec<(String, u64, u64)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, u64, u64)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok()?;
            if !meta.is_file() && !meta.is_dir() {
                return None;
            }
            if official_only && !is_official_privacy_export(&entry.path()) {
                return None;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Some((
                entry.file_name().to_string_lossy().into_owned(),
                mtime,
                meta.len(),
            ))
        })
        .collect();
    out.sort();
    out
}

/// Official Claude.ai / Desktop export drop folder. Chat-only windows have
/// no hook; a newer `conversations.json` is the identity-preserving channel.
pub fn ingest_chat_exports(dir: &Path, held: &[Record], log: &mut SegmentLog) -> io::Result<u64> {
    ingest_chat_exports_filtered(dir, held, log, false, None)
}

/// The drop folder at start: only exports written after `since`.
fn ingest_chat_exports_since(
    dir: &Path,
    held: &[Record],
    log: &mut SegmentLog,
    since: Option<i64>,
) -> io::Result<u64> {
    ingest_chat_exports_filtered(dir, held, log, false, since)
}

/// `~/Downloads` after Privacy → Export data. Only vendor-named zips / JSON.
pub fn ingest_official_privacy_exports(
    dir: &Path,
    held: &[Record],
    log: &mut SegmentLog,
) -> io::Result<u64> {
    ingest_chat_exports_filtered(dir, held, log, true, None)
}

fn ingest_chat_exports_filtered(
    dir: &Path,
    held: &[Record],
    log: &mut SegmentLog,
    official_only: bool,
    since: Option<i64>,
) -> io::Result<u64> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut filled = 0u64;
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| !official_only || is_official_privacy_export(path))
        .filter(|path| !untouched_since(path, since))
        .collect();
    entries.sort();
    for path in entries {
        let Ok(text) = read_chat_export(&path) else {
            continue;
        };
        let outcome = ingest_chat_text(held, &text, Some(log))?;
        filled += outcome.records + outcome.gap_records;
    }
    Ok(filled)
}

/// Official app-server only. Live Codex threads the recorder already bound.
fn fill_live_codex_gaps(
    held: &[Record],
    log: &mut SegmentLog,
    since: Option<i64>,
) -> io::Result<u64> {
    let known = known_from(held.iter());
    if known.live_codex.is_empty() {
        return Ok(0);
    }
    // Restart must not wait out the 10-minute backfill budget: the socket
    // is already bound, so a wedged initialize leaves every hook as EAGAIN.
    let mut server = match AppServer::start(std::time::Duration::from_secs(8)) {
        Ok(server) => server,
        Err(_) => return Ok(0),
    };
    // One `thread/list` pass says which threads moved since the cutoff, so a
    // start asks for the turns of those rather than of every thread the log
    // has ever bound. When the list cannot be read, every thread is asked.
    let recent = since.and_then(|since| threads_updated_since(&mut server, since));
    let grouped = by_bus(held);
    let mut filled = 0u64;
    for session in &known.live_codex {
        if recent
            .as_ref()
            .is_some_and(|recent| !recent.contains(session))
        {
            continue;
        }
        let Some(&bus_id) = known.live_codex_bus.get(session) else {
            continue;
        };
        let turns = match list_turns(&mut server, &session.to_string()) {
            Ok(turns) => turns,
            Err(_) => continue,
        };
        let thread =
            serde_json::json!({"id": session.to_string(), "sessionId": session.to_string()});
        let recovered = parse_codex_thread(&thread, &turns);
        let mine = grouped.get(&bus_id).into_iter().flatten().copied();
        for entry in missing_from(mine, recovered) {
            if log.append(entry)?.is_some() {
                filled += 1;
            }
        }
    }
    Ok(filled)
}

/// Threads `thread/list` says were updated at or after `since`, or `None`
/// when the list cannot be read to the end.
fn threads_updated_since(server: &mut AppServer, since: i64) -> Option<HashSet<BusId>> {
    let mut recent = HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut params = serde_json::json!({"limit": 100});
        if let Some(cursor) = &cursor {
            params["cursor"] = serde_json::json!(cursor);
        }
        let page = server.call("thread/list", params).ok()?;
        for thread in page["data"].as_array().into_iter().flatten() {
            // Strings on `thread/list`, numbers elsewhere in the same API.
            let updated = match &thread["updatedAt"] {
                serde_json::Value::String(text) => text.parse::<i64>().ok(),
                other => other.as_i64(),
            };
            let Some(id) = thread["id"]
                .as_str()
                .and_then(|id| id.parse::<BusId>().ok())
            else {
                continue;
            };
            // A thread that does not say when it moved is asked about.
            if updated.is_none_or(|updated| updated >= since) {
                recent.insert(id);
            }
        }
        cursor = page["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            return Some(recent);
        }
    }
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
    append_gaps(transcripts, &held, log.as_mut(), None)
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
        Body::Elided { .. } | Body::Sealed { .. } => None,
    }
}

fn append_gaps(
    transcripts: &Path,
    held: &[Record],
    mut log: Option<&mut SegmentLog>,
    since: Option<i64>,
) -> io::Result<Outcome> {
    let known = known_from(held.iter());
    let grouped = by_bus(held);
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
        if untouched_since(&file, since) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            *outcome.unread.entry("unreadable file".into()).or_default() += 1;
            continue;
        };
        let recovered = parse_claude_transcript(&text, live_bus);
        for (reason, count) in &recovered.skipped {
            *outcome.unread.entry(reason.clone()).or_default() += count;
        }
        let missing = missing_from(
            grouped.get(&live_bus).into_iter().flatten().copied(),
            recovered,
        );
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
    /// The live `bus_id` for each Codex thread, so gap fill writes under the
    /// conversation the recorder already minted.
    live_codex_bus: HashMap<BusId, BusId>,
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
        live_codex_bus: HashMap::new(),
        unreadable_live: HashSet::new(),
    };
    for record in records {
        if record.source.backfilled_from.is_none() {
            let Some(session) = record.source.vendor_session_id.as_deref() else {
                continue;
            };
            match (record.source.harness.as_str(), session.parse::<BusId>()) {
                (Harness::CLAUDE_CODE | Harness::CLAUDE_CHAT, Ok(session)) => {
                    known.live.insert(session);
                    known.live_bus.insert(session, record.bus_id);
                }
                (Harness::CLAUDE_CODE | Harness::CLAUDE_CHAT, Err(_)) => {
                    known.unreadable_live.insert(session.to_string());
                }
                (Harness::CODEX, Ok(session)) => {
                    known.live_codex.insert(session);
                    known.live_codex_bus.insert(session, record.bus_id);
                }
                _ => {}
            }
            continue;
        }
        if record.source.backfilled_from.as_deref() == Some(CODEX_THREAD) {
            known.backfilled.insert(record.bus_id, true);
            continue;
        }
        if record.source.backfilled_from.as_deref() == Some(CLAUDE_CHAT_EXPORT) {
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
                profile: None,
            },
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
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
    fn a_start_skips_a_transcript_untouched_since_before_the_newest_record() {
        // An untouched file cannot hold a turn this downtime missed, and
        // reading all of them is what kept the socket deaf for 71 s after a
        // reboot. Asked by hand, `backfill --gaps` still reads everything.
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript_two_turns(&machine, SESSION);
        let file = machine
            .transcripts
            .join("-Applications-conbus")
            .join(format!("{SESSION}.jsonl"));
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
        fs::File::options()
            .write(true)
            .open(&file)
            .expect("open")
            .set_modified(hour_ago)
            .expect("backdate");
        let held = SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records;
        let since = newest_unix(&held).map(|newest| newest - GAP_MARGIN_SECS);

        let at_start = append_gaps(&machine.transcripts, &held, None, since).expect("start");
        let by_hand = append_gaps(&machine.transcripts, &held, None, None).expect("by hand");

        assert_eq!(at_start.gap_records, 0);
        assert_eq!(by_hand.gap_records, 2, "the missed prompt and its reply");
    }

    #[test]
    fn a_start_still_reads_a_transcript_that_changed_after_the_newest_record() {
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript_two_turns(&machine, SESSION);
        let held = SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records;
        let since = newest_unix(&held).map(|newest| newest - GAP_MARGIN_SECS);

        let at_start = append_gaps(&machine.transcripts, &held, None, since).expect("start");

        assert_eq!(at_start.gap_records, 2);
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

    #[test]
    fn a_live_codex_thread_receives_official_turns_the_hooks_missed() {
        // Codex.app burns the account without CLI hooks. Official turns join
        // the live bus_id — they do not mint a twin. App-server I/O is the
        // recover suite; this pins the join.
        let live_thread = "01a06307-cc6a-7702-8533-d02e159b951d";
        let machine = machine();
        let bus = BusId::mint();
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");
        log.append(Entry {
            bus_id: bus,
            observed_at: None,
            source: Source {
                harness: Harness::new(Harness::CODEX),
                harness_version: None,
                vendor_session_id: Some(live_thread.into()),
                workspace: Some("/repo".into()),
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            },
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event: Event::TurnStarted {
                author: None,
                prompt: Body::kept("seen live"),
            },
        })
        .expect("append");
        let held = SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records;
        let turns = [serde_json::json!({
            "id": "t1",
            "items": [
                {"type": "userMessage", "id": "u1", "content": [{"type": "text", "text": "from the app"}]},
                {"type": "agentMessage", "id": "a1", "text": "ok", "phase": "final_answer"}
            ],
            "status": "completed",
            "startedAt": 1,
            "completedAt": 2
        })];
        let recovered = parse_codex_thread(
            &serde_json::json!({"id": live_thread, "sessionId": live_thread}),
            &turns,
        );
        let mine: Vec<_> = held.iter().filter(|r| r.bus_id == bus).collect();
        let missing = missing_from(mine, recovered);
        assert!(
            missing.iter().any(|e| e.bus_id == bus
                && matches!(&e.event, Event::TurnStarted { prompt, .. } if prompt_text(prompt).as_deref() == Some("from the app"))),
            "{missing:?}"
        );
    }

    #[test]
    fn a_later_chat_export_appends_only_the_new_turn() {
        let machine = machine();
        let first = r#"{
            "conversations": [{
                "uuid": "00000000-1111-2222-3333-444444444444",
                "chat_messages": [
                    {"uuid": "m1", "sender": "human", "text": "first", "created_at": "2026-09-06T12:00:00.000Z"},
                    {"uuid": "m2", "sender": "assistant", "text": "ok", "created_at": "2026-09-06T12:01:00.000Z"}
                ]
            }]
        }"#;
        let later = r#"{
            "conversations": [{
                "uuid": "00000000-1111-2222-3333-444444444444",
                "chat_messages": [
                    {"uuid": "m1", "sender": "human", "text": "first", "created_at": "2026-09-06T12:00:00.000Z"},
                    {"uuid": "m2", "sender": "assistant", "text": "ok", "created_at": "2026-09-06T12:01:00.000Z"},
                    {"uuid": "m3", "sender": "human", "text": "next turn", "created_at": "2026-09-06T12:02:00.000Z"},
                    {"uuid": "m4", "sender": "assistant", "text": "still the same chat", "created_at": "2026-09-06T12:03:00.000Z"}
                ]
            }]
        }"#;
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");
        let first_out = ingest_chat_text(&[], first, Some(&mut log)).expect("first");
        assert_eq!(first_out.sessions, 1);
        assert!(first_out.gap_records == 0);
        let held = SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records;
        let later_out = ingest_chat_text(&held, later, Some(&mut log)).expect("later");
        assert_eq!(later_out.sessions, 0);
        assert_eq!(later_out.already_backfilled, 0);
        assert_eq!(later_out.gap_sessions, 1);
        assert_eq!(later_out.gap_turns, 1);
        let all = SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records;
        assert!(
            all.iter().any(|r| matches!(
                &r.event,
                Event::TurnStarted { prompt, .. } if prompt_text(prompt).as_deref() == Some("next turn")
            )),
            "{all:?}"
        );
        assert!(
            all.iter()
                .all(|r| r.source.harness.as_str() == Harness::CLAUDE_CHAT)
        );
    }

    #[test]
    fn chat_export_signature_changes_only_when_a_file_lands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = chat_export_signature(dir.path());
        assert!(empty.is_empty());
        fs::write(dir.path().join("conversations.json"), "{}").expect("write");
        let once = chat_export_signature(dir.path());
        assert_eq!(once.len(), 1);
        assert_eq!(once, chat_export_signature(dir.path()));
        fs::write(
            dir.path().join("conversations.json"),
            "{\"conversations\":[]}",
        )
        .expect("grow");
        assert_ne!(once, chat_export_signature(dir.path()));
    }

    #[test]
    fn a_nested_privacy_export_zip_keeps_the_chat_uuid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inner = dir.path().join("mail");
        fs::create_dir(&inner).expect("dir");
        fs::write(
            inner.join("conversations.json"),
            r#"[{"uuid":"00000000-aaaa-bbbb-cccc-ddddeeeeffff","chat_messages":[
                {"uuid":"m1","sender":"human","text":"from the mailed zip","created_at":"2026-09-06T12:00:00.000Z"},
                {"uuid":"m2","sender":"assistant","text":"same conversation","created_at":"2026-09-06T12:01:00.000Z"}
            ]}]"#,
        )
        .expect("json");
        let zip = dir.path().join("export.zip");
        let status = std::process::Command::new("/usr/bin/zip")
            .args(["-q", "-r"])
            .arg(&zip)
            .arg("mail/conversations.json")
            .current_dir(dir.path())
            .status()
            .expect("zip");
        assert!(status.success());
        let text = read_chat_export(&zip).expect("read");
        let recovered = parse_claude_chat_export(&text);
        assert_eq!(
            recovered[0].vendor_session_id.as_deref(),
            Some("00000000-aaaa-bbbb-cccc-ddddeeeeffff")
        );
        assert_eq!(
            recovered[0].entries[0].source.harness.as_str(),
            Harness::CLAUDE_CHAT
        );
    }

    #[test]
    fn downloads_ingest_only_the_vendor_named_privacy_zip() {
        let machine = machine();
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("conversations.json"),
            r#"[{"uuid":"00000000-aaaa-bbbb-cccc-ddddeeeeffff","chat_messages":[
                {"uuid":"m1","sender":"human","text":"from downloads","created_at":"2026-09-06T12:00:00.000Z"},
                {"uuid":"m2","sender":"assistant","text":"kept","created_at":"2026-09-06T12:01:00.000Z"}
            ]}]"#,
        )
        .expect("json");
        let zip = dir.path().join("data-export-2026-09-06.zip");
        let status = std::process::Command::new("/usr/bin/zip")
            .args(["-q", "-j"])
            .arg(&zip)
            .arg(dir.path().join("conversations.json"))
            .status()
            .expect("zip");
        assert!(status.success());
        fs::write(dir.path().join("random.zip"), b"not an export").expect("noise");
        assert!(is_official_privacy_export(&zip));
        assert!(!is_official_privacy_export(&dir.path().join("random.zip")));
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");
        let filled =
            ingest_chat_exports_filtered(dir.path(), &[], &mut log, true, None).expect("ingest");
        assert!(filled >= 2, "{filled}");
        let all = SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records;
        assert!(
            all.iter()
                .all(|r| r.source.harness.as_str() == Harness::CLAUDE_CHAT)
        );
        assert!(all.iter().any(|r| matches!(
            &r.event,
            Event::TurnStarted { prompt, .. } if prompt_text(prompt).as_deref() == Some("from downloads")
        )));
    }
}
