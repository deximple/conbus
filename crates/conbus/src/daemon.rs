//! The recorder daemon: one socket, one registry, one append-only log.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use conbus_core::paths::Paths;
use conbus_core::record::Entry;
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
}

impl Daemon {
    /// Build a daemon around a log and the bindings it already has.
    ///
    /// The registry is a parameter rather than a fresh `Registry::new()`
    /// because a daemon that starts with empty bindings forks every live
    /// conversation: the vendor session keeps its id, the bus forgets which
    /// conversation it was, and its next prompt mints a second `bus_id`.
    /// `serve` restores it from the segments.
    pub fn new(log: SegmentLog, registry: Registry) -> Self {
        Daemon { registry, log }
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
                    backfilled_from: None,
                };
                let mut seqs = Vec::with_capacity(observed.len());
                let mut errors = Vec::new();
                // Each observation keeps the correlators its own payload had.
                // A deferred session start carried none; borrowing the minting
                // prompt's would fabricate a correlation the vendor did not send.
                for one in observed {
                    let entry = Entry {
                        bus_id,
                        observed_at: None,
                        source: source.clone(),
                        vendor_turn_id: one.vendor_turn_id,
                        vendor_agent_id: one.vendor_agent_id,
                        event: one.event,
                    };
                    match self.log.append(entry) {
                        Ok(Some(seq)) => seqs.push(seq),
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
                    Response::Withheld
                } else {
                    Response::Recorded { bus_id, seqs }
                }
            }
        }
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
    let log = SegmentLog::open(paths.log_dir(), retention)?;
    let mut daemon = Daemon::new(log, registry);

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

fn handle_connection(daemon: &mut Daemon, stream: UnixStream) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
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
    }
    Ok(())
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
            event,
        }
    }

    #[test]
    fn a_daemon_older_than_the_hook_refuses_instead_of_dropping_what_it_cannot_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new());

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
        let mut daemon = Daemon::new(log, Registry::new());

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
        let mut daemon = Daemon::new(log, Registry::new());

        let response = daemon.handle(request(Event::SessionCandidate {
            trigger: Some("startup".into()),
            cwd: None,
        }));

        assert_eq!(response, Response::Deferred);
        assert_eq!(SegmentLog::scan(dir.path()).expect("scan").records, vec![]);
    }

    #[test]
    fn the_first_prompt_writes_the_session_start_and_the_turn_under_one_conversation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log, Registry::new());
        daemon.handle(request(Event::SessionCandidate {
            trigger: Some("startup".into()),
            cwd: Some("/repo".into()),
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
        let mut daemon = Daemon::new(log, Registry::new());
        daemon.handle(Request {
            vendor_turn_id: None,
            ..request(Event::SessionCandidate {
                trigger: Some("startup".into()),
                cwd: Some("/repo".into()),
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
        let mut daemon = Daemon::new(log, Registry::new());

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
}
