//! The recorder daemon: one socket, one registry, one append-only log.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use conbus_core::clock::Timestamp;
use conbus_core::continuation::{
    Asking, Decision, Stopped, decide, machine_authored, opening_units, stopped, unattended,
};
use conbus_core::event::{Event, Harness, Hop, HopProvenance, Source, TurnState};
use conbus_core::identity;
use conbus_core::ids::BusId;
use conbus_core::pack::{self, Chapter};
use conbus_core::paths::Paths;
use conbus_core::record::{Entry, Record};
use conbus_core::registry::{Disposition, Observed, Registry, SessionKey};
use conbus_core::retention::Retention;
use conbus_core::segment::SegmentLog;

use crate::harden;
use crate::wire::{Request, Response};

/// Derived state plus the log it feeds. Lives in one process, in one thread, so
/// the registry needs no lock: hook clients are short-lived and a handful of
/// events per turn does not need concurrency.
pub struct Daemon {
    registry: Registry,
    log: SegmentLog,
    /// One entry per conversation, folded as records are written.
    ///
    /// Kept in memory rather than read back per prompt. The briefing decision
    /// runs on `UserPromptSubmit`, which is the hook standing between a person
    /// pressing enter and anything happening, and re-reading a log that only
    /// grows would put a visible and worsening delay there. A feature meant to
    /// be invisible cannot be the thing that makes typing feel slow.
    chapters: HashMap<BusId, Chapter>,
    /// The home directory, so a session started from it is not treated as a
    /// checkout containing every conversation on the machine.
    home: Option<String>,
    /// A quota hop that still needs its Codex warm thread. Set on the hook
    /// path, taken after the reply goes out — `thread/start` is too slow to
    /// run while the vendor is waiting.
    pending_warm: Option<BusId>,
    /// A chat order naming another catalog leg. Same spawn as a quota hop,
    /// with `--to` so the destination is the one they said.
    pending_directed: Option<(BusId, crate::intent::Kind, String)>,
    /// Same-leg resume from chat ("커서로 재개"). Open that window after
    /// the hook reply; do not record a second hop.
    pending_enter: Option<(String, Option<String>)>,
    /// Conbus root, so a briefing can load linked facts. Tests leave this
    /// unset so they do not read the machine's arena files.
    arena_root: Option<std::path::PathBuf>,
    /// Official Chat export drop folder. `None` in tests that do not watch.
    chat_watch: Option<ChatWatch>,
    /// The last 4,096 hook ids written, in arrival order — a spooled request
    /// that already landed is answered Recorded, not written twice.
    seen_hooks: std::collections::VecDeque<String>,
    seen_hook_bus: std::collections::HashMap<String, conbus_core::ids::BusId>,
}

/// One directory the daemon polls for Chat exports.
struct ChatWatchDir {
    dir: PathBuf,
    official_only: bool,
    signature: Vec<(String, u64, u64)>,
}

/// Watch official Chat export drops without a log scan on every connection.
struct ChatWatch {
    dirs: Vec<ChatWatchDir>,
    log_dir: PathBuf,
}

impl Daemon {
    /// Build a daemon around a log and the bindings it already has.
    ///
    /// The registry is a parameter rather than a fresh `Registry::new()`
    /// because a daemon that starts with empty bindings forks every live
    /// conversation: the vendor session keeps its id, the bus forgets which
    /// conversation it was, and its next prompt mints a second `bus_id`.
    /// `serve` restores it from the segments.
    pub fn new(log: SegmentLog, registry: Registry, chapters: HashMap<BusId, Chapter>) -> Self {
        Daemon {
            registry,
            log,
            chapters,
            home: std::env::var("HOME").ok(),
            pending_warm: None,
            pending_directed: None,
            pending_enter: None,
            arena_root: None,
            chat_watch: None,
            seen_hooks: std::collections::VecDeque::new(),
            seen_hook_bus: std::collections::HashMap::new(),
        }
    }

    /// Remember that this hook id already wrote, under this bus — the last
    /// 4,096. A drained spool re-sending it gets Recorded, not a second
    /// record.
    fn remember_hook(&mut self, hook_id: String, bus_id: conbus_core::ids::BusId) {
        self.seen_hook_bus.insert(hook_id.clone(), bus_id);
        self.seen_hooks.push_back(hook_id);
        while self.seen_hooks.len() > 4096 {
            if let Some(old) = self.seen_hooks.pop_front() {
                self.seen_hook_bus.remove(&old);
            }
        }
    }

    pub fn with_arena(mut self, root: std::path::PathBuf) -> Self {
        self.arena_root = Some(root);
        self
    }

    /// Ingest Chat exports that land after start. Signature is the files
    /// already consumed by `append_gaps_at_start`.
    pub fn watch_chat_exports(self, paths: &Paths) -> Self {
        // Do not readdir ~/Downloads here. On this machine opendir of that
        // folder blocks in the kernel and the socket is already bound, so
        // every hook becomes EAGAIN until it returns.
        self.watch_chat_export_sources(paths, None)
    }

    fn watch_chat_export_sources(mut self, paths: &Paths, downloads: Option<PathBuf>) -> Self {
        let export_dir = paths.chat_export();
        let _ = fs::create_dir_all(&export_dir);
        let mut dirs = vec![ChatWatchDir {
            signature: crate::backfill::chat_export_signature(&export_dir),
            dir: export_dir,
            official_only: false,
        }];
        if let Some(downloads) = downloads {
            dirs.push(ChatWatchDir {
                signature: crate::backfill::chat_export_signature_filtered(&downloads, true),
                dir: downloads,
                official_only: true,
            });
        }
        self.chat_watch = Some(ChatWatch {
            dirs,
            log_dir: paths.log_dir(),
        });
        self
    }

    /// Append new Chat-export turns and rebuild derived state when a watched
    /// folder changed. No-op when every signature is unchanged.
    pub fn absorb_chat_exports(&mut self) -> io::Result<u64> {
        let Some(watch) = self.chat_watch.as_ref() else {
            return Ok(0);
        };
        let mut changed = Vec::new();
        for (i, dir) in watch.dirs.iter().enumerate() {
            let signature =
                crate::backfill::chat_export_signature_filtered(&dir.dir, dir.official_only);
            if signature != dir.signature {
                changed.push((i, dir.dir.clone(), dir.official_only, signature));
            }
        }
        if changed.is_empty() {
            return Ok(0);
        }
        let log_dir = watch.log_dir.clone();
        let cache_dir = log_dir.parent().map(|root| root.join("cache"));
        let mut held = SegmentLog::scan_meta(&log_dir, cache_dir.as_deref())?.records;
        let mut n = 0u64;
        for (_i, dir, official_only, _signature) in &changed {
            n += if *official_only {
                crate::backfill::ingest_official_privacy_exports(dir, &held, &mut self.log)?
            } else {
                crate::backfill::ingest_chat_exports(dir, &held, &mut self.log)?
            };
            if n > 0 {
                held = SegmentLog::scan_meta(&log_dir, cache_dir.as_deref())?.records;
            }
        }
        if n > 0 {
            if let Ok(scan) = SegmentLog::scan_meta(&log_dir, cache_dir.as_deref()) {
                self.registry = Registry::restore(&scan.records);
                self.chapters = Self::chapters(&scan.records);
            }
            note!("filled {n} chat export record(s)");
        }
        if let Some(watch) = self.chat_watch.as_mut() {
            for (i, _dir, _official_only, signature) in changed {
                watch.dirs[i].signature = signature;
            }
        }
        Ok(n)
    }

    /// Fold the log into the per-conversation state the briefing reads.
    ///
    /// Derived, so it is rebuilt at start for the same reason the registry's
    /// bindings are: a daemon that woke up knowing nothing would tell the next
    /// session that its checkout had never been worked in.
    pub fn chapters(records: &[Record]) -> HashMap<BusId, Chapter> {
        let mut chapters = HashMap::new();
        for record in records {
            Chapter::apply(&mut chapters, record);
        }
        chapters
    }

