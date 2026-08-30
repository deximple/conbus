//! The recorder daemon: one socket, one registry, one append-only log.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use conbus_core::paths::Paths;
use conbus_core::registry::{Disposition, Registry, SessionKey};
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
}

impl Daemon {
    pub fn new(log: SegmentLog) -> Self {
        Daemon {
            registry: Registry::new(),
            log,
        }
    }

    /// Record one observed event.
    pub fn handle(&mut self, request: Request) -> Response {
        let key = SessionKey::new(request.harness.clone(), request.vendor_session_id);
        match self.registry.observe(&key, request.event) {
            Disposition::Deferred => Response::Deferred,
            Disposition::Orphaned => Response::Orphaned,
            Disposition::Write { bus_id, events } => {
                let source = conbus_core::event::Source {
                    harness: request.harness,
                    harness_version: None,
                    vendor_session_id: Some(key.vendor_session_id.clone()),
                };
                let mut seqs = Vec::with_capacity(events.len());
                for event in events {
                    match self.log.append(bus_id, &source, event) {
                        Ok(Some(seq)) => seqs.push(seq),
                        Ok(None) => {}
                        Err(err) => {
                            return Response::Failed {
                                error: err.to_string(),
                            };
                        }
                    }
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
    let log = SegmentLog::open(paths.log_dir(), retention)?;
    let mut daemon = Daemon::new(log);

    eprintln!(
        "conbus daemon: socket {} log {} core_dumps_disabled={} memory_locked={}{}",
        paths.socket().display(),
        paths.log_dir().display(),
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
    use conbus_core::event::{Body, Event, Harness, TurnState};
    use conbus_core::segment::SegmentLog;

    fn request(event: Event) -> Request {
        Request {
            harness: Harness::new(Harness::CLAUDE_CODE),
            vendor_session_id: "abc123".into(),
            event,
        }
    }

    #[test]
    fn a_ghost_session_start_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log);

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
        let mut daemon = Daemon::new(log);
        daemon.handle(request(Event::SessionCandidate {
            trigger: Some("startup".into()),
            cwd: Some("/repo".into()),
        }));

        let response = daemon.handle(request(Event::TurnStarted {
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
    fn a_stop_without_a_prompt_is_reported_as_an_orphan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = SegmentLog::open(dir.path(), Retention::default()).expect("open");
        let mut daemon = Daemon::new(log);

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
