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
use std::time::Instant;

use conbus_core::backfill::{
    CLAUDE_CHAT_EXPORT, CLAUDE_TRANSCRIPT, CODEX_THREAD, Recovered, parse_claude_chat_export,
    parse_claude_transcript, parse_codex_thread,
};

use crate::appserver::AppServer;
use crate::wire::Request;
use conbus_core::clock::Timestamp;
use conbus_core::event::{Body, Event, Harness, Source};
use conbus_core::ids::BusId;
use conbus_core::paths::Paths;
use conbus_core::record::{Entry, Record, SCHEMA_VERSION};
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
    /// The same, for live Codex threads, through the app-server.
    pub gap_codex_threads: u64,
    pub gap_codex_turns: u64,
    pub gap_codex_records: u64,
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
    let outcome = fill_gaps(paths, &transcripts, apply, true)?;

    println!("history   {}", transcripts.display());
    println!(
        "gaps      {} conversation(s), {} turn(s), {} record(s)",
        outcome.gap_sessions, outcome.gap_turns, outcome.gap_records
    );
    match &outcome.codex_unavailable {
        Some(why) => println!("codex     history unavailable — {why}"),
        None => println!(
            "codex     {} thread(s), {} turn(s), {} record(s)",
            outcome.gap_codex_threads, outcome.gap_codex_turns, outcome.gap_codex_records
        ),
    }
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
            let Some(bus) = mine.last().map(|record| record.bus_id) else {
                continue;
            };
            let missing = missing_from(mine, &sid, bus, recovered);
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
/// Per-part times of one start-up gap fill — the `prepare breakdown` line
/// carries them so the next restart investigation starts from evidence
/// rather than another guess.
#[derive(Default, Debug)]
pub struct GapTiming {
    /// Enumerating `~/.claude/projects`.
    pub claude_walk_ms: u64,
    /// Session files that passed the mtime filter and were read.
    pub claude_files: u64,
    /// Bytes those files brought in.
    pub claude_bytes: u64,
    pub claude_read_ms: u64,
    /// Statting `~/.codex` rollouts for the `since` cut.
    pub codex_walk_ms: u64,
    /// `codex app-server` initialize.
    pub codex_start_ms: u64,
    /// All `thread/list` pages together.
    pub thread_list_ms: u64,
    pub thread_list_pages: u64,
    /// Per-thread `list_turns` calls: how many threads were asked and how
    /// many answered before the budget or an error stopped the walk.
    pub list_turns_ms: u64,
    pub list_turns_threads: u64,
    pub list_turns_attempted: u64,
    /// First `list_turns` failure seen, so an always-failing call is visible
    /// rather than a silent zero.
    pub list_turns_err: Option<String>,
    pub chat_export_ms: u64,
    /// The start budget cut the fill short; the deferred range is held in
    /// `cache/gap-since` and resumed on the next start.
    pub budget_exhausted: bool,
    /// Why the Codex pass did not read everything it was asked to, when it did
    /// not. Its range is held in `cache/gap-since-codex` for the next start.
    pub codex_deferred: Option<String>,
    /// A `gap-since` marker that did not parse was ignored for this fill.
    pub gap_marker_garbled: bool,
    /// `sessions/` held files but none shaped like a rollout — the layout
    /// changed under the build; `thread/list` was the fallback.
    pub codex_layout_unknown: bool,
    /// Files under `sessions/` whose names were not rollout names.
    pub codex_unknown_files: u64,
}

/// How long a start may spend filling gaps before it stops and serves.
/// Overridable for tests; the point is that a wedged vendor call or a giant
/// transcript never holds the socket closed past a bound.
fn gap_budget() -> std::time::Duration {
    std::env::var("CONBUS_GAP_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(15))
}

fn elapsed_ms(at: std::time::Instant) -> u64 {
    at.elapsed().as_millis() as u64
}

pub fn append_gaps_at_start(
    paths: &Paths,
    held: &[Record],
    log: &mut SegmentLog,
) -> io::Result<(u64, GapTiming)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    append_gaps_at_start_in(paths, held, log, gap_budget(), home.as_deref())
}

fn append_gaps_at_start_in(
    paths: &Paths,
    held: &[Record],
    log: &mut SegmentLog,
    budget: std::time::Duration,
    home: Option<&Path>,
) -> io::Result<(u64, GapTiming)> {
    // Only what changed after the log's newest record can hold a turn that
    // happened while no daemon was listening. Reading every transcript the log
    // had ever bound took ten seconds with a warm cache and 71 after a reboot
    // on a machine with 6,190 of them (6.3 GB) — with the socket already
    // bound, so every hook in that window timed out. Earlier downtimes were
    // filled by earlier starts; `conbus backfill --gaps` still reads them all.
    let mut timing = GapTiming::default();
    let deadline = Instant::now() + budget;
    // A fill the budget cut short leaves `cache/gap-since` holding the
    // effective since it stopped at, so the next start resumes the range
    // rather than losing it to a newer `newest`. A fill that finishes
    // deletes the marker; a garbled one is ignored and said so.
    //
    // The Codex pass keeps its own marker, because it can fail for as long as
    // the app-server cannot start, and the transcripts must not pay for that: a
    // shared one would have every start read further back into them.
    let marker = paths.cache_dir().join("gap-since");
    let codex_marker = paths.cache_dir().join("gap-since-codex");
    let remembered = read_since(&marker);
    let codex_remembered = read_since(&codex_marker);
    if (marker.exists() && remembered.is_none())
        || (codex_marker.exists() && codex_remembered.is_none())
    {
        timing.gap_marker_garbled = true;
    }
    let since = newest_unix(held)
        .map(|newest| newest - GAP_MARGIN_SECS)
        .map(|fresh| remembered.map_or(fresh, |old| old.min(fresh)));
    let codex_since = since.map(|since| codex_remembered.map_or(since, |old| old.min(since)));
    let mut n = 0;
    if let Some(home) = home {
        let transcripts = home.join(".claude").join("projects");
        if transcripts.is_dir() {
            let walk = Instant::now();
            let files = session_files(&transcripts)?;
            timing.claude_walk_ms = elapsed_ms(walk);
            let read = Instant::now();
            n += append_gaps_files(
                &files,
                held,
                Some(&mut *log),
                since,
                &paths.spool_dir(),
                &mut Some(&mut timing),
                Some(deadline),
            )?
            .gap_records;
            timing.claude_read_ms = elapsed_ms(read);
        }
    }
    // Codex.app burns the same account and never hits CLI hooks. Official
    // thread/turns/list on threads we already bound fills those turns under
    // the live `bus_id` — not a second conversation.
    if timing.budget_exhausted {
        timing.codex_deferred = Some("the transcripts used the start's budget".to_string());
    } else {
        let server_limit = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(std::time::Duration::ZERO)
            .min(std::time::Duration::from_secs(8));
        n += fill_live_codex_gaps(
            held,
            Some(&mut *log),
            codex_since,
            &paths.spool_dir(),
            &mut timing,
            deadline,
            server_limit,
        )?
        .records;
    }
    if !timing.budget_exhausted {
        let export = Instant::now();
        n += ingest_chat_exports_since(&paths.chat_export(), held, log, since)?;
        timing.chat_export_ms = elapsed_ms(export);
    }
    if timing.budget_exhausted {
        // The cut-short range outlives this start: next start resumes it.
        if let Some(since) = since {
            write_since(paths, &marker, since);
        }
    } else {
        let _ = std::fs::remove_file(&marker);
    }
    match (&timing.codex_deferred, codex_since) {
        (None, _) => {
            let _ = std::fs::remove_file(&codex_marker);
        }
        (Some(_), Some(since)) => write_since(paths, &codex_marker, since),
        (Some(_), None) => {}
    }
    Ok((n, timing))
}