    /// Record one observed event.
    pub fn handle(&mut self, request: Request) -> Response {
        // A newer hook may carry envelope fields this build does not know, and
        // dropping them silently is exactly the class of bug this refuses to
        // repeat. Upgrading conbus leaves the old daemon running until someone
        // restarts it, so this is an ordinary situation, not an exotic one.
        if request
            .protocol_version
            .is_some_and(|v| v > crate::wire::PROTOCOL_VERSION)
        {
            // The daemon's own stderr, like every other refusal here: it is the
            // channel launchd captures, and the hook's is swallowed.
            note!(
                "refusing a hook speaking protocol v{}; this daemon speaks v{}",
                request.protocol_version.unwrap_or_default(),
                crate::wire::PROTOCOL_VERSION
            );
            return Response::Failed {
                error: format!(
                    "this daemon speaks protocol v{}; the hook speaks v{}. Restart the daemon.",
                    crate::wire::PROTOCOL_VERSION,
                    request.protocol_version.unwrap_or_default()
                ),
            };
        }

        // The hook refuses these before they are sent; this is the second
        // line, for a client that does not. One registry key shared by every
        // session that names none is how two strangers become one
        // conversation (`hook::NO_SESSION`).
        if request.vendor_session_id.trim().is_empty() {
            return Response::Failed {
                error: crate::hook::NO_SESSION.to_string(),
            };
        }

        // A re-sent request — one the spool delivered after the live copy
        // already landed — is answered as written, not written twice.
        if let Some(hook_id) = request.hook_id.as_ref()
            && let Some(bus_id) = self.seen_hook_bus.get(hook_id)
        {
            return Response::Recorded {
                bus_id: *bus_id,
                seqs: Vec::new(),
            };
        }
        let hook_id = request.hook_id.clone();
        let spooled_at = request
            .spooled_at
            .as_deref()
            .and_then(|s| s.parse::<Timestamp>().ok());
        let brief = request.brief;
        let declared = request.provenance;
        let key = SessionKey::new(request.harness.clone(), request.vendor_session_id);
        let observed = Observed {
            vendor_turn_id: request.vendor_turn_id,
            vendor_agent_id: request.vendor_agent_id,
            workspace: request.workspace.clone(),
            event: request.event,
        };
        match self.registry.observe(&key, observed) {
            Disposition::Deferred => {
                self.maybe_brief_candidate(brief, &request.harness, request.workspace.as_deref())
            }
            Disposition::Orphaned => Response::Orphaned,
            Disposition::Write { bus_id, observed } => {
                let source = conbus_core::event::Source {
                    harness: request.harness,
                    harness_version: request.harness_version,
                    vendor_session_id: Some(key.vendor_session_id.clone()),
                    // One per request, not per observation: the front end a
                    // session runs under does not change while it runs, and
                    // the hook reads it from its own environment rather than
                    // from the payload.
                    entrypoint: request.entrypoint.clone(),
                    // Filled per observation below: a flushed session start
                    // carried its own, and a checkout can change between the
                    // start and the prompt that proves the session real.
                    workspace: None,
                    backfilled_from: None,
                    // Same shape as `entrypoint`: the profile is a property of
                    // the session, read from the hook child's own env.
                    profile: request.profile.clone(),
                };
                // A conversation nothing has ever been folded for is one
                // whose first prompt this is — the registry mints on exactly
                // that event, so the two agree without either asking the other.
                let first_prompt = !self.chapters.contains_key(&bus_id);
                let mut seqs = Vec::with_capacity(observed.len());
                let mut written: Vec<Record> = Vec::with_capacity(observed.len());
                let mut errors = Vec::new();
                // Each observation keeps the correlators its own payload had.
                // A deferred session start carried none; borrowing the minting
                // prompt's would fabricate a correlation the vendor did not send.
                let last = observed.len().saturating_sub(1);
                for (index, one) in observed.into_iter().enumerate() {
                    // The hook id rides on the request's own event, not on a
                    // session start flushed before it — that is a different
                    // fact, and stamping it with the same id would have the
                    // fold count it as a re-delivery.
                    let is_own = index == last;
                    let entry = Entry {
                        bus_id,
                        observed_at: None,
                        source: Source {
                            workspace: one.workspace,
                            ..source.clone()
                        },
                        vendor_turn_id: one.vendor_turn_id,
                        vendor_agent_id: one.vendor_agent_id,
                        hook_id: is_own.then(|| hook_id.clone()).flatten(),
                        spooled_at: if is_own { spooled_at } else { None },
                        event: one.event,
                    };
                    match self.log.append(entry) {
                        Ok(Some(record)) => {
                            seqs.push(record.seq);
                            written.push(record);
                        }
                        Ok(None) => {}
                        // Keep going. Returning here would abandon the events
                        // behind this one untried and report two losses as one,
                        // and the prompt that minted the conversation is often
                        // the one behind a flushed session start.
                        Err(err) => errors.push(err.to_string()),
                    }
                }
                if !errors.is_empty() {
                    // The daemon's own stderr, because the hook's goes to a
                    // vendor that swallows it.
                    note!(
                        "{} of {} record(s) refused by the log: {}",
                        errors.len(),
                        errors.len() + seqs.len(),
                        errors.join("; ")
                    );
                    return Response::Failed {
                        error: format!(
                            "{} record(s) written, {} lost: {}",
                            seqs.len(),
                            errors.len(),
                            errors.join("; ")
                        ),
                    };
                }
                if seqs.is_empty() {
                    return Response::Withheld;
                }
                // Written once means a re-send is answered, never rewritten.
                if let Some(hook_id) = &hook_id {
                    self.remember_hook(hook_id.clone(), bus_id);
                }
                // Not after `/clear`: the person just said "forget it", and
                // the vendor names that moment on the session start flushed
                // behind this prompt. And not when the person switched it off.
                let cleared = session_was_cleared(&written);
                let briefing = (first_prompt && brief && !cleared)
                    .then(|| self.brief(bus_id, &written))
                    .flatten();
                match briefing {
                    Some((context, message, event)) => {
                        // The briefing knows where it happened, like every
                        // other record: the workspace is the prompt's.
                        let workspace = written
                            .iter()
                            .find(|record| matches!(record.event, Event::TurnStarted { .. }))
                            .and_then(|record| record.source.workspace.clone());
                        // Appended after the turn it belongs to, so the log
                        // reads in the order things happened. If this write
                        // fails the briefing still goes out — the person is
                        // better served by the context than by our bookkeeping
                        // — and the daemon says so on the channel launchd
                        // keeps.
                        match self.log.append(Entry {
                            bus_id,
                            observed_at: None,
                            source: Source {
                                workspace,
                                ..source.clone()
                            },
                            vendor_turn_id: None,
                            vendor_agent_id: None,
                            hook_id: None,
                            spooled_at: None,
                            event,
                        }) {
                            Ok(Some(record)) => {
                                seqs.push(record.seq);
                                written.push(record);
                            }
                            Ok(None) => {}
                            Err(err) => {
                                note!("briefing sent but not recorded: {err}")
                            }
                        }
                        self.maybe_continue(bus_id, &source, &mut written, &mut seqs);
                        self.remember(&written);
                        self.follow_through(bus_id, &source, &mut written, &mut seqs, declared);
                        self.finish_write(
                            Response::Briefed {
                                bus_id,
                                seqs,
                                context,
                                message,
                            },
                            bus_id,
                            &source,
                            &written,
                        )
                    }
                    None => {
                        self.remember(&written);
                        self.follow_through(bus_id, &source, &mut written, &mut seqs, declared);
                        self.finish_write(
                            Response::Recorded { bus_id, seqs },
                            bus_id,
                            &source,
                            &written,
                        )
                    }
                }
            }
        }
    }
}

impl Daemon {
    /// Fold what was written into the state the next briefing reads.
    fn remember(&mut self, written: &[Record]) {
        for record in written {
            Chapter::apply(&mut self.chapters, record);
        }
    }

    /// The hop and the digest, after the vendor events are on disk.
    fn follow_through(
        &mut self,
        bus_id: BusId,
        source: &Source,
        written: &mut Vec<Record>,
        seqs: &mut Vec<u64>,
        declared: Option<HopProvenance>,
    ) {
        self.maybe_route(source, written, seqs, declared);
        self.maybe_continue_leg(bus_id, source, written, seqs);
        self.maybe_summarise(bus_id, source, written, seqs);
    }

    /// The strong claim: we opened the next leg, so identity is known.
    ///
    /// Briefing never runs on this path — the chapter already exists — and
    /// `maybe_continue` only fires after a briefing that led with a displaced
    /// conversation. A directed hop would otherwise leave a `routed` line
    /// with no edge.
    fn maybe_continue_leg(
        &mut self,
        bus_id: BusId,
        source: &Source,
        written: &mut Vec<Record>,
        seqs: &mut Vec<u64>,
    ) {
        if written
            .iter()
            .any(|record| matches!(record.event, Event::Continued { .. }))
        {
            return;
        }
        let Some((next, hop)) = self
            .chapters
            .get(&bus_id)
            .and_then(|chapter| chapter.pending_hop.clone())
        else {
            return;
        };
        if source.harness.as_str() != next {
            return;
        }
        self.write_extra(
            bus_id,
            source,
            Event::Continued {
                predecessor: bus_id,
                hop,
            },
            written,
            seqs,
        );
    }

    fn maybe_route(
        &mut self,
        source: &Source,
        written: &mut Vec<Record>,
        seqs: &mut Vec<u64>,
        declared: Option<HopProvenance>,
    ) {
        let Some(reason) = written.iter().find_map(|record| match &record.event {
            Event::TurnEnded {
                state: TurnState::InterruptedQuota { reason },
                ..
            } => Some(reason.clone()),
            _ => None,
        }) else {
            return;
        };
        let Some(next) = conbus_core::route::overflow_target(&source.harness) else {
            return;
        };
        let bus_id = written.last().map(|r| r.bus_id);
        let Some(bus_id) = bus_id else {
            return;
        };
        // One decision per transition. A leg that keeps refusing keeps
        // sending `InterruptedQuota`, and this used to answer every one of
        // them: on 2026-09-08 a single conversation wrote 146 `routed`
        // records between 16:14 and 16:52, spawned a `hop --apply` behind
        // each, and left 32 headless Claude sessions leased. The decision
        // stands until a `continued` edge closes it, which is the same state
        // `hop --apply` consults before writing one by hand.
        // A program's quota stop is not a routing decision. `route::decide_driven`
        // turns an unattended failover into `Wait` — moving an automation to
        // another vendor with nobody watching is its own hazard — so the
        // `hop --apply` this would spawn declines and nothing is applied.
        // Writing the line anyway put a decision the router refused into the
        // log, and counted it as organic in the very number the provenance
        // work exists to make honest.
        if conbus_core::continuation::unattended(source.entrypoint.as_ref()) {
            return;
        }
        // Nor a profiled conversation back onto the default account: a hop to
        // `claude_code` lands on the default profile, which is another of the
        // person's accounts, and switching is manual only. Writing the line
        // anyway would record a hop the router is about to refuse.
        if next.as_str() == Harness::CLAUDE_CODE
            && self
                .chapters
                .get(&bus_id)
                .is_some_and(|chapter| chapter.claude_profile.is_some())
        {
            return;
        }
        let already_going = self
            .chapters
            .get(&bus_id)
            .and_then(|chapter| chapter.pending_hop.as_ref())
            .is_some_and(|(pending, hop)| pending == next.as_str() && *hop == Hop::Displaced);
        if already_going {
            return;
        }
        self.write_extra(
            bus_id,
            source,
            Event::Routed {
                next: next.as_str().to_string(),
                hop: Hop::Displaced,
                reason,
                code_freshness: None,
                // The daemon writes this one because a hook forwarded a
                // vendor payload saying the turn hit a limit — the only
                // path that is the product deciding for itself. A run that
                // declared itself staged still overrides it, which the hook
                // carries on the wire.
                provenance: Some(declared.unwrap_or(HopProvenance::VendorSignal)),
            },
            written,
            seqs,
        );
        if next.as_str() == conbus_core::event::Harness::CODEX {
            self.pending_warm = Some(bus_id);
        }
    }

    /// The conversation whose Codex warm thread should be opened now that
    /// the hook has its reply.
    pub fn take_pending_warm(&mut self) -> Option<BusId> {
        self.pending_warm.take()
    }

