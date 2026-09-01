//! The recorder daemon: one socket, one registry, one append-only log.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use conbus_core::continuation::{Asking, Decision, decide, opening_units, unattended};
use conbus_core::event::{Event, Source};
use conbus_core::ids::BusId;
use conbus_core::pack::{self, Chapter};
use conbus_core::paths::Paths;
use conbus_core::record::{Entry, Record};
use conbus_core::registry::{Disposition, Observed, Registry, SessionKey};
use conbus_core::retention::Retention;
use conbus_core::segment::{Scan, SegmentLog};

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
        }
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
            eprintln!(
                "conbus daemon: refusing a hook speaking protocol v{}; this daemon speaks v{}",
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

        let key = SessionKey::new(request.harness.clone(), request.vendor_session_id);
        let observed = Observed {
            vendor_turn_id: request.vendor_turn_id,
            vendor_agent_id: request.vendor_agent_id,
            workspace: request.workspace,
            event: request.event,
        };
        match self.registry.observe(&key, observed) {
            Disposition::Deferred => Response::Deferred,
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
                for one in observed {
                    let entry = Entry {
                        bus_id,
                        observed_at: None,
                        source: Source {
                            workspace: one.workspace,
                            ..source.clone()
                        },
                        vendor_turn_id: one.vendor_turn_id,
                        vendor_agent_id: one.vendor_agent_id,
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
                    eprintln!(
                        "conbus daemon: {} of {} record(s) refused by the log: {}",
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
                let briefing = first_prompt.then(|| self.brief(bus_id, &written)).flatten();
                match briefing {
                    Some((context, message, event)) => {
                        // Appended after the turn it belongs to, so the log
                        // reads in the order things happened. If this write
                        // fails the briefing still goes out — the person is
                        // better served by the context than by our bookkeeping
                        // — and the daemon says so on the channel launchd
                        // keeps.
                        match self.log.append(Entry {
                            bus_id,
                            observed_at: None,
                            source: source.clone(),
                            vendor_turn_id: None,
                            vendor_agent_id: None,
                            event,
                        }) {
                            Ok(Some(record)) => {
                                seqs.push(record.seq);
                                written.push(record);
                            }
                            Ok(None) => {}
                            Err(err) => {
                                eprintln!("conbus daemon: briefing sent but not recorded: {err}")
                            }
                        }
                        self.remember(&written);
                        Response::Briefed {
                            bus_id,
                            seqs,
                            context,
                            message,
                        }
                    }
                    None => {
                        self.remember(&written);
                        Response::Recorded { bus_id, seqs }
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
        let Decision::Brief { recent } =
            decide(&asking, &candidates, self.home.as_deref(), prompt.ts)
        else {
            return None;
        };

        // In the order the decision named them. A briefing that reorders itself
        // is one whose leading item — the conversation that could not continue
        // — stops leading.
        let chapters: Vec<Chapter> = recent
            .iter()
            .filter_map(|id| self.chapters.get(id).cloned())
            .collect();
        let packed = pack::brief(bus_id, &chapters, prompt.ts, pack::PACK_BUDGET_UNITS);
        if packed.is_empty() {
            return None;
        }

        let message = format!(
            "conbus · {} earlier session{} in this checkout · conbus log --bus-id {}",
            chapters.len(),
            if chapters.len() == 1 { "" } else { "s" },
            &bus_id.to_string()[..8],
        );
        let event = Event::Briefed {
            about: chapters.iter().map(|c| c.about.bus_id).collect(),
            units: packed.units,
            dropped: packed.dropped,
        };
        Some((packed.text, message, event))
    }
}

/// Bind the control socket, refusing to steal one a live daemon still owns.
pub fn bind(socket: &Path) -> io::Result<UnixListener> {
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
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
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Run the daemon until the process is stopped.
pub fn serve(paths: &Paths, retention: Retention) -> io::Result<()> {
    let report = harden::harden();
    let listener = bind(&paths.socket())?;
    // The registry is derived state, so it is rebuilt from the segments rather
    // than starting empty — otherwise a restart forks every live conversation.
    // But a log we cannot read must not stop us recording: an empty registry
    // costs a forked conversation, which the vendor session id on every record
    // still makes recoverable, while refusing to start costs every future turn.
    let scan = SegmentLog::scan(paths.log_dir()).unwrap_or_else(|err| {
        eprintln!(
            "conbus daemon: could not read {} ({err}); starting with no bindings, \
             so a session that outlived the last daemon will get a second bus_id",
            paths.log_dir().display()
        );
        Scan::default()
    });
    let registry = Registry::restore(&scan.records);
    let chapters = Daemon::chapters(&scan.records);
    let log = SegmentLog::open(paths.log_dir(), retention)?;
    let mut daemon = Daemon::new(log, registry, chapters);

    eprintln!(
        "conbus daemon: socket {} log {} ({} record(s), {} unreadable) core_dumps_disabled={} memory_locked={}{}",
        paths.socket().display(),
        paths.log_dir().display(),
        scan.records.len(),
        scan.defects.len(),
        report.core_dumps_disabled,
        report.memory_locked,
        report
            .memory_lock_error
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default(),
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_connection(&mut daemon, stream) {
                    eprintln!("conbus daemon: connection failed: {err}");
                }
            }
            Err(err) => eprintln!("conbus daemon: accept failed: {err}"),
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::event::{Body, Event, Harness, TurnState, VendorTurnId};
    use conbus_core::segment::SegmentLog;

    fn request(event: Event) -> Request {
        Request {
            protocol_version: Some(crate::wire::PROTOCOL_VERSION),
            harness: Harness::new(Harness::CLAUDE_CODE),
            harness_version: None,
            vendor_session_id: "abc123".into(),
            vendor_turn_id: None,
            vendor_agent_id: None,
            workspace: Some("/repo".into()),
            entrypoint: None,
            event,
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
}