/// The time a marker under `cache/` holds, as unix seconds.
fn read_since(marker: &Path) -> Option<i64> {
    let when = fs::read_to_string(marker)
        .ok()?
        .trim()
        .parse::<Timestamp>()
        .ok()?;
    Some(when.seconds_since(Timestamp::from_unix_seconds(0)?))
}

/// Hold a deferred range in a marker, whole or not at all. It costs a repeat of
/// the range at worst, so a failure here is not worth more than that.
fn write_since(paths: &Paths, marker: &Path, since: i64) {
    let Some(when) = Timestamp::from_unix_seconds(since) else {
        return;
    };
    let temp = paths
        .cache_dir()
        .join(format!(".gap-since.{}.tmp", std::process::id()));
    let _ = fs::create_dir_all(paths.cache_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        if options
            .open(&temp)
            .and_then(|mut f| std::io::Write::write_all(&mut f, when.to_string().as_bytes()))
            .and_then(|_| fs::rename(&temp, marker))
            .is_err()
        {
            let _ = fs::remove_file(&temp);
        }
    }
}

/// Keep the range for the next start, when this one could not fill at all.
///
/// The log could not be read, so its newest record is not known. The newest
/// segment's write time is as good, and needs no read of the segment. Without
/// this the hooks served after this start would move the next start's range past
/// whatever this downtime missed.
pub fn defer_fill(paths: &Paths) {
    let Some(written) = SegmentLog::current_segment(&paths.log_dir())
        .ok()
        .and_then(|segment| modified_unix(&segment))
    else {
        return;
    };
    let marker = paths.cache_dir().join("gap-since");
    let fresh = written - GAP_MARGIN_SECS;
    write_since(
        paths,
        &marker,
        read_since(&marker).map_or(fresh, |old| old.min(fresh)),
    );
}

/// How long before the log's newest record a file may have last changed and
/// still hold a turn this daemon missed: vendors buffer their writes, and a
/// clock can step.
const GAP_MARGIN_SECS: i64 = 15 * 60;