    pub fn take_pending_directed(&mut self) -> Option<(BusId, String)> {
        self.pending_directed
            .take()
            .map(|(bus_id, _kind, dest)| (bus_id, dest))
    }

    pub fn take_pending_enter(&mut self) -> Option<(String, Option<String>)> {
        self.pending_enter.take()
    }

    fn finish_write(
        &mut self,
        mut response: Response,
        bus_id: BusId,
        source: &Source,
        written: &[Record],
    ) -> Response {
        self.note_chat_order(bus_id, source, written);
        self.announce_chat_order(&mut response);
        response
    }

    fn note_chat_order(&mut self, bus_id: BusId, source: &Source, written: &[Record]) {
        let Some((order, workspace)) = written.iter().find_map(|record| {
            let Event::TurnStarted { prompt, author } = &record.event else {
                return None;
            };
            if author.as_ref().is_some_and(|a| a.is_machine()) {
                return None;
            }
            let order = crate::intent::parse(prompt.as_text()?)?;
            Some((order, record.source.workspace.clone()))
        }) else {
            return;
        };
        if order.to == source.harness.as_str() {
            self.pending_enter = Some((order.to, workspace));
        } else {
            self.pending_directed = Some((bus_id, order.kind, order.to));
        }
    }

    fn announce_chat_order(&self, response: &mut Response) {
        let (kind, dest) = if let Some((_, kind, dest)) = &self.pending_directed {
            (kind.as_str(), dest.as_str())
        } else if let Some((dest, _)) = &self.pending_enter {
            ("resume", dest.as_str())
        } else {
            return;
        };
        let context = format!(
            "conbus {kind} · the person ordered {kind} to {dest}. Acknowledge in one line. Do not start new work on this leg."
        );
        let message = format!("conbus · {kind} → {dest}");
        match response {
            Response::Briefed {
                context: existing,
                message: existing_msg,
                ..
            } => {
                *existing = format!("{context}\n\n{existing}");
                *existing_msg = format!("{message} · {existing_msg}");
            }
            Response::Recorded { bus_id, seqs } => {
                *response = Response::Briefed {
                    bus_id: *bus_id,
                    seqs: seqs.clone(),
                    context,
                    message,
                };
            }
            _ => {}
        }
    }

    fn maybe_summarise(
        &mut self,
        bus_id: BusId,
        source: &Source,
        written: &mut Vec<Record>,
        seqs: &mut Vec<u64>,
    ) {
        if !written
            .iter()
            .any(|record| matches!(record.event, Event::TurnEnded { .. }))
        {
            return;
        }
        let Some(chapter) = self.chapters.get(&bus_id) else {
            return;
        };
        if !conbus_core::summary::due(chapter.about.turns) {
            return;
        }
        let covered_turns = chapter.about.turns;
        if written.iter().any(|record| {
            matches!(&record.event, Event::Summarised { covered_turns: n, .. } if *n >= covered_turns)
        }) {
            return;
        }
        let digest = conbus_core::summary::extract(chapter);
        self.write_extra(
            bus_id,
            source,
            Event::Summarised {
                backend: conbus_core::summary::EXTRACTIVE.to_string(),
                covered_turns,
                digest: conbus_core::event::Body::kept(digest),
            },
            written,
            seqs,
        );
    }

    fn maybe_continue(
        &mut self,
        bus_id: BusId,
        source: &Source,
        written: &mut Vec<Record>,
        seqs: &mut Vec<u64>,
    ) {
        let Some(predecessor) = written.iter().rev().find_map(|record| match &record.event {
            Event::Briefed { about, .. } => about.first().copied(),
            _ => None,
        }) else {
            return;
        };
        let Some(chapter) = self.chapters.get(&predecessor) else {
            return;
        };
        if stopped(&chapter.about) != Stopped::Displaced {
            return;
        }
        self.write_extra(
            bus_id,
            source,
            Event::Continued {
                predecessor,
                hop: Hop::Displaced,
            },
            written,
            seqs,
        );
    }

    fn write_extra(
        &mut self,
        bus_id: BusId,
        source: &Source,
        event: Event,
        written: &mut Vec<Record>,
        seqs: &mut Vec<u64>,
    ) {
        match self.log.append(Entry {
            bus_id,
            observed_at: None,
            source: source.clone(),
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event,
        }) {
            Ok(Some(record)) => {
                Chapter::apply(&mut self.chapters, &record);
                seqs.push(record.seq);
                written.push(record);
            }
            Ok(None) => {}
            Err(err) => note!("hop bookkeeping lost: {err}"),
        }
    }

    /// What this session should be told about its checkout, if anything.
    ///
    /// Consulted before the conversation is folded in, so it cannot be briefed
    /// about itself, and only on the prompt that minted it: the per-prompt cost
    /// is paid once at injection and then carried in history for every turn
    /// after it.
    fn brief(&self, bus_id: BusId, written: &[Record]) -> Option<(String, String, Event)> {
        let prompt = written
            .iter()
            .find(|record| matches!(record.event, Event::TurnStarted { .. }))?;

        let asking = Asking {
            harness: prompt.source.harness.clone(),
            workspace: prompt.source.workspace.clone(),
            first_prompt: true,
            // The entrypoint, and only the entrypoint. The vendor's own
            // `promptSource` looked like a second route to the same fact and is
            // not: `sdk` is stamped on hand-typed prompts in this corpus — one
            // of them still carrying the stray keystroke of somebody typing —
            // so reading it as "not a person" would go silent on exactly the
            // sessions worth speaking to.
            unattended: unattended(prompt.source.entrypoint.as_ref()),
            machine_authored: match &prompt.event {
                Event::TurnStarted { prompt, .. } => machine_authored(prompt),
                _ => false,
            },
            prompt_units: match &prompt.event {
                Event::TurnStarted { prompt, .. } => opening_units(prompt),
                _ => 0,
            },
        };

        let candidates: Vec<_> = self
            .chapters
            .values()
            .map(|chapter| chapter.about.clone())
            .collect();
        let decision = decide(&asking, &candidates, self.home.as_deref(), prompt.ts);
        let facts = self.arena_facts(asking.workspace.as_deref());
        let chapters: Vec<Chapter> = match &decision {
            Decision::Brief { recent } => recent
                .iter()
                .filter_map(|id| self.chapters.get(id).cloned())
                .collect(),
            Decision::Silent(silent)
                if matches!(silent, conbus_core::continuation::Silent::NothingToSay)
                    && !facts.is_empty() =>
            {
                Vec::new()
            }
            _ => return None,
        };

        // In the order the decision named them. A briefing that reorders itself
        // is one whose leading item — the conversation that could not continue
        // — stops leading.
        let packed = pack::brief_with(
            bus_id,
            &chapters,
            &facts,
            prompt.ts,
            pack::PACK_BUDGET_UNITS,
        );
        if packed.is_empty() {
            return None;
        }

        // The receipt names what was narrated, most recent first, so the
        // command it offers opens the conversation the person is being told
        // about — not the session they are sitting in.
        let named = chapters
            .first()
            .map(|chapter| chapter.about.bus_id.to_string())
            .unwrap_or_default();
        let hop = chapters
            .first()
            .filter(|chapter| stopped(&chapter.about) == Stopped::Displaced)
            .map(|_| " · conbus hop")
            .unwrap_or("");
        let message = format!(
            "conbus · {} earlier session{} in this checkout · conbus log --bus-id {}{}{hop}",
            chapters.len(),
            if chapters.len() == 1 { "" } else { "s" },
            named.get(..13).unwrap_or(&named),
            match chapters.len() {
                0 | 1 => String::new(),
                n => format!(" (+{} more)", n - 1),
            },
        );
        let event = Event::Briefed {
            about: chapters.iter().map(|c| c.about.bus_id).collect(),
            units: packed.units,
            dropped: packed.dropped,
        };
        Some((packed.text, message, event))
    }

    /// Cursor's inject channel is sessionStart, which is Deferred — no mint.
    /// Brief the checkout anyway so a new composer gets the pack.
    fn maybe_brief_candidate(
        &self,
        brief: bool,
        harness: &Harness,
        workspace: Option<&str>,
    ) -> Response {
        if !brief || harness.as_str() != Harness::CURSOR {
            return Response::Deferred;
        }
        let asking = Asking {
            harness: harness.clone(),
            workspace: workspace.map(str::to_string),
            first_prompt: true,
            unattended: false,
            machine_authored: false,
            prompt_units: 0,
        };
        let now = Timestamp::now();
        let candidates: Vec<_> = self
            .chapters
            .values()
            .map(|chapter| chapter.about.clone())
            .collect();
        let decision = decide(&asking, &candidates, self.home.as_deref(), now);
        let facts = self.arena_facts(asking.workspace.as_deref());
        let chapters: Vec<Chapter> = match &decision {
            Decision::Brief { recent } => recent
                .iter()
                .filter_map(|id| self.chapters.get(id).cloned())
                .collect(),
            Decision::Silent(silent)
                if matches!(silent, conbus_core::continuation::Silent::NothingToSay)
                    && !facts.is_empty() =>
            {
                Vec::new()
            }
            _ => return Response::Deferred,
        };
        let Some(named) = chapters.first().map(|chapter| chapter.about.bus_id) else {
            return Response::Deferred;
        };
        let packed = pack::brief_with(named, &chapters, &facts, now, pack::PACK_BUDGET_UNITS);
        if packed.is_empty() {
            return Response::Deferred;
        }
        let label = named.to_string();
        let hop = chapters
            .first()
            .filter(|chapter| stopped(&chapter.about) == Stopped::Displaced)
            .map(|_| " · conbus hop")
            .unwrap_or("");
        let message = format!(
            "conbus · {} earlier session{} in this checkout · conbus log --bus-id {}{}{hop}",
            chapters.len(),
            if chapters.len() == 1 { "" } else { "s" },
            label.get(..13).unwrap_or(&label),
            match chapters.len() {
                0 | 1 => String::new(),
                n => format!(" (+{} more)", n - 1),
            },
        );
        Response::Briefed {
            bus_id: named,
            seqs: vec![],
            context: packed.text,
            message,
        }
    }