/// The newest record's time, as unix seconds.
fn newest_unix(held: &[Record]) -> Option<i64> {
    let epoch = Timestamp::from_unix_seconds(0)?;
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

/// For each session about to be read, the records its turns could already be
/// in: the conversation a fill would write to, and any other conversation that
/// names the session, because a split leaves one session's turns under more
/// than one id. One pass, so a lookup per file is not a pass over the whole log
/// per file, and nothing is grouped for the sessions a start will not read.
fn records_of<'a, 's>(
    held: &'a [Record],
    targets: &[(&'s str, BusId)],
) -> HashMap<&'s str, Vec<&'a Record>> {
    let mut found: HashMap<&'s str, Vec<&'a Record>> = HashMap::new();
    let mut by_bus: HashMap<BusId, Vec<&'s str>> = HashMap::new();
    for &(session, bus) in targets {
        found.entry(session).or_default();
        by_bus.entry(bus).or_default().push(session);
    }
    if targets.is_empty() {
        return found;
    }
    for record in held {
        let named = record.source.vendor_session_id.as_deref();
        if let Some(records) = named.and_then(|name| found.get_mut(name)) {
            records.push(record);
        }
        for session in by_bus.get(&record.bus_id).into_iter().flatten() {
            if named != Some(*session)
                && let Some(records) = found.get_mut(session)
            {
                records.push(record);
            }
        }
    }
    found
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

/// What the Codex pass found, whether or not it wrote any of it.
#[derive(Default, Debug)]
struct CodexFill {
    threads: u64,
    turns: u64,
    records: u64,
    /// Threads the app-server said no about. Asking again gets the same answer,
    /// so these do not hold the range.
    refused: u64,
}

/// Official app-server only. Live Codex threads the recorder already bound.
///
/// `log` is `None` for a dry run, and `server_limit` is how long the app-server
/// may live: seconds at a start, which has a socket waiting, and minutes by hand.
/// A pass that could not read what it was asked to says why in
/// `timing.codex_deferred`, because the range it leaves behind is not covered by
/// any later start unless something holds it.
fn fill_live_codex_gaps(
    held: &[Record],
    mut log: Option<&mut SegmentLog>,
    since: Option<i64>,
    spool: &Path,
    timing: &mut GapTiming,
    deadline: Instant,
    server_limit: std::time::Duration,
) -> io::Result<CodexFill> {
    let mut fill = CodexFill::default();
    let known = known_from(held.iter());
    if known.live_codex.is_empty() {
        return Ok(fill);
    }
    // Rollout mtimes answer the same question `thread/list` does — the
    // app-server's `updatedAt` for a thread is its rollout file's mtime —
    // for a stat instead of seconds a page. A session with no rollout on
    // disk is out: the app-server cannot hydrate one it cannot find, so
    // `list_turns` would only refuse. When the directories cannot be walked
    // at all the ask falls back to `thread/list` as before.
    let walk = Instant::now();
    let mut unknown = 0;
    let mut recent = codex_threads_touched_since(&known.live_codex, since, &mut unknown);
    timing.codex_walk_ms = elapsed_ms(walk);
    timing.codex_unknown_files = unknown;
    timing.codex_layout_unknown = recent.is_none() && unknown > 0;
    if recent.as_ref().is_some_and(|recent| recent.is_empty()) {
        return Ok(fill);
    }
    // Restart must not wait out the 10-minute backfill budget: the socket
    // is already bound, so a wedged initialize leaves every hook as EAGAIN.
    // The gap budget is the tighter bound of the two.
    if server_limit.is_zero() {
        timing.budget_exhausted = true;
        timing.codex_deferred = Some("no time left in the start's budget".to_string());
        return Ok(fill);
    }
    let start = Instant::now();
    let mut server = match AppServer::start(server_limit) {
        Ok(server) => server,
        Err(err) => {
            timing.codex_deferred = Some(err.to_string());
            return Ok(fill);
        }
    };
    timing.codex_start_ms = elapsed_ms(start);
    // When the rollout walk could not answer, one `thread/list` pass says
    // which threads moved since the cutoff, so a start asks for the turns of
    // those rather than of every thread the log has ever bound. When the
    // list cannot be read either, every thread is asked.
    if recent.is_none() {
        let list = Instant::now();
        recent = since
            .and_then(|since| threads_updated_since(&mut server, since, timing))
            .map(|set| set.into_iter().collect());
        timing.thread_list_ms = elapsed_ms(list);
    }
    let sessions: Vec<BusId> = match recent {
        Some(asked) => asked,
        None => known.live_codex.iter().copied().collect(),
    };
    let names: Vec<(String, BusId)> = sessions
        .iter()
        .filter_map(|session| Some((session.to_string(), *known.live_codex_bus.get(session)?)))
        .collect();
    let targets: Vec<(&str, BusId)> = names
        .iter()
        .map(|(name, bus)| (name.as_str(), *bus))
        .collect();
    let grouped = records_of(held, &targets);
    let turns_at = Instant::now();
    for &(name, bus_id) in &targets {
        if Instant::now() >= deadline {
            timing.budget_exhausted = true;
            timing.codex_deferred = Some("the start's budget ran out".to_string());
            break;
        }
        timing.list_turns_attempted += 1;
        let turns = match list_turns(&mut server, name) {
            Ok(turns) => turns,
            Err(err) => {
                let why = err.to_string();
                if timing.list_turns_err.is_none() {
                    timing.list_turns_err = Some(why.chars().take(120).collect());
                }
                if crate::appserver::is_refusal(&err) {
                    fill.refused += 1;
                    continue;
                }
                // Nothing behind it will answer either.
                timing.codex_deferred = Some(why);
                break;
            }
        };
        timing.list_turns_threads += 1;
        let thread = serde_json::json!({"id": name, "sessionId": name});
        let recovered = parse_codex_thread(&thread, &turns);
        let mine = grouped.get(name).into_iter().flatten().copied();
        let missing = missing_from(mine, name, bus_id, recovered);
        let missing = not_on_its_way(missing, spool, name, bus_id);
        if !missing.is_empty() {
            fill.threads += 1;
            fill.turns += missing
                .iter()
                .filter(|entry| matches!(entry.event, Event::TurnStarted { .. }))
                .count() as u64;
        }
        for entry in missing {
            match log.as_deref_mut() {
                Some(log) => {
                    if log.append(entry)?.is_some() {
                        fill.records += 1;
                    }
                }
                None => fill.records += 1,
            }
        }
    }
    timing.list_turns_ms = elapsed_ms(turns_at);
    Ok(fill)
}

/// Threads `thread/list` says were updated at or after `since`, or `None`
/// when the list cannot be read to the end.
fn threads_updated_since(
    server: &mut AppServer,
    since: i64,
    timing: &mut GapTiming,
) -> Option<HashSet<BusId>> {
    let mut recent = HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut params = serde_json::json!({"limit": 100});
        if let Some(cursor) = &cursor {
            params["cursor"] = serde_json::json!(cursor);
        }
        let page = server.call("thread/list", params).ok()?;
        timing.thread_list_pages += 1;
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

/// The threads whose rollout file could hold turns newer than `since` —
/// `~/.codex/sessions/<y>/<m>/<d>/rollout-<stamp>-<thread-id>.jsonl`, plus
/// `archived_sessions/` whose `<id>.jsonl.zst` keeps the same tail. A thread
/// whose file is old is out; so is one with no file at all — the app-server
/// hydrates a thread from its rollout and answers "no rollout found" when
/// there is none, so asking could never return turns anyway (most sessions
/// in a long-lived log have no file on this disk). `None` when neither
/// directory could be walked, leaving the caller its `thread/list` fallback.
/// With no `since` every thread with a rollout is asked, which is what a fill by
/// hand wants.
fn codex_threads_touched_since(
    sessions: &HashSet<BusId>,
    since: Option<i64>,
    unknown: &mut u64,
) -> Option<Vec<BusId>> {
    let since = since.unwrap_or(i64::MIN);
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))?;
    codex_threads_touched_since_in(&home, sessions, since, unknown)
}

fn codex_threads_touched_since_in(
    codex_home: &Path,
    sessions: &HashSet<BusId>,
    since: i64,
    unknown: &mut u64,
) -> Option<Vec<BusId>> {
    let mut touched = HashMap::new();
    let sessions_dir = codex_home.join("sessions");
    for dir in [sessions_dir.clone(), codex_home.join("archived_sessions")] {
        collect_rollout_mtimes(&dir, 6, &mut touched, unknown);
    }
    // Files are here but none parse as a rollout — the vendor changed the
    // layout under the build, and a silent empty list would mean "no
    // threads". `thread/list` is the fallback that cannot miss them.
    if touched.is_empty() && *unknown > 0 {
        return None;
    }
    if touched.is_empty() && !sessions_dir.is_dir() {
        return None;
    }
    let mut asked: Vec<BusId> = sessions
        .iter()
        .filter(|session| touched.get(*session).is_some_and(|mtime| *mtime >= since))
        .copied()
        .collect();
    // Most recently touched first: a bounded start fills the threads most
    // likely to hold missed turns before older ones.
    asked.sort_by_key(|session| std::cmp::Reverse(touched[session]));
    Some(asked)
}

/// `rollout-<stamp>-<thread-id>.jsonl[.zst]` → the thread id: the last 36
/// characters of the stem, a uuid.
fn rollout_thread_id(name: &str) -> Option<BusId> {
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?;
    let stem = stem.strip_prefix("rollout-")?;
    stem.get(stem.len().saturating_sub(36)..)?.parse().ok()
}

/// Latest mtime per thread id under `dir`, recursing no deeper than `depth`
/// — `sessions/` nests three levels of date buckets, and the cap keeps a
/// pathological tree from stalling a start.
fn collect_rollout_mtimes(
    dir: &Path,
    depth: u8,
    touched: &mut HashMap<BusId, i64>,
    unknown: &mut u64,
) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_rollout_mtimes(&path, depth - 1, touched, unknown);
            continue;
        }
        let Some(id) = entry.file_name().to_str().and_then(rollout_thread_id) else {
            *unknown += 1;
            continue;
        };
        if let Some(mtime) = modified_unix(&path) {
            touched
                .entry(id)
                .and_modify(|seen| *seen = (*seen).max(mtime))
                .or_insert(mtime);
        }
    }
}