    fn arena_facts(&self, workspace: Option<&str>) -> Vec<String> {
        let Some(root) = &self.arena_root else {
            return Vec::new();
        };
        let Ok(paths) = Paths::from_env(root.to_str(), None, None) else {
            return Vec::new();
        };
        match crate::arena::visible(&paths, workspace) {
            Ok(facts) if !facts.is_empty() => crate::arena::render(&facts),
            _ => Vec::new(),
        }
    }
}

/// How long the liveness probe waits for the far end to say something.
///
/// A daemon that is serving another connection says nothing at all, so this
/// expiring is the *positive* answer. It only has to outlast a scheduling
/// hiccup, not any real work.
const LIVENESS_PROBE: Duration = Duration::from_millis(50);

/// Is anything actually serving this socket?
///
/// Connecting is not the question, though it used to be the answer. Closing a
/// listening AF_UNIX socket does not tear the path down at once, so a
/// connect to a daemon that has just died can still succeed — measured here
/// under load, with `lsof` showing no process holding the path. On a machine
/// where launchd restarts the daemon the moment it exits, "I connected"
/// would refuse the new daemon its own socket and turn a crash into a restart
/// loop.
///
/// A peer that is really there holds the line: our request is what it waits
/// for, so the read expires. A peer that is going away hands back end-of-file
/// straight away.
fn someone_is_serving(socket: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(socket) else {
        return false;
    };
    if stream.set_read_timeout(Some(LIVENESS_PROBE)).is_err() {
        return false;
    }
    let mut byte = [0u8; 1];
    match (&stream).read(&mut byte) {
        // Nobody on the other end: the socket answered out of a listener
        // that is closing, or the daemon hung up as we arrived.
        Ok(0) => false,
        // It spoke first, which no version of this daemon does, but anything
        // that talks is certainly alive.
        Ok(_) => true,
        Err(err) => matches!(
            err.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ),
    }
}

/// Bind the control socket, refusing to steal one a live daemon still owns.
pub fn bind(socket: &Path) -> io::Result<UnixListener> {
    if socket.exists() {
        if someone_is_serving(socket) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "a conbus daemon is already listening on {}",
                    socket.display()
                ),
            ));
        }
        // Nothing answered, so this is a socket left behind by a dead daemon.
        fs::remove_file(socket)?;
    }
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(socket)?;
    // Raise the backlog before any client can connect. Doing this later,
    // after set_nonblocking, has been observed to hand the next hook a
    // dead accepted fd — write fails with Broken pipe, or the read comes
    // back empty. Cursor fires tool hooks in parallel; 128 is the std
    // default and is too short once a few of those land together.
    let _ = unsafe { libc::listen(listener.as_raw_fd(), 256) };
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Write `aliased` records so a reader following either split id sees both.
fn repair_splits(held: &[Record], log: &mut SegmentLog) -> io::Result<u64> {
    let mut n = 0;
    for (key, conversations) in identity::live_splits(held) {
        let mut ids: Vec<BusId> = conversations.into_iter().collect();
        ids.sort();
        let Some((canonical, rest)) = ids.split_first() else {
            continue;
        };
        for other in rest {
            if identity::already_aliased(held, *canonical, *other) {
                continue;
            }
            let source = Source {
                harness: key.harness.clone(),
                harness_version: None,
                vendor_session_id: Some(key.vendor_session_id.clone()),
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            };
            log.append(Entry {
                bus_id: *canonical,
                observed_at: None,
                source: source.clone(),
                vendor_turn_id: None,
                vendor_agent_id: None,
                hook_id: None,
                spooled_at: None,
                event: Event::Aliased { also: *other },
            })?;
            log.append(Entry {
                bus_id: *other,
                observed_at: None,
                source,
                vendor_turn_id: None,
                vendor_agent_id: None,
                hook_id: None,
                spooled_at: None,
                event: Event::Aliased { also: *canonical },
            })?;
            n += 1;
        }
    }
    Ok(n)
}

/// What a daemon keeps from the log it starts on: the bindings, the chapters,
/// and two counts for its start line. Not the records.
struct Restored {
    registry: Registry,
    chapters: HashMap<BusId, Chapter>,
    records: usize,
    unreadable: usize,
}

/// Fill the gaps the official transcripts show and alias split conversations,
/// writing both to the log. `conbus daemon --prepare`.
///
/// Both read the whole log, so the recorder runs this in a short-lived child of
/// itself, whose memory goes back to the system when it exits, and folds its
/// own state afterwards from a streaming read.
pub fn prepare(paths: &Paths, retention: Retention) -> io::Result<()> {
    // No core dump of a process holding every prompt on the machine.
    let _ = harden::harden();
    let t0 = std::time::Instant::now();
    // A log that cannot be read is not an empty log. Everything the fill reads is
    // compared against it, so nothing can be said to be missing: read as empty,
    // every conversation in a chat export looked new and was appended again.
    let mut scan = match SegmentLog::scan_meta(paths.log_dir(), Some(&paths.cache_dir())) {
        Ok(scan) => scan,
        Err(err) => {
            note!(
                "could not read {} ({err}); gap fill and split repair skipped, \
                 and the next start fills the range",
                paths.log_dir().display()
            );
            crate::backfill::defer_fill(paths);
            return Ok(());
        }
    };
    let t_scan = t0.elapsed();
    let mut log = SegmentLog::open(paths.log_dir(), retention)?;
    // Official transcripts only, same parser as `backfill --gaps`. Turns
    // that happened while no recorder was running join the live `bus_id`
    // before hooks are accepted, so the registry sees one conversation.
    let mut gap_timing = crate::backfill::GapTiming::default();
    match crate::backfill::append_gaps_at_start(paths, &scan.records, &mut log) {
        Ok((0, timing)) => gap_timing = timing,
        Ok((n, timing)) => {
            gap_timing = timing;
            note!("filled {n} gap record(s) from official transcripts");
            // The written records are the log's newest n — read just the tail
            // instead of scanning every segment a second time.
            let (written, _) =
                SegmentLog::tail(paths.log_dir(), n as usize, |_| true).unwrap_or_default();
            scan.records
                .extend(written.iter().map(conbus_core::segment::meta));
        }
        Err(err) => {
            note!("gap fill skipped ({err}); the next start fills the range");
            crate::backfill::defer_fill(paths);
        }
    }
    let t_gaps = t0.elapsed() - t_scan;
    match repair_splits(&scan.records, &mut log) {
        Ok(0) => {}
        Ok(n) => note!("aliased {n} split conversation pair(s)"),
        Err(err) => note!("split alias skipped ({err})"),
    }
    if gap_timing.budget_exhausted {
        // The socket serving matters more than finishing the fill: the
        // skipped range is remembered in `cache/gap-since` and resumed on
        // the next start, or filled now by `conbus backfill --gaps`.
        note!(
            "gap fill hit its start budget; the skipped range is held in cache/gap-since and resumed on the next start"
        );
    }
    note!(
        "prepare breakdown: meta scan {} ms, gap fill {} ms \
         (claude: walk {} ms, read {} files {} MB in {} ms; \
         codex: walk {} ms, app-server {} ms, thread/list {} pages {} ms, \
         list_turns {}/{} threads {} ms{}; chat export {} ms), split repair {} ms{}{}",
        t_scan.as_millis(),
        t_gaps.as_millis(),
        gap_timing.claude_walk_ms,
        gap_timing.claude_files,
        gap_timing.claude_bytes / 1048576,
        gap_timing.claude_read_ms,
        gap_timing.codex_walk_ms,
        gap_timing.codex_start_ms,
        gap_timing.thread_list_pages,
        gap_timing.thread_list_ms,
        gap_timing.list_turns_threads,
        gap_timing.list_turns_attempted,
        gap_timing.list_turns_ms,
        gap_timing
            .list_turns_err
            .as_deref()
            .map(|err| format!(", first error: {err}"))
            .unwrap_or_default(),
        gap_timing.chat_export_ms,
        (t0.elapsed() - t_scan - t_gaps).as_millis(),
        if gap_timing.codex_layout_unknown {
            format!(
                "; codex rollout names unrecognised ({} files) — fell back to thread/list",
                gap_timing.codex_unknown_files
            )
        } else {
            String::new()
        },
        if gap_timing.gap_marker_garbled {
            "; gap-since marker unreadable — ignored"
        } else {
            ""
        },
    );
    Ok(())
}

/// Run [`prepare`] in a child process, and in this one only if the child could
/// not run.
fn prepare_in_child(paths: &Paths, retention: Retention) {
    let started = std::time::Instant::now();
    let status = std::env::current_exe().and_then(|bin| {
        Command::new(bin)
            .args(["daemon", "--prepare"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // The child's notes belong in the same place as the daemon's.
            .stderr(Stdio::inherit())
            .status()
    });
    let why = match status {
        Ok(status) if status.success() => {
            note!(
                "start-up repair ran in a separate process in {} ms",
                started.elapsed().as_millis()
            );
            return;
        }
        Ok(status) => format!("it exited {status}"),
        Err(err) => format!("it could not start ({err})"),
    };
    note!("start-up repair in a separate process failed: {why}; running it in the daemon itself");
    if let Err(err) = prepare(paths, retention) {
        note!("start-up repair skipped ({err})");
    }
}

/// Fold what the daemon runs on from one streaming read of the log. The records
/// are not collected.
///
/// `serve` used to hold a collected scan for the life of the process, which is
/// weeks: 564 MB of parsed records for a log of 298k on 2026-09-28, locked into
/// RAM by `relock_derived`, and growing with every record the machine writes.
fn restore(paths: &Paths, retention: Retention) -> io::Result<(SegmentLog, Restored)> {
    let mut registry = conbus_core::registry::Restoring::default();
    let mut chapters = HashMap::new();
    let mut records = 0;
    let folded = SegmentLog::scan_meta_each(paths.log_dir(), Some(&paths.cache_dir()), |record| {
        registry.absorb(&record);
        Chapter::apply(&mut chapters, &record);
        records += 1;
    });
    let restored = match folded {
        Ok(defects) => Restored {
            registry: registry.finish(),
            chapters,
            records,
            unreadable: defects.len(),
        },
        Err(err) => {
            note!(
                "could not read {} ({err}); starting with no bindings, \
                 so a session that outlived the last daemon will get a second bus_id",
                paths.log_dir().display()
            );
            Restored {
                registry: Registry::new(),
                chapters: HashMap::new(),
                records: 0,
                unreadable: 0,
            }
        }
    };
    let log = SegmentLog::open(paths.log_dir(), retention)?;
    Ok((log, restored))
}

/// Run the daemon until the process is stopped.
pub fn serve(paths: &Paths, retention: Retention) -> io::Result<()> {
    let _ = harden::harden();
    if let Some(problem) = paths.socket_problem() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, problem));
    }
    // Before anything is written: an older build left the root 0755. A root
    // that cannot be closed — someone else's, or on a volume without modes —
    // is still used, and said so: the log directory under it is closed on
    // its own.
    if let Err(err) = paths.keep_private() {
        note!(
            "{} could not be made private ({err}); the log under it still is",
            paths.root().display()
        );
    }
    // The first stamp, before anything long: a daemon that takes a minute to
    // prepare shows "starting" to a doctor, never a dead process — and a hook
    // that arrives early meets a refused socket at once rather than waiting
    // 500 ms on a daemon that cannot answer yet.
    if let Err(err) = crate::build_info::stamp(paths, retention.name()) {
        note!("could not write the build stamp: {err}");
    }
    // The registry is derived state, so it is rebuilt from the segments rather
    // than starting empty — otherwise a restart forks every live conversation.
    // But a log we cannot read must not stop us recording: an empty registry
    // costs a forked conversation, which the vendor session id on every record
    // still makes recoverable, while refusing to start costs every future turn.
    prepare_in_child(paths, retention);
    let (log, restored) = restore(paths, retention)?;
    let mut daemon = Daemon::new(log, restored.registry, restored.chapters)
        .with_arena(paths.root().to_path_buf())
        .watch_chat_exports(paths);
    // Anything the last daemon lost to its restart is answered before the
    // socket exists — these hooks already waited once.
    drain_spool(&mut daemon, paths);

    // Bind only now: a hook that arrives during prepare and restore found no
    // socket (or a refused one) and went straight to its fallback, instead of
    // blocking on a listener that was not answering. The test knob widens that
    // window so the order is observable.
    if let Ok(ms) = std::env::var("CONBUS_TEST_PAUSE_BEFORE_BIND_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let listener = bind(&paths.socket())?;
    // A hook that spooled while restore ran is answered as soon as the socket
    // can report it.
    drain_spool(&mut daemon, paths);

    // Restore grew the heap. Darwin's first `mlockall` is ENOSYS; lock the
    // pages that now hold the bindings and chapters, not the empty start heap.
    // The build stamp is already on disk; this only adds the lock result.
    let report = harden::relock_derived();
    if let Err(err) = crate::build_info::stamp_hardening(
        paths,
        report.core_dumps_disabled,
        report.memory_locked,
        report.memory_lock_error.clone(),
    ) {
        note!("could not stamp hardening: {err}");
    }
    // Serving is the last thing the stamp learns: absent means still starting.
    if let Err(err) = crate::build_info::stamp_serving(paths) {
        note!("could not mark the stamp serving: {err}");
    }

    note!(
        "{}: socket {} log {} ({} record(s), {} unreadable) core_dumps_disabled={} memory_locked={}{}",
        crate::build_info::BUILD,
        paths.socket().display(),
        paths.log_dir().display(),
        restored.records,
        restored.unreadable,
        report.core_dumps_disabled,
        report.memory_locked,
        report
            .memory_lock_error
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default(),
    );

    listener.set_nonblocking(true)?;
    loop {
        let mut fds = [libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 1, 2000) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            note!("poll failed: {err}");
            continue;
        }
        if n == 0 {
            drain_spool(&mut daemon, paths);
            if let Err(err) = daemon.absorb_chat_exports() {
                note!("chat export ingest skipped ({err})");
            }
            continue;
        }
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(err) = handle_connection(&mut daemon, stream) {
                        note!("connection failed: {err}");
                    }
                    drain_pending(&mut daemon);
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => {
                    note!("accept failed: {err}");
                    break;
                }
            }
        }
    }
}

/// Hand off everything the connection queued, without waiting for any of it.
///
/// The daemon serves one connection to its end before it accepts the next, so
/// anything done here is done in front of every hook still in the backlog.
/// Proven from the outside: `a_landing_that_blocks_does_not_hold_the_next_hook`
/// in the e2e suite starts a daemon whose Cursor binary sleeps, and times the
/// hook behind it.
fn drain_pending(daemon: &mut Daemon) {
    if let Some(bus_id) = daemon.take_pending_warm() {
        spawn_hop_apply(bus_id, None);
    }
    if let Some((bus_id, dest)) = daemon.take_pending_directed() {
        spawn_hop_apply(bus_id, Some(dest));
    }
    if let Some((dest, workspace)) = daemon.take_pending_enter() {
        spawn_enter(dest, workspace);
    }
}

/// Bring the person to the leg they asked for, off the accept loop.
///
/// Its two siblings were already threads and this one was not. The daemon
/// serves one connection to its end before it accepts the next, so anything
/// it waits for here is a wait every hook behind it pays: `cursor -n` blocks
/// until Cursor's own process exits, and the Terminal path blocks on
/// `osascript` — a cold app launch, or a macOS automation prompt nobody is
/// looking at. On 2026-09-08 sixty hooks in five minutes gave up at their
/// 500ms deadline while the daemon was busy with something.
fn spawn_enter(dest: String, workspace: Option<String>) {
    std::thread::spawn(
        move || match crate::landing::enter(&dest, workspace.as_deref(), None) {
            Ok(true) => {}
            Ok(false) => note!("resume {dest} had nothing to open"),
            Err(err) => note!("resume {dest} failed ({err})"),
        },
    );
}

/// Open the Codex warm thread after a quota hop, out of the hook's deadline.
///
/// `conbus hop --apply` is the same path a person runs; it writes a lease
/// and never injects into a thread they opened. The daemon must not wait
/// for it — `thread/start` is seconds, the hook client gives up at 500ms.
fn spawn_hop_apply(bus_id: BusId, to: Option<String>) {
    let bin = std::env::current_exe().unwrap_or_else(|_| "conbus".into());
    std::thread::spawn(move || {
        let id = bus_id.to_string();
        let mut cmd = Command::new(bin);
        cmd.args(["hop", "--from", &id, "--apply"]);
        if let Some(dest) = to.as_deref() {
            cmd.args(["--to", dest]);
        }
        // Timed and quoted, whatever it did. On 2026-09-27 the first organic
        // hop reached its Codex thread 21.6 minutes after the decision, and
        // this said nothing: success and a decision to wait both left no line.
        let started = std::time::Instant::now();
        let output = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        let ms = started.elapsed().as_millis();
        match output {
            Ok(out) => {
                let said = String::from_utf8_lossy(&out.stdout);
                let route = said
                    .lines()
                    .find_map(|line| line.strip_prefix("route"))
                    .map(str::trim)
                    .unwrap_or("no route line");
                let steps = said
                    .lines()
                    .find_map(|line| line.strip_prefix("steps"))
                    .map(str::trim)
                    .unwrap_or("no steps line");
                if out.status.success() {
                    note!("automatic hop --apply for {bus_id} in {ms} ms: {route} ({steps})");
                } else {
                    let err = String::from_utf8_lossy(&out.stderr);
                    note!(
                        "automatic hop --apply for {bus_id} exited {} after {ms} ms ({route}; {steps}): {}",
                        out.status,
                        err.trim()
                    );
                }
            }
            Err(err) => note!("automatic hop --apply failed after {ms} ms: {err}"),
        }
    });
}

/// How long an accepted connection may stay silent before the daemon walks
/// away from it.
///
/// The daemon serves one connection to its end before it accepts the next, so
/// a client that connects and says nothing is not its own problem but
/// everybody else's: every hook behind it waits in the backlog, and the hook
/// client gives up after its `REPLY_DEADLINE` — the turn goes on, and its
/// record is gone. A hook has its whole request ready before it connects (the
/// payload was read from stdin first), so silence here is never a hook that is
/// thinking; it is something that is not a hook, or one that has wedged. Well
/// inside the hook's half second, so a silent neighbour costs the hooks behind
/// it a wait rather than their records.
pub const SILENCE_BOUND: Duration = Duration::from_millis(200);

// The daemon has to give up on silence before the hook gives up on the daemon,
// or the bound protects nobody.
const _: () = assert!(SILENCE_BOUND.as_millis() < crate::hook::client::REPLY_DEADLINE.as_millis());

fn handle_connection(daemon: &mut Daemon, stream: UnixStream) -> io::Result<()> {
    // The listener is non-blocking so poll can sleep. An accepted fd may
    // inherit that; a read then returns WouldBlock before the hook writes,
    // and we drop a live client as if it had gone silent.
    if let Err(err) = stream.set_nonblocking(false) {
        return if err.raw_os_error() == Some(libc::EINVAL) {
            Ok(())
        } else {
            Err(err)
        };
    }
    if let Err(err) = bound(&stream) {
        // A peer that connected and left before it was served — the doctor's
        // liveness probe, the daemon's own bind check. Darwin refuses to set
        // options on a socket that can no longer send or receive, with EINVAL,
        // and there is nobody left to serve, so this is the ordinary end of a
        // connection rather than a fault to report. The first build of this
        // bound reported it, one line in daemon.log per `conbus doctor`.
        return if err.raw_os_error() == Some(libc::EINVAL) {
            Ok(())
        } else {
            Err(err)
        };
    }
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    let mut served = 0usize;
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            // A client that has been answered and not yet closed is on its way
            // out, and that is the ordinary end of a connection. One that never
            // spoke is not a hook, and the hooks queued behind it must not wait
            // for it.
            Err(err) if went_silent(&err) => {
                return if served == 0 {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "a connection sent no request within {SILENCE_BOUND:?}; dropped it"
                        ),
                    ))
                } else {
                    Ok(())
                };
            }
            Err(err) => return Err(err),
        };
        if line.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => daemon.handle(request),
            Err(err) => Response::Failed {
                error: format!("unreadable request: {err}"),
            },
        };
        writeln!(
            writer,
            "{}",
            serde_json::to_string(&response).map_err(io::Error::other)?
        )?;
        writer.flush()?;
        served += 1;
    }
    Ok(())
}