/// Read live sessions' official transcripts and, when `codex`, their Codex
/// threads, and when `apply` append the turns the log does not already hold.
/// Refuses to write while a daemon is listening — the daemon fills the same gaps
/// itself on restart. `codex` is a parameter for the reason it is one on
/// [`recover`].
pub fn fill_gaps(
    paths: &Paths,
    transcripts: &Path,
    apply: bool,
    codex: bool,
) -> io::Result<Outcome> {
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
    let mut outcome = append_gaps(transcripts, &held, log.as_mut(), None, &paths.spool_dir())?;
    if codex {
        let mut timing = GapTiming::default();
        let fill = fill_live_codex_gaps(
            &held,
            log.as_mut(),
            None,
            &paths.spool_dir(),
            &mut timing,
            Instant::now() + CODEX_DEADLINE,
            CODEX_DEADLINE,
        )?;
        outcome.gap_codex_threads = fill.threads;
        outcome.gap_codex_turns = fill.turns;
        outcome.gap_codex_records = fill.records;
        outcome.codex_unavailable = timing.codex_deferred;
        if fill.refused > 0 {
            outcome
                .unread
                .insert("thread/turns/list refused".into(), fill.refused);
        }
    }
    Ok(outcome)
}

/// Turns the official transcript has that the live log does not.
///
/// Reconstruction never outranks a live record: an already-seen turn stays
/// the live one. A turn that happened while the daemon was down is written
/// under the live `bus_id` so a restart does not fork the conversation.
///
/// The id decides, wherever there is one. The hook's `prompt_id` is the
/// transcript's `promptId`: of 7,791 real live prompts, 7,785 are in their
/// transcript under the same id and none under another, while the words differ
/// on 2,285 of those, so words are no key for what the hook saw. They stand in
/// only for a held prompt whose id explains no line of this source, one record
/// for one turn: one from before the hook carried an id, or a source that numbers
/// turns its own way, as a Chat export does. The same words with a new id, on a
/// prompt already explained, are a new turn.
///
/// `held` is every record this session's turns could already be in, which may
/// include another leg's records under the same conversation. Its ids can still
/// match, and its words are not this session's.
fn missing_from<'a>(
    held: impl IntoIterator<Item = &'a Record>,
    session: &str,
    bus_id: BusId,
    recovered: Recovered,
) -> Vec<Entry> {
    let listed: HashSet<&str> = recovered
        .entries
        .iter()
        .filter(|entry| matches!(entry.event, Event::TurnStarted { .. }))
        .filter_map(|entry| entry.vendor_turn_id.as_ref().map(|id| id.as_str()))
        .collect();
    let mut started = HashSet::new();
    let mut ended = HashSet::new();
    let mut unexplained: HashMap<String, u32> = HashMap::new();
    let mut said: HashMap<String, u32> = HashMap::new();
    for record in held {
        // A subagent's records are its own turn, under the parent's id.
        if record.vendor_agent_id.is_some() {
            continue;
        }
        let id = record.vendor_turn_id.as_ref().map(|id| id.as_str());
        match &record.event {
            Event::TurnEnded { .. } => ended.extend(id.map(str::to_string)),
            Event::TurnStarted { prompt, .. } => {
                started.extend(id.map(str::to_string));
                let Some(text) = prompt_text(prompt) else {
                    continue;
                };
                let answers_by_words = id.is_none_or(|id| {
                    !listed.contains(id)
                        && record.source.vendor_session_id.as_deref() == Some(session)
                });
                if answers_by_words {
                    *unexplained.entry(text.clone()).or_default() += 1;
                }
                *said.entry(text).or_default() += 1;
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    let mut keep_open = false;
    let mut turn: Option<String> = None;
    for mut entry in recovered.entries {
        match &entry.event {
            Event::SessionCandidate { .. } => continue,
            Event::TurnStarted { prompt, .. } => {
                let text = prompt_text(prompt);
                let id = entry.vendor_turn_id.as_ref().map(|id| id.as_str());
                let is_held = match id {
                    Some(id) => started.contains(id) || take_one(&mut unexplained, text),
                    None => take_one(&mut said, text),
                };
                keep_open = !is_held;
                turn = id.map(str::to_string);
            }
            // The hook reported this turn's end and missed its prompt.
            Event::TurnEnded { .. } if turn.as_ref().is_some_and(|id| ended.contains(id)) => {
                continue;
            }
            _ => {}
        }
        if !keep_open {
            continue;
        }
        entry.bus_id = bus_id;
        out.push(entry);
    }
    out
}

/// Leave out what the spool's drain is about to write.
///
/// A hook that found no daemon waits in the spool, and the drain runs after this
/// fill: the question is not whether the log holds a turn but whether it holds
/// it or is about to. The hook's record is the stronger fact, and it knows what
/// the transcript cannot, which is whether the vendor stopped.
///
/// Read only when something looks missing, and after the transcript that made it
/// look so: a hook that spooled before its line was written is seen here. One
/// that spools in the moment between the two reads is not, and is written by both.
fn not_on_its_way(missing: Vec<Entry>, spool: &Path, session: &str, bus_id: BusId) -> Vec<Entry> {
    if missing.is_empty() {
        return missing;
    }
    let waiting = spooled_records(spool, session, bus_id);
    if waiting.is_empty() {
        return missing;
    }
    let entries = Recovered {
        entries: missing,
        ..Recovered::default()
    };
    missing_from(&waiting, session, bus_id, entries)
}

/// What the spool holds for one session, as the records the drain will make of
/// it. An entry that does not read is the drain's to report, not this one's.
fn spooled_records(spool: &Path, session: &str, bus_id: BusId) -> Vec<Record> {
    let Ok(entries) = fs::read_dir(spool) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        // The drain's own rule: a regular file no larger than the hook's cap.
        .filter(|path| {
            fs::symlink_metadata(path)
                .is_ok_and(|meta| meta.file_type().is_file() && meta.len() <= 1_048_576)
        })
        .filter_map(|path| {
            let text = fs::read_to_string(path).ok()?;
            serde_json::from_str::<Request>(text.trim()).ok()
        })
        .filter(|request| request.vendor_session_id == session)
        .map(|request| Record {
            schema_version: SCHEMA_VERSION,
            seq: 0,
            ts: Timestamp::now(),
            bus_id,
            vendor_turn_id: request.vendor_turn_id,
            vendor_agent_id: request.vendor_agent_id,
            hook_id: request.hook_id,
            spooled_at: None,
            source: Source {
                harness: request.harness,
                harness_version: None,
                vendor_session_id: Some(request.vendor_session_id),
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            },
            event: request.event,
        })
        .collect()
}

/// Use up one held prompt with these words, if any is left.
fn take_one(words: &mut HashMap<String, u32>, text: Option<String>) -> bool {
    match text.and_then(|text| words.get_mut(&text)) {
        Some(left) if *left > 0 => {
            *left -= 1;
            true
        }
        _ => false,
    }
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
    log: Option<&mut SegmentLog>,
    since: Option<i64>,
    spool: &Path,
) -> io::Result<Outcome> {
    append_gaps_files(
        &session_files(transcripts)?,
        held,
        log,
        since,
        spool,
        &mut None,
        None,
    )
}

/// The per-file loop of `append_gaps`, lifted out of the walk so the start
/// path can time each half separately and stop when its budget runs out.
/// `timing` and `deadline` are the start path's; the manual `backfill` path
/// passes none of either — an explicit backfill has no socket to free.
fn append_gaps_files(
    files: &[PathBuf],
    held: &[Record],
    mut log: Option<&mut SegmentLog>,
    since: Option<i64>,
    spool: &Path,
    timing: &mut Option<&mut GapTiming>,
    deadline: Option<Instant>,
) -> io::Result<Outcome> {
    let known = known_from(held.iter());
    let mut outcome = Outcome::default();
    if !known.unreadable_live.is_empty() {
        outcome.unread.insert(
            "live session id is not a uuid".into(),
            known.unreadable_live.len() as u64,
        );
    }
    let to_read: Vec<(&PathBuf, &str, BusId)> = files
        .iter()
        .filter_map(|file| {
            let (stem, session) = session_of(file)?;
            let live_bus = *known.live_bus.get(&session)?;
            (!untouched_since(file, since)).then_some((file, stem, live_bus))
        })
        .collect();
    let targets: Vec<(&str, BusId)> = to_read.iter().map(|&(_, stem, bus)| (stem, bus)).collect();
    let grouped = records_of(held, &targets);
    for (file, stem, live_bus) in to_read {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if let Some(t) = timing.as_deref_mut() {
                t.budget_exhausted = true;
            }
            break;
        }
        let Ok(text) = fs::read_to_string(file) else {
            *outcome.unread.entry("unreadable file".into()).or_default() += 1;
            continue;
        };
        if let Some(t) = timing.as_deref_mut() {
            t.claude_files += 1;
            t.claude_bytes += text.len() as u64;
        }
        let recovered = parse_claude_transcript(&text, stem, live_bus);
        for (reason, count) in &recovered.skipped {
            *outcome.unread.entry(reason.clone()).or_default() += count;
        }
        let missing = missing_from(
            grouped.get(stem).into_iter().flatten().copied(),
            stem,
            live_bus,
            recovered,
        );
        let missing = not_on_its_way(missing, spool, stem, live_bus);
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
        let Some((stem, bus_id)) = session_of(&file) else {
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

        let mut recovered = parse_claude_transcript(&text, stem, bus_id);
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

/// The vendor session a transcript is named for: the name itself, and as a
/// UUID. The name is the file's identity, whatever its lines go on to claim.
fn session_of(file: &Path) -> Option<(&str, BusId)> {
    let stem = file.file_stem()?.to_str()?;
    Some((stem, stem.parse().ok()?))
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
                 \"version\":\"2.1.241\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{{\"type\":\"text\",\
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

    /// A session's transcript that opens on its parent's lines.
    fn transcript_of_a_fork(machine: &Machine, session: &str, parent: &str) {
        let project = machine.transcripts.join("-Applications-conbus");
        fs::create_dir_all(&project).expect("project dir");
        fs::write(
            project.join(format!("{session}.jsonl")),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"{parent}\",\"timestamp\":\"2026-08-30T09:33:02.222Z\",\
                 \"version\":\"2.1.241\",\"cwd\":\"/parent\",\"entrypoint\":\"cli\",\
                 \"promptId\":\"P0\",\"message\":{{\"role\":\"user\",\"content\":\"the parent's prompt\"}}}}\n\
                 {{\"type\":\"user\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:33:02.222Z\",\
                 \"version\":\"2.1.241\",\"cwd\":\"/Applications/conbus\",\"entrypoint\":\"cli\",\
                 \"promptId\":\"P1\",\"message\":{{\"role\":\"user\",\"content\":\"the fork's prompt\"}}}}\n"
            ),
        )
        .expect("write transcript");
    }

    #[test]
    fn recovering_a_fork_keeps_its_own_lines_under_its_own_session() {
        // The same parser serves `backfill --apply`, so a recovered fork must
        // not carry its parent's turns or its parent's session id either.
        let machine = machine();
        transcript_of_a_fork(&machine, SESSION, "11111111-aaaa-bbbb-cccc-ddddeeeeffff");

        let outcome = recover(&machine.paths, &machine.transcripts, true, false).expect("recover");

        assert_eq!(outcome.turns, 1);
        assert_eq!(outcome.unread.get("line of another session"), Some(&1));
        let records = held(&machine);
        assert!(!records.is_empty());
        assert!(records.iter().all(|record| {
            record.bus_id.to_string() == SESSION
                && record.source.vendor_session_id.as_deref() == Some(SESSION)
                && record.source.workspace.as_deref() == Some("/Applications/conbus")
        }));
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
                 \"version\":\"2.1.241\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{{\"type\":\"text\",\
                 \"text\":\"OK\"}}]}}}}\n\
                 {{\"type\":\"user\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:34:00.000Z\",\
                 \"version\":\"2.1.241\",\"cwd\":\"/Applications/conbus\",\"entrypoint\":\"cli\",\
                 \"promptId\":\"P2\",\"message\":{{\"role\":\"user\",\"content\":\"the turn the daemon missed\"}}}}\n\
                 {{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:34:01.000Z\",\
                 \"version\":\"2.1.241\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{{\"type\":\"text\",\
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

        let outcome = fill_gaps(&machine.paths, &machine.transcripts, true, false).expect("fill");

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

        let spool = machine.paths.spool_dir();
        let at_start =
            append_gaps(&machine.transcripts, &held, None, since, &spool).expect("start");
        let by_hand =
            append_gaps(&machine.transcripts, &held, None, None, &spool).expect("by hand");

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

        let at_start = append_gaps(
            &machine.transcripts,
            &held,
            None,
            since,
            &machine.paths.spool_dir(),
        )
        .expect("start");

        assert_eq!(at_start.gap_records, 2);
    }

    #[test]
    fn a_live_session_with_no_missing_turn_is_left_alone() {
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript(&machine, SESSION);

        let outcome = fill_gaps(&machine.paths, &machine.transcripts, true, false).expect("fill");

        assert_eq!(outcome.gap_sessions, 0);
        assert_eq!(records_on_disk(&machine), 1);
    }

    #[test]
    fn gap_apply_refuses_while_a_daemon_is_listening() {
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript_two_turns(&machine, SESSION);
        let _daemon = UnixListener::bind(machine.paths.socket()).expect("listen");

        let refused = fill_gaps(&machine.paths, &machine.transcripts, true, false);

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
        let missing = missing_from(mine, live_thread, bus, recovered);
        assert!(
            missing.iter().any(|e| e.bus_id == bus
                && matches!(&e.event, Event::TurnStarted { prompt, .. } if prompt_text(prompt).as_deref() == Some("from the app"))),
            "{missing:?}"
        );
    }

    fn live_prompt(
        session: &str,
        harness: &str,
        bus_id: BusId,
        turn: Option<&str>,
        text: &str,
    ) -> Entry {
        Entry {
            bus_id,
            observed_at: None,
            source: Source {
                harness: Harness::new(harness),
                harness_version: None,
                vendor_session_id: Some(session.into()),
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            },
            vendor_turn_id: turn.map(conbus_core::event::VendorTurnId::new),
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event: Event::TurnStarted {
                author: None,
                prompt: Body::kept(text),
            },
        }
    }

    /// Append `entries` and read the log back.
    fn logged(machine: &Machine, entries: Vec<Entry>) -> Vec<Record> {
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");
        for entry in entries {
            log.append(entry).expect("append");
        }
        held(machine)
    }

    /// One prompt per `(id, words)`, as a transcript of `SESSION` holds them.
    fn transcript_of(turns: &[(&str, &str)]) -> Recovered {
        let lines: Vec<String> = turns
            .iter()
            .map(|(id, words)| {
                serde_json::json!({
                    "type": "user", "sessionId": SESSION, "promptId": id,
                    "timestamp": "2026-08-31T09:33:02.222Z",
                    "message": {"role": "user", "content": words}
                })
                .to_string()
            })
            .collect();
        parse_claude_transcript(&lines.join("\n"), SESSION, BusId::mint())
    }

    fn turns_in(missing: &[Entry]) -> Vec<&str> {
        missing
            .iter()
            .filter_map(|entry| match &entry.event {
                Event::TurnStarted { .. } => entry.vendor_turn_id.as_ref().map(|id| id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn one_record_with_no_id_answers_for_one_turn_with_its_words() {
        // A live record from before the hook carried an id can only be matched
        // by its words, and only once: the same words again are another turn.
        let machine = machine();
        let bus = BusId::mint();
        let records = logged(
            &machine,
            vec![live_prompt(
                SESSION,
                Harness::CLAUDE_CODE,
                bus,
                None,
                "continue",
            )],
        );

        let missing = missing_from(
            &records,
            SESSION,
            bus,
            transcript_of(&[("p0", "continue"), ("p1", "continue")]),
        );

        assert_eq!(turns_in(&missing), vec!["p1"]);
    }

    #[test]
    fn a_turn_listed_under_another_id_is_not_written_again() {
        // Two sources can number one turn differently: a Chat hook's request id
        // against an export's message uuid. A held prompt whose id explains no
        // line of the source answers by its words, so this is not a second copy.
        let machine = machine();
        let bus = BusId::mint();
        let records = logged(
            &machine,
            vec![live_prompt(
                SESSION,
                Harness::CLAUDE_CODE,
                bus,
                Some("the hook's own id"),
                "said once",
            )],
        );

        let missing = missing_from(
            &records,
            SESSION,
            bus,
            transcript_of(&[("the source's id", "said once"), ("p2", "said once")]),
        );

        assert_eq!(turns_in(&missing), vec!["p2"], "one record, one turn");
    }

    #[test]
    fn a_prompt_an_id_explains_cannot_also_answer_by_its_words() {
        let machine = machine();
        let bus = BusId::mint();
        let records = logged(
            &machine,
            vec![live_prompt(
                SESSION,
                Harness::CLAUDE_CODE,
                bus,
                Some("p0"),
                "continue",
            )],
        );

        let missing = missing_from(
            &records,
            SESSION,
            bus,
            transcript_of(&[("p0", "continue"), ("p1", "continue")]),
        );

        assert_eq!(turns_in(&missing), vec!["p1"]);
    }

    #[test]
    fn the_same_words_on_another_leg_do_not_hide_a_missed_turn() {
        // A hop puts two vendor sessions under one conversation. What the other
        // leg was told is not a record of this leg's turn.
        let machine = machine();
        let bus = BusId::mint();
        let records = logged(
            &machine,
            vec![live_prompt(
                "another-leg",
                Harness::CODEX,
                bus,
                Some("its-turn"),
                "continue",
            )],
        );

        let missing = missing_from(&records, SESSION, bus, transcript_of(&[("p1", "continue")]));

        assert_eq!(turns_in(&missing), vec!["p1"]);
    }

    #[test]
    fn a_line_with_no_id_is_matched_by_its_words_against_any_prompt() {
        let machine = machine();
        let bus = BusId::mint();
        let records = logged(
            &machine,
            vec![live_prompt(
                SESSION,
                Harness::CLAUDE_CODE,
                bus,
                Some("p0"),
                "said once",
            )],
        );
        let recovered = parse_claude_transcript(
            &serde_json::json!({"type": "user", "sessionId": SESSION,
                "message": {"role": "user", "content": "said once"}})
            .to_string(),
            SESSION,
            BusId::mint(),
        );

        assert!(missing_from(&records, SESSION, bus, recovered).is_empty());
    }

    #[test]
    fn a_chat_turn_seen_live_is_not_copied_from_an_export_and_a_second_saying_is() {
        let machine = machine();
        let chat = "00000000-1111-2222-3333-444444444444";
        let bus: BusId = chat.parse().expect("uuid");
        let records = logged(
            &machine,
            vec![live_prompt(
                chat,
                Harness::CLAUDE_CHAT,
                bus,
                Some("request-1"),
                "hello",
            )],
        );
        let export = r#"{"conversations": [{
            "uuid": "00000000-1111-2222-3333-444444444444",
            "chat_messages": [
                {"uuid": "m1", "sender": "human", "text": "hello", "created_at": "2026-09-06T12:00:00.000Z"},
                {"uuid": "m2", "sender": "assistant", "text": "hi", "created_at": "2026-09-06T12:01:00.000Z"},
                {"uuid": "m3", "sender": "human", "text": "hello", "created_at": "2026-09-06T12:02:00.000Z"},
                {"uuid": "m4", "sender": "assistant", "text": "hi again", "created_at": "2026-09-06T12:03:00.000Z"}
            ]
        }]}"#;
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");

        let outcome = ingest_chat_text(&records, export, Some(&mut log)).expect("ingest");

        assert_eq!(outcome.gap_turns, 1, "the second saying only");
        assert_eq!(outcome.gap_records, 2);
    }

    /// A request waiting in the spool, as the hook writes it.
    fn in_the_spool(machine: &Machine, name: &str, session: &str, turn: &str, words: &str) {
        fs::create_dir_all(machine.paths.spool_dir()).expect("spool");
        fs::write(
            machine.paths.spool_dir().join(format!("{name}.json")),
            serde_json::json!({
                "harness": "claude_code", "vendor_session_id": session, "vendor_turn_id": turn,
                "hid": name, "spooled": "2026-11-01T00:00:00.000Z",
                "event": {"event": "turn_started", "prompt": {"kind": "kept", "text": words}}
            })
            .to_string(),
        )
        .expect("write");
    }

    #[test]
    fn backfill_gaps_by_hand_leaves_a_spooled_turn_to_the_drain() {
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript_two_turns(&machine, SESSION);
        in_the_spool(&machine, "a", SESSION, "P2", "the turn the daemon missed");

        let outcome = fill_gaps(&machine.paths, &machine.transcripts, true, false).expect("fill");

        assert_eq!(outcome.gap_records, 0);
        assert_eq!(records_on_disk(&machine), 1);
    }

    #[test]
    fn another_sessions_spool_entry_does_not_stand_in_for_this_turn() {
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript_two_turns(&machine, SESSION);
        in_the_spool(
            &machine,
            "a",
            "11111111-aaaa-bbbb-cccc-ddddeeeeffff",
            "P2",
            "the turn the daemon missed",
        );

        let outcome = fill_gaps(&machine.paths, &machine.transcripts, true, false).expect("fill");

        assert_eq!(outcome.gap_records, 2);
    }

    #[test]
    fn a_link_in_the_spool_is_not_followed_by_the_fill() {
        // Only what the drain would read counts as on its way.
        let machine = machine();
        recorded_live(&machine, SESSION);
        transcript_two_turns(&machine, SESSION);
        in_the_spool(
            &machine,
            "real",
            SESSION,
            "P2",
            "the turn the daemon missed",
        );
        let spool = machine.paths.spool_dir();
        let elsewhere = machine.paths.root().join("elsewhere.json");
        fs::rename(spool.join("real.json"), &elsewhere).expect("move");
        std::os::unix::fs::symlink(&elsewhere, spool.join("real.json")).expect("link");

        let outcome = fill_gaps(&machine.paths, &machine.transcripts, true, false).expect("fill");

        assert_eq!(outcome.gap_records, 2);
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

    /// A rollout file with the modification time `stamp` (unix seconds).
    fn rollout(dir: &Path, name: &str, stamp: u64) {
        fs::create_dir_all(dir).expect("dir");
        let file = fs::File::create(dir.join(name)).expect("rollout");
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(stamp))
            .expect("mtime");
    }

    #[test]
    fn a_rollout_name_yields_its_thread_id() {
        let id: BusId = SESSION.parse().expect("uuid");
        assert_eq!(
            rollout_thread_id(&format!("rollout-2026-11-01T13-45-52-{SESSION}.jsonl")),
            Some(id)
        );
        assert_eq!(
            rollout_thread_id(&format!("rollout-2026-03-21T00-15-32-{SESSION}.jsonl.zst")),
            Some(id)
        );
        assert_eq!(rollout_thread_id("rollout-not-a-thread.jsonl"), None);
        assert_eq!(rollout_thread_id("notes.txt"), None);
    }

    #[test]
    fn only_codex_threads_touched_since_the_cutoff_are_asked() {
        let home = tempfile::tempdir().expect("tempdir");
        let old: BusId = SESSION.parse().expect("uuid");
        let fresh: BusId = "00000000-aaaa-bbbb-cccc-ddddeeeeffff"
            .parse()
            .expect("uuid");
        let gone: BusId = "11111111-aaaa-bbbb-cccc-ddddeeeeffff"
            .parse()
            .expect("uuid");
        let sessions = home
            .path()
            .join("sessions")
            .join("2026")
            .join("11")
            .join("01");
        rollout(
            &sessions,
            &format!("rollout-2026-11-01T01-00-00-{old}.jsonl"),
            1_000,
        );
        rollout(
            &sessions,
            &format!("rollout-2026-11-01T13-00-00-{fresh}.jsonl"),
            2_000,
        );
        // The archived store answers the same way, compressed.
        rollout(
            &home.path().join("archived_sessions"),
            &format!("rollout-2026-03-21T00-15-32-{gone}.jsonl.zst"),
            3_000,
        );
        // `gone` names its file through the archived name rather than
        // sessions/: the map still carries the thread id.
        let mut bound = HashSet::from([old, fresh, gone]);
        let mut unknown = 0;
        let asked = codex_threads_touched_since_in(home.path(), &bound, 1_500, &mut unknown)
            .expect("sessions dir read");
        // Asked about in most-recently-touched order.
        assert_eq!(asked, vec![gone, fresh]);
        // A thread with no file anywhere cannot be hydrated by the
        // app-server — it is out rather than asked.
        bound.insert(BusId::mint());
        let asked = codex_threads_touched_since_in(home.path(), &bound, 1_500, &mut unknown)
            .expect("sessions dir read");
        assert_eq!(asked, vec![gone, fresh]);
    }

    /// A transcript the log missed while `home/.claude/projects` holds it.
    fn claude_transcript(home: &Path, session: &str) {
        let project = home
            .join(".claude")
            .join("projects")
            .join("-Applications-conbus");
        fs::create_dir_all(&project).expect("project dir");
        fs::write(
            project.join(format!("{session}.jsonl")),
            format!(
                "{{\"type\":\"user\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:33:02.222Z\",\
                 \"version\":\"2.1.241\",\"cwd\":\"/Applications/conbus\",\"entrypoint\":\"cli\",\
                 \"promptId\":\"P1\",\"message\":{{\"role\":\"user\",\"content\":\"Reply with OK\"}}}}\n\
                 {{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-08-31T09:33:03.000Z\",\
                 \"version\":\"2.1.241\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{{\"type\":\"text\",\
                 \"text\":\"OK\"}}]}}}}\n"
            ),
        )
        .expect("write transcript");
    }

    /// A live record at `when`, so the start fill has a `newest` to hang
    /// `since` on.
    fn live_at(machine: &Machine, when: conbus_core::clock::Timestamp, session: &str) {
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");
        log.append(Entry {
            bus_id: BusId::mint(),
            observed_at: Some(when),
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
                prompt: Body::kept("a live prompt the transcript does not hold"),
            },
        })
        .expect("append");
    }

    fn held(machine: &Machine) -> Vec<Record> {
        SegmentLog::scan(machine.paths.log_dir())
            .expect("scan")
            .records
    }

    #[test]
    fn a_fill_cut_short_resumes_its_range_on_the_next_start() {
        let machine = machine();
        let home = tempfile::tempdir().expect("tempdir");
        claude_transcript(home.path(), SESSION);
        let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
        let now = conbus_core::clock::Timestamp::now().seconds_since(epoch);
        // The log's newest record is around now: the first start's effective
        // since (now - margin) includes the transcript by mtime.
        live_at(
            &machine,
            conbus_core::clock::Timestamp::from_unix_seconds(now).expect("ts"),
            SESSION,
        );
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");
        let held_now = held(&machine);
        // A zero budget cuts the fill before the first file: nothing is
        // appended, and the marker holds the range that was skipped.
        let (appended, timing) = append_gaps_at_start_in(
            &machine.paths,
            &held_now,
            &mut log,
            std::time::Duration::ZERO,
            Some(home.path()),
        )
        .expect("fill");
        assert!(timing.budget_exhausted, "a zero budget must exhaust");
        assert_eq!(appended, 0);
        let marker = machine.paths.cache_dir().join("gap-since");
        assert!(marker.exists(), "the deferred range is remembered");
        // Time passes: a new live record makes the fresh since (newest -
        // margin) later than the transcript's mtime — without the marker the
        // skipped file is filtered out forever.
        live_at(
            &machine,
            conbus_core::clock::Timestamp::from_unix_seconds(now + GAP_MARGIN_SECS + 3600)
                .expect("ts"),
            "live-b",
        );
        let held_later = held(&machine);
        let (appended, timing) = append_gaps_at_start_in(
            &machine.paths,
            &held_later,
            &mut log,
            std::time::Duration::from_secs(60),
            Some(home.path()),
        )
        .expect("resume fill");
        assert!(appended > 0, "the deferred range is resumed");
        assert!(!timing.budget_exhausted);
        assert!(!marker.exists(), "a finished fill clears the marker");
        let after = records_on_disk(&machine);
        // A third fill has nothing left: the deferred turns landed exactly
        // once.
        let held_third = held(&machine);
        let (third, _) = append_gaps_at_start_in(
            &machine.paths,
            &held_third,
            &mut log,
            std::time::Duration::from_secs(60),
            Some(home.path()),
        )
        .expect("third fill");
        assert_eq!(third, 0, "resumed turns are appended exactly once");
        assert_eq!(records_on_disk(&machine), after);
    }

    /// Set a file's modification time, in unix seconds.
    fn touched_at(file: &Path, seconds: i64) {
        fs::File::options()
            .write(true)
            .open(file)
            .expect("open")
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64))
            .expect("mtime");
    }

    #[test]
    fn a_fill_skipped_for_a_read_error_keeps_its_range() {
        // The log could not be read, so nothing was compared and nothing filled.
        // Newer records must not then push the missed range out of reach.
        let machine = machine();
        let home = tempfile::tempdir().expect("tempdir");
        claude_transcript(home.path(), SESSION);
        let now = conbus_core::clock::Timestamp::now();
        live_at(&machine, now, SESSION);

        defer_fill(&machine.paths);

        let marker = machine.paths.cache_dir().join("gap-since");
        assert!(marker.exists(), "the skipped range is remembered");
        let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
        let later = now.seconds_since(epoch) + GAP_MARGIN_SECS + 3600;
        live_at(
            &machine,
            conbus_core::clock::Timestamp::from_unix_seconds(later).expect("ts"),
            "live-b",
        );
        let mut log =
            SegmentLog::open(machine.paths.log_dir(), Retention::default()).expect("open");
        let (appended, _) = append_gaps_at_start_in(
            &machine.paths,
            &held(&machine),
            &mut log,
            std::time::Duration::from_secs(60),
            Some(home.path()),
        )
        .expect("the next start");

        assert!(appended > 0, "the range is filled once the log can be read");
        assert!(!marker.exists());
    }

    #[test]
    fn a_read_error_does_not_move_a_range_already_deferred_forward() {
        let machine = machine();
        live_at(&machine, conbus_core::clock::Timestamp::now(), SESSION);
        fs::create_dir_all(machine.paths.cache_dir()).expect("cache");
        let marker = machine.paths.cache_dir().join("gap-since");
        fs::write(&marker, "2026-10-01T00:00:00.000Z").expect("marker");

        defer_fill(&machine.paths);

        assert_eq!(
            fs::read_to_string(&marker).expect("marker"),
            "2026-10-01T00:00:00.000Z"
        );
    }

    #[test]
    fn a_read_error_defers_from_the_newest_segments_write_time() {
        let machine = machine();
        live_at(&machine, conbus_core::clock::Timestamp::now(), SESSION);
        let segment = SegmentLog::current_segment(&machine.paths.log_dir()).expect("segment");
        touched_at(&segment, 1_800_000_000);

        defer_fill(&machine.paths);

        let marker = machine.paths.cache_dir().join("gap-since");
        let written: conbus_core::clock::Timestamp = fs::read_to_string(&marker)
            .expect("marker")
            .parse()
            .expect("a time");
        let epoch = conbus_core::clock::Timestamp::from_unix_seconds(0).expect("epoch");
        assert_eq!(
            written.seconds_since(epoch),
            1_800_000_000 - GAP_MARGIN_SECS,
            "the same margin the fill keeps"
        );
    }

    #[test]
    fn a_codex_layout_with_no_rollouts_falls_back_to_the_rpc() {
        let home = tempfile::tempdir().expect("tempdir");
        let bound = HashSet::from([SESSION.parse::<BusId>().expect("uuid")]);
        // Files are present but none are rollout names: the layout changed
        // under the build, so an empty list would be a silent stop.
        let dir = home
            .path()
            .join("sessions")
            .join("2026")
            .join("11")
            .join("01");
        fs::create_dir_all(&dir).expect("dirs");
        fs::write(dir.join("foo.jsonl"), "{}\n").expect("file");
        let mut unknown = 0;
        assert_eq!(
            codex_threads_touched_since_in(home.path(), &bound, 1_500, &mut unknown),
            None,
            "unrecognised files mean the walk cannot be trusted"
        );
        assert_eq!(unknown, 1);
        // An empty sessions/ still means "no threads".
        let empty = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(empty.path().join("sessions")).expect("dir");
        let mut unknown = 0;
        assert_eq!(
            codex_threads_touched_since_in(empty.path(), &bound, 1_500, &mut unknown),
            Some(vec![])
        );
    }

    #[test]
    fn an_unwalkable_codex_home_falls_back_to_the_rpc() {
        let home = tempfile::tempdir().expect("tempdir");
        let bound = HashSet::from([SESSION.parse::<BusId>().expect("uuid")]);
        assert_eq!(
            codex_threads_touched_since_in(home.path(), &bound, 1_500, &mut 0),
            None
        );
    }
}