/// Bound how long this connection may keep the daemon waiting, in either
/// direction.
fn bound(stream: &UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SILENCE_BOUND))?;
    stream.set_write_timeout(Some(SILENCE_BOUND))
}

/// A read or write that ran out its deadline. Unix reports it as `WouldBlock`;
/// the standard library documents either kind.
fn went_silent(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// `/clear` means forget. CLI names it on the flushed session start. Desktop
/// `#76704` omits `source`; inventing one would be a workaround. Treating a
/// desktop start with no trigger as that gesture is reading the omission.
fn session_was_cleared(written: &[Record]) -> bool {
    written.iter().any(|record| match &record.event {
        Event::SessionCandidate { trigger: Some(t) } if t == "clear" => true,
        Event::SessionCandidate { trigger: None } => record
            .source
            .entrypoint
            .as_ref()
            .is_some_and(|e| e.as_str() == conbus_core::event::Entrypoint::CLAUDE_DESKTOP),
        _ => false,
    })
}

/// Requests the spool holds, oldest first — the files are named for a UUIDv7,
/// so a name sort is time order. Each goes through `Daemon::handle`, the one
/// writer, and is deleted after; a refusal that names a NEWER protocol stays
/// for a newer daemon, while any other refusal is a drop with the reason.
fn drain_spool(daemon: &mut Daemon, paths: &Paths) {
    let Ok(entries) = std::fs::read_dir(paths.spool_dir()) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    for file in files {
        // Only what a hook writes: a regular file no larger than the hook's own
        // 1 MiB cap. A link or an oversized file is nobody's spooled request,
        // and following or loading it would read whatever it points at.
        let regular = std::fs::symlink_metadata(&file)
            .is_ok_and(|meta| meta.file_type().is_file() && meta.len() <= 1_048_576);
        let request = regular
            .then(|| std::fs::read_to_string(&file).ok())
            .flatten()
            .and_then(|text| serde_json::from_str::<Request>(text.trim()).ok());
        match request {
            None => {
                crate::drops::record_in(
                    Some(paths),
                    &crate::drops::Dropped::new(format!(
                        "spool entry {} could not be read",
                        file.display()
                    )),
                );
                let _ = std::fs::remove_file(&file);
            }
            Some(request) => {
                // The same shape the hook's own drop carried: the event and
                // the session, so a ghost session's end can be told from a
                // lost tool call.
                let kind = serde_json::to_value(&request.event)
                    .ok()
                    .and_then(|v| v.get("event").and_then(|k| k.as_str().map(String::from)))
                    .unwrap_or_else(|| "unknown".into());
                let session: String = request.vendor_session_id.chars().take(8).collect();
                match daemon.handle(request) {
                    Response::Failed { ref error } if error.contains("protocol v") => {
                        // A newer hook wrote this; a newer daemon drains it.
                    }
                    Response::Failed { error } => {
                        crate::drops::record_in(
                            Some(paths),
                            &crate::drops::Dropped::new(format!("spooled hook refused: {error}")),
                        );
                        let _ = std::fs::remove_file(&file);
                    }
                    Response::Orphaned => {
                        crate::drops::record_in(
                            Some(paths),
                            &crate::drops::Dropped::new(
                                "spooled hook still has no conversation (orphan)",
                            )
                            .with_detail(format!("{kind} for session {session}")),
                        );
                        let _ = std::fs::remove_file(&file);
                    }
                    _ => {
                        let _ = std::fs::remove_file(&file);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::event::{Body, Event, Harness, Hop, Source, TurnState, VendorTurnId};
    use conbus_core::ids::BusId;
    use conbus_core::record::Record;
    use conbus_core::segment::SegmentLog;

    fn request(event: Event) -> Request {
        Request {
            protocol_version: Some(crate::wire::PROTOCOL_VERSION),
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: None,
            vendor_session_id: "abc123".into(),
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            workspace: Some("/repo".into()),
            entrypoint: None,
            brief: true,
            provenance: None,
            event,
            profile: None,
        }
    }

    #[test]
    fn a_daemon_older_than_the_hook_refuses_instead_of_dropping_what_it_cannot_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());

        let response = daemon.handle(Request {
            protocol_version: Some(crate::wire::PROTOCOL_VERSION + 1),
            ..request(Event::TurnStarted {
                author: None,
                prompt: Body::kept("from a newer hook"),
            })
        });

        let Response::Failed { error } = response else {
            panic!("a newer hook must be refused, not half-read: {response:?}");
        };
        assert!(error.contains("Restart the daemon"), "{error}");
        assert_eq!(SegmentLog::scan(dir.path()).expect("scan").records, vec![]);
    }

    #[test]
    fn a_request_that_names_no_session_is_refused_rather_than_keyed_on_nothing() {
        // The second line behind the hook's own refusal. Two nameless tool
        // calls from different sessions must not become one conversation.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());

        for blank in ["", "   "] {
            let response = daemon.handle(Request {
                harness: Harness::new(Harness::CURSOR),
                vendor_session_id: blank.into(),
                ..request(Event::TurnStarted {
                    author: None,
                    prompt: Body::kept("a prompt with no session"),
                })
            });

            let Response::Failed { error } = response else {
                panic!("an empty session id must be refused: {response:?}");
            };
            assert_eq!(error, crate::hook::NO_SESSION);
        }
        assert_eq!(SegmentLog::scan(dir.path()).expect("scan").records, vec![]);
    }

    #[test]
    fn a_hook_too_old_to_name_a_version_is_still_served() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());

        let response = daemon.handle(Request {
            protocol_version: None,
            ..request(Event::TurnStarted {
                author: None,
                prompt: Body::kept("from an older hook"),
            })
        });

        assert!(
            matches!(response, Response::Recorded { .. }),
            "{response:?}"
        );
    }

    #[test]
    fn a_ghost_session_start_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());

        let response = daemon.handle(request(Event::SessionCandidate {
            trigger: Some("startup".into()),
        }));

        assert_eq!(response, Response::Deferred);
        assert_eq!(SegmentLog::scan(dir.path()).expect("scan").records, vec![]);
    }

    #[test]
    fn the_first_prompt_writes_the_session_start_and_the_turn_under_one_conversation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        daemon.handle(request(Event::SessionCandidate {
            trigger: Some("startup".into()),
        }));

        let response = daemon.handle(request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("ship the recorder"),
        }));

        let Response::Recorded { bus_id, seqs } = response else {
            panic!("expected a recording, got {response:?}");
        };
        assert_eq!(seqs, vec![0, 1]);

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert_eq!(scan.defects, vec![]);
        assert_eq!(scan.records.len(), 2);
        assert!(scan.records.iter().all(|r| r.bus_id == bus_id));
        assert_eq!(
            scan.records[0].source.vendor_session_id.as_deref(),
            Some("abc123"),
            "the vendor session id must be recorded as an alias of the bus_id"
        );
    }

    #[test]
    fn a_hook_under_a_profile_stamps_the_record_with_it() {
        // `CLAUDE_CONFIG_DIR=$HOME/.claude-work claude` puts every hook child
        // under the work account; the records it makes say so.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());

        let response = daemon.handle(Request {
            profile: Some("work".into()),
            ..request(Event::TurnStarted {
                author: None,
                prompt: Body::kept("a prompt from the work account"),
            })
        });

        assert!(
            matches!(response, Response::Recorded { .. }),
            "{response:?}"
        );
        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert!(
            scan.records
                .iter()
                .all(|r| r.source.profile.as_deref() == Some("work")),
            "every record the request wrote is stamped with the profile"
        );
    }

    #[test]
    fn a_quota_stop_records_the_hop_the_router_decided() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        daemon.handle(request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("keep going"),
        }));

        let response = daemon.handle(request(Event::TurnEnded {
            state: TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            },
            assistant_message: None,
            tokens: None,
        }));

        let Response::Recorded { seqs, .. } = response else {
            panic!("expected a recording, got {response:?}");
        };
        assert!(seqs.len() >= 2, "turn + routed: {seqs:?}");

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        let routed = scan
            .records
            .iter()
            .find(|record| matches!(record.event, Event::Routed { .. }));
        let Some(Event::Routed {
            next, hop, reason, ..
        }) = routed.map(|r| &r.event)
        else {
            panic!("the quota stop must leave a routed line: {scan:?}");
        };
        assert_eq!(next, Harness::CODEX);
        assert_eq!(*hop, Hop::Displaced);
        assert_eq!(reason, "rate_limit");
        assert_eq!(
            daemon.take_pending_warm(),
            Some(scan.records[0].bus_id),
            "the warm thread is opened after the hook is answered"
        );
    }

    #[test]
    fn an_opencode_quota_stop_overflows_to_codex() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        let mut req = request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("keep going"),
        });
        req.harness = Harness::new(Harness::OPENCODE);
        daemon.handle(req.clone());
        req.event = Event::TurnEnded {
            state: TurnState::InterruptedQuota {
                reason: "GoUsageLimitError".into(),
            },
            assistant_message: None,
            tokens: None,
        };
        let response = daemon.handle(req);
        let Response::Recorded { seqs, .. } = response else {
            panic!("expected a recording, got {response:?}");
        };
        assert!(seqs.len() >= 2, "turn + routed: {seqs:?}");
        let scan = SegmentLog::scan(dir.path()).expect("scan");
        let routed = scan.records.iter().find_map(|record| match &record.event {
            Event::Routed { next, .. } => Some(next.as_str()),
            _ => None,
        });
        assert_eq!(routed, Some(Harness::CODEX));
        assert_eq!(daemon.take_pending_warm(), Some(scan.records[0].bus_id));
    }

    #[test]
    fn restore_keeps_what_the_daemon_runs_on_and_not_the_records() {
        // `serve` held its start-up scan for the life of the process: on
        // 2026-09-28 that was 564 MB of parsed records for a log of 298k,
        // locked into RAM with the rest of the derived state. What a daemon
        // runs on is the bindings, the chapters and two counts for its start
        // line, and `restore` returns exactly that.
        let home = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(home.path().join("bus").to_str(), None, None).expect("paths");
        {
            let log = SegmentLog::open(paths.log_dir(), Retention::default()).expect("open");
            let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
            daemon.handle(request(Event::TurnStarted {
                author: None,
                prompt: Body::kept("hello"),
            }));
        }
        let written = SegmentLog::scan(paths.log_dir())
            .expect("scan")
            .records
            .len();
        assert!(written > 0);

        let (_log, restored) = restore(&paths, Retention::default()).expect("restore");

        assert_eq!(restored.records, written);
        assert_eq!(restored.unreadable, 0);
        assert_eq!(restored.chapters.len(), 1, "one conversation");
    }

    #[test]
    fn a_grok_quota_stop_overflows_to_codex() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        let mut grok = request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("keep going"),
        });
        grok.harness = Harness::new(Harness::GROK);
        daemon.handle(grok.clone());
        grok.event = Event::TurnEnded {
            state: TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            },
            assistant_message: None,
            tokens: None,
        };

        let response = daemon.handle(grok);
        let Response::Recorded { seqs, .. } = response else {
            panic!("expected a recording, got {response:?}");
        };
        assert!(seqs.len() >= 2, "turn + routed: {seqs:?}");

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        let routed = scan.records.iter().find_map(|record| match &record.event {
            Event::Routed { next, .. } => Some(next.as_str()),
            _ => None,
        });
        assert_eq!(routed, Some(Harness::CODEX));
        assert_eq!(
            daemon.take_pending_warm(),
            Some(scan.records[0].bus_id),
            "attach overflow still opens the Codex warm after the hook reply"
        );
    }

    #[test]
    fn a_cursor_classified_quota_stop_overflows_to_codex() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        let mut req = request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("keep going"),
        });
        req.harness = Harness::new(Harness::CURSOR);
        daemon.handle(req.clone());
        req.event = Event::TurnEnded {
            state: TurnState::InterruptedQuota {
                reason: "PRO_USER_USAGE_LIMIT".into(),
            },
            assistant_message: None,
            tokens: None,
        };
        let response = daemon.handle(req);
        let Response::Recorded { seqs, .. } = response else {
            panic!("expected a recording, got {response:?}");
        };
        assert!(seqs.len() >= 2, "turn + routed: {seqs:?}");
        let scan = SegmentLog::scan(dir.path()).expect("scan");
        let routed = scan.records.iter().find_map(|record| match &record.event {
            Event::Routed { next, .. } => Some(next.as_str()),
            _ => None,
        });
        assert_eq!(routed, Some(Harness::CODEX));
    }

    #[test]
    fn a_chat_order_queues_a_directed_hop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        let mut req = request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("이제 커서로 작업할거야"),
        });
        req.workspace = Some("/repo".into());
        let response = daemon.handle(req);
        match response {
            Response::Briefed { message, .. } => {
                assert!(message.contains("cursor"), "{message}");
            }
            other => panic!("expected a hop briefing, got {other:?}"),
        }
        let (_bus_id, dest) = daemon
            .take_pending_directed()
            .expect("the order must hop after the hook reply");
        assert_eq!(dest, Harness::CURSOR);
        assert!(daemon.take_pending_warm().is_none());
    }

    #[test]
    fn a_prompt_that_is_not_an_order_does_not_hop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        daemon.handle(request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("hand this off"),
        }));
        assert!(daemon.take_pending_directed().is_none());
        assert!(daemon.take_pending_enter().is_none());
    }

    #[test]
    fn a_successor_on_the_warm_leg_records_the_edge() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        daemon.handle(request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("hand this off"),
        }));
        daemon.handle(request(Event::Routed {
            next: Harness::CODEX.into(),
            hop: Hop::HandedOff,
            reason: "directed".into(),
            code_freshness: None,
            provenance: None,
        }));
        daemon.handle(request(Event::Leased {
            holder: "warm:thr_bus".into(),
        }));

        let response = daemon.handle(Request {
            harness: Harness::new(Harness::CODEX),
            vendor_session_id: "thr_bus".into(),
            ..request(Event::SessionCandidate {
                trigger: Some("resume".into()),
            })
        });

        let Response::Recorded { bus_id, .. } = response else {
            panic!("the warm thread must stay on the predecessor: {response:?}");
        };
        let scan = SegmentLog::scan(dir.path()).expect("scan");
        let continued = scan
            .records
            .iter()
            .find(|record| matches!(record.event, Event::Continued { .. }));
        let Some(Event::Continued { predecessor, hop }) = continued.map(|r| &r.event) else {
            panic!("the successor must leave a continued line: {scan:?}");
        };
        assert_eq!(*predecessor, bus_id);
        assert_eq!(*hop, Hop::HandedOff);
        assert!(
            scan.records.iter().all(|r| r.bus_id == bus_id),
            "identity must not move"
        );
    }

    #[test]
    fn a_flushed_session_start_keeps_its_own_absent_correlator() {
        // The deferred candidate and the prompt that flushes it are written in
        // one batch, but they did not arrive with the same correlator — the
        // vendor sends none on a session start. Stamping the batch once would
        // write a correlation the vendor never sent, into the source of truth,
        // with nothing on the line to show it was invented.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        daemon.handle(Request {
            vendor_turn_id: None,
            ..request(Event::SessionCandidate {
                trigger: Some("startup".into()),
            })
        });

        daemon.handle(Request {
            vendor_turn_id: Some(VendorTurnId::new("TURN-1")),
            ..request(Event::TurnStarted {
                author: None,
                prompt: Body::kept("ship it"),
            })
        });

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert_eq!(scan.records.len(), 2, "both records must be written");
        assert_eq!(
            scan.records[0].vendor_turn_id, None,
            "the session start borrowed the prompt's correlator"
        );
        assert_eq!(
            scan.records[1]
                .vendor_turn_id
                .as_ref()
                .map(VendorTurnId::as_str),
            Some("TURN-1"),
            "the prompt lost its own correlator"
        );
    }

    #[test]
    fn a_stop_without_a_prompt_is_reported_as_an_orphan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());

        let response = daemon.handle(request(Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: None,
            tokens: None,
        }));

        assert_eq!(response, Response::Orphaned);
    }

    #[test]
    fn binding_refuses_to_steal_a_socket_a_live_daemon_still_owns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("conbus.sock");
        let _live = bind(&socket).expect("first bind");

        let err = bind(&socket).expect_err("second bind must fail");

        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
    }

    #[test]
    fn binding_reclaims_a_socket_left_behind_by_a_dead_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("conbus.sock");
        drop(bind(&socket).expect("first bind"));
        assert!(
            socket.exists(),
            "the stale socket file should still be there"
        );

        bind(&socket).expect("a stale socket must be reclaimable");
    }

    #[test]
    fn a_connection_that_never_speaks_is_dropped_so_the_hooks_behind_it_are_served() {
        // The daemon serves one connection to its end before it accepts the
        // next, so a client that connects and says nothing is not its own
        // problem but everybody else's. A hook has its request ready before it
        // connects — the payload was read from stdin first — so silence here
        // is never a hook that is thinking.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        let (ours, theirs) = UnixStream::pair().expect("socketpair");

        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = handle_connection(&mut daemon, ours);
            let _ = done.send(outcome.map_err(|err| err.kind()));
        });

        let outcome = finished
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the daemon is still waiting on a client that will never speak");
        assert_eq!(outcome, Err(io::ErrorKind::TimedOut));
        drop(theirs);
    }

    #[test]
    fn a_client_that_lingers_after_being_answered_is_not_a_fault() {
        // A hook exits the moment it has printed its reply, and the kernel
        // closes its end for it. One that is a little slow about it has still
        // been served, so walking away from it is the ordinary end of a
        // connection, not a failure to report.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        let (ours, mut theirs) = UnixStream::pair().expect("socketpair");
        let line = serde_json::to_string(&request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("hello"),
        }))
        .expect("json");
        writeln!(theirs, "{line}").expect("write the request");

        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = handle_connection(&mut daemon, ours);
            let _ = done.send(outcome.map_err(|err| err.kind()));
        });

        let mut reply = String::new();
        BufReader::new(&theirs)
            .read_line(&mut reply)
            .expect("read the reply");
        assert!(reply.contains(r#""status":"recorded""#), "{reply}");

        // Say nothing more, and do not hang up.
        let outcome = finished
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the daemon is still waiting on a client it already answered");
        assert_eq!(outcome, Ok(()));
        drop(theirs);
    }

    #[test]
    fn a_client_that_hangs_up_before_it_is_served_is_not_a_fault() {
        // The doctor's liveness probe and the daemon's own bind check connect
        // and leave without a word. Darwin refuses to set options on a socket
        // whose peer has already gone, with EINVAL, and the first build of the
        // silence bound reported every such probe as a failed connection — one
        // line in daemon.log per `conbus doctor`.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        let (ours, theirs) = UnixStream::pair().expect("socketpair");
        drop(theirs);

        let outcome = handle_connection(&mut daemon, ours).map_err(|err| err.kind());

        assert_eq!(outcome, Ok(()), "a peer that left is nobody to serve");
    }

    fn cleared_record(trigger: Option<&str>, desktop: bool) -> Record {
        Record {
            schema_version: conbus_core::record::SCHEMA_VERSION,
            ts: "2026-09-06T12:00:00.000Z".parse().expect("ts"),
            seq: 1,
            bus_id: BusId::mint(),
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some("s".into()),
                workspace: Some("/repo".into()),
                entrypoint: desktop.then(|| {
                    conbus_core::event::Entrypoint::new(
                        conbus_core::event::Entrypoint::CLAUDE_DESKTOP,
                    )
                }),
                backfilled_from: None,
                profile: None,
            },
            event: Event::SessionCandidate {
                trigger: trigger.map(str::to_string),
            },
        }
    }

    #[test]
    fn a_named_clear_and_a_desktop_omitted_source_are_both_forget() {
        assert!(session_was_cleared(&[cleared_record(Some("clear"), false)]));
        assert!(session_was_cleared(&[cleared_record(None, true)]));
        assert!(!session_was_cleared(&[cleared_record(
            Some("startup"),
            false
        )]));
        assert!(!session_was_cleared(&[cleared_record(None, false)]));
    }

    #[test]
    fn a_chat_export_dropped_after_start_is_absorbed() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(
            Some(root.path().join("conbus").to_str().expect("utf-8")),
            None,
            None,
        )
        .expect("paths");
        fs::create_dir_all(paths.log_dir()).expect("log");
        let log = SegmentLog::open(paths.log_dir(), Retention::default()).expect("open");
        let mut daemon =
            Daemon::new(log, Registry::new(), HashMap::new()).watch_chat_exports(&paths);
        assert_eq!(daemon.absorb_chat_exports().expect("empty"), 0);

        fs::write(
            paths.chat_export().join("conversations.json"),
            r#"{
                "conversations": [{
                    "uuid": "00000000-1111-2222-3333-444444444444",
                    "chat_messages": [
                        {"uuid": "m1", "sender": "human", "text": "from the chat window", "created_at": "2026-09-06T12:00:00.000Z"},
                        {"uuid": "m2", "sender": "assistant", "text": "heard", "created_at": "2026-09-06T12:01:00.000Z"}
                    ]
                }]
            }"#,
        )
        .expect("drop");
        let n = daemon.absorb_chat_exports().expect("absorb");
        assert!(n >= 2, "human + assistant: {n}");
        assert_eq!(daemon.absorb_chat_exports().expect("idempotent"), 0);

        let scan = SegmentLog::scan(paths.log_dir()).expect("scan");
        assert!(scan.records.iter().all(|r| {
            r.source.harness.as_str() == Harness::CLAUDE_CHAT
                && r.source.vendor_session_id.as_deref()
                    == Some("00000000-1111-2222-3333-444444444444")
        }));
        assert!(scan.records.iter().any(|r| matches!(
            &r.event,
            Event::TurnStarted { prompt: Body::Kept { text }, .. }
                if text == "from the chat window"
        )));
    }

    #[test]
    fn a_vendor_named_privacy_zip_in_the_drop_folder_is_absorbed() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(
            Some(root.path().join("conbus").to_str().expect("utf-8")),
            None,
            None,
        )
        .expect("paths");
        fs::create_dir_all(paths.log_dir()).expect("log");
        let log = SegmentLog::open(paths.log_dir(), Retention::default()).expect("open");
        let mut daemon =
            Daemon::new(log, Registry::new(), HashMap::new()).watch_chat_exports(&paths);
        assert_eq!(daemon.absorb_chat_exports().expect("empty"), 0);
        fs::write(
            paths.chat_export().join("conversations.json"),
            r#"[{"uuid":"00000000-aaaa-bbbb-cccc-ddddeeeeffff","chat_messages":[
                {"uuid":"m1","sender":"human","text":"privacy export","created_at":"2026-09-06T12:00:00.000Z"},
                {"uuid":"m2","sender":"assistant","text":"first class","created_at":"2026-09-06T12:01:00.000Z"}
            ]}]"#,
        )
        .expect("json");
        let zip = paths.chat_export().join("data-export-2026-09-06.zip");
        let status = std::process::Command::new("/usr/bin/zip")
            .args(["-q", "-j"])
            .arg(&zip)
            .arg(paths.chat_export().join("conversations.json"))
            .status()
            .expect("zip");
        assert!(status.success());
        let _ = fs::remove_file(paths.chat_export().join("conversations.json"));
        let n = daemon.absorb_chat_exports().expect("absorb");
        assert!(n >= 2, "{n}");
        let scan = SegmentLog::scan(paths.log_dir()).expect("scan");
        assert!(scan.records.iter().all(|r| {
            r.source.harness.as_str() == Harness::CLAUDE_CHAT
                && r.source.vendor_session_id.as_deref()
                    == Some("00000000-aaaa-bbbb-cccc-ddddeeeeffff")
        }));
    }

    #[test]
    fn a_vendor_named_privacy_zip_in_downloads_is_absorbed() {
        let root = tempfile::tempdir().expect("tempdir");
        let downloads = root.path().join("Downloads");
        fs::create_dir_all(&downloads).expect("downloads");
        let paths = Paths::from_env(
            Some(root.path().join("conbus").to_str().expect("utf-8")),
            None,
            None,
        )
        .expect("paths");
        fs::create_dir_all(paths.log_dir()).expect("log");
        let log = SegmentLog::open(paths.log_dir(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new())
            .watch_chat_export_sources(&paths, Some(downloads.clone()));
        assert_eq!(daemon.absorb_chat_exports().expect("empty"), 0);
        fs::write(
            downloads.join("conversations.json"),
            r#"[{"uuid":"00000000-dddd-eeee-ffff-000011112222","chat_messages":[
                {"uuid":"m1","sender":"human","text":"from downloads","created_at":"2026-09-06T12:00:00.000Z"},
                {"uuid":"m2","sender":"assistant","text":"kept uuid","created_at":"2026-09-06T12:01:00.000Z"}
            ]}]"#,
        )
        .expect("json");
        let zip = downloads.join("data-export-2026-09-07.zip");
        let status = std::process::Command::new("/usr/bin/zip")
            .args(["-q", "-j"])
            .arg(&zip)
            .arg(downloads.join("conversations.json"))
            .status()
            .expect("zip");
        assert!(status.success());
        let _ = fs::remove_file(downloads.join("conversations.json"));
        fs::write(downloads.join("random.zip"), b"PK not official").expect("noise");
        let n = daemon.absorb_chat_exports().expect("absorb");
        assert!(n >= 2, "{n}");
        let scan = SegmentLog::scan(paths.log_dir()).expect("scan");
        assert!(scan.records.iter().all(|r| {
            r.source.harness.as_str() == Harness::CLAUDE_CHAT
                && r.source.vendor_session_id.as_deref()
                    == Some("00000000-dddd-eeee-ffff-000011112222")
        }));
    }

    #[test]
    fn the_live_watch_never_opens_downloads() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(
            Some(root.path().join("conbus").to_str().expect("utf-8")),
            None,
            None,
        )
        .expect("paths");
        fs::create_dir_all(paths.log_dir()).expect("log");
        let log = SegmentLog::open(paths.log_dir(), Retention::default()).expect("open");
        let daemon = Daemon::new(log, Registry::new(), HashMap::new()).watch_chat_exports(&paths);
        let watch = daemon.chat_watch.expect("watch");
        assert_eq!(watch.dirs.len(), 1);
        assert_eq!(watch.dirs[0].dir, paths.chat_export());
        assert!(
            !watch
                .dirs
                .iter()
                .any(|dir| dir.dir.to_string_lossy().contains("Downloads"))
        );
    }

    #[test]
    fn nothing_in_the_daemon_speaks_around_the_stamped_channel() {
        // Every line this file writes must carry the time it happened, or
        // the next outage is again unplaceable. The needle is assembled at
        // runtime so this assertion's own source cannot satisfy it.
        let source = include_str!("daemon.rs");
        let bare = format!("{}{}", "eprint", "ln!");

        assert!(
            !source.contains(&bare),
            "the daemon must say everything through `note!`, which stamps it"
        );
    }

    #[test]
    fn a_leg_that_keeps_refusing_gets_one_decision_not_one_per_turn() {
        // Measured on 2026-09-08: one conversation wrote 146 `routed`
        // records in 38 minutes — one per `InterruptedQuota` from a leg that
        // kept refusing — spawned a `hop --apply` behind each, and left 32
        // headless Claude sessions leased. The decision stands until a
        // `continued` edge closes it.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        daemon.handle(request(Event::TurnStarted {
            author: None,
            prompt: Body::kept("work"),
        }));
        let refused = || {
            request(Event::TurnEnded {
                state: TurnState::InterruptedQuota {
                    reason: "rate_limit".into(),
                },
                assistant_message: None,
                tokens: None,
            })
        };

        for _ in 0..4 {
            daemon.handle(refused());
        }

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        let routed = scan
            .records
            .iter()
            .filter(|record| matches!(record.event, Event::Routed { .. }))
            .count();
        assert_eq!(routed, 1, "one transition, one decision");
    }

    #[test]
    fn a_daemon_that_is_busy_still_counts_as_serving() {
        // The probe's positive answer is silence: a daemon serving another
        // connection says nothing until it has our request, so the read
        // expiring is what proves it is there.
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("conbus.sock");
        let _live = bind(&socket).expect("bind");

        assert!(someone_is_serving(&socket));
    }

    #[test]
    fn a_socket_nobody_holds_is_not_serving() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("conbus.sock");
        std::fs::write(&socket, b"not a socket").expect("write");

        assert!(!someone_is_serving(&socket));
    }

    #[test]
    fn a_peer_that_hangs_up_as_we_arrive_is_not_serving() {
        // What a dying daemon looks like from here, and what used to read as
        // "already listening": the connection is accepted and then dropped.
        // Under load, closing a listening AF_UNIX socket leaves the path
        // answering for long enough to do exactly this.
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("conbus.sock");
        let listener = bind(&socket).expect("bind");
        let hangs_up = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                drop(stream);
            }
        });

        let serving = someone_is_serving(&socket);
        hangs_up.join().expect("accepted");

        assert!(
            !serving,
            "a peer that hung up is nobody to refuse a bind for"
        );
    }

    #[test]
    fn a_programs_quota_stop_is_not_a_routing_decision() {
        // `decide_driven` makes an unattended failover `Wait`, so the
        // `hop --apply` the daemon spawns declines and applies nothing. The
        // log used to keep the decision anyway, and count it as organic.
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new(), HashMap::new());
        let driven = |event| Request {
            entrypoint: Some(conbus_core::event::Entrypoint::new(
                conbus_core::event::Entrypoint::SDK_CLI,
            )),
            ..request(event)
        };
        daemon.handle(driven(Event::TurnStarted {
            author: None,
            prompt: Body::kept("nightly sweep"),
        }));

        daemon.handle(driven(Event::TurnEnded {
            state: TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            },
            assistant_message: None,
            tokens: None,
        }));

        let scan = SegmentLog::scan(dir.path()).expect("scan");
        assert!(
            !scan
                .records
                .iter()
                .any(|record| matches!(record.event, Event::Routed { .. })),
            "a program's quota stop must not record a hop nobody will take"
        );
    }
}
