//! `conbus hop` — decide the next leg, and record the decision when asked.
//!
//! The vendor TUI stays the window. This command does not open a chat. It
//! prints where the next turn should go, and `--apply` writes that as a
//! `routed` line under the predecessor so the kill criteria can start counting.
//! When the next leg is Codex it also opens — or syncs — a bus-owned thread
//! so the hop is not a cold start. That write is not a model turn.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{SystemTime, UNIX_EPOCH};

use conbus_core::event::{Event, Harness, Hop, TurnState};
use conbus_core::ids::BusId;
use conbus_core::pack::Chapter;
use conbus_core::paths::Paths;
use conbus_core::record::Record;
use conbus_core::route::{self, Decision, Leg};
use conbus_core::segment::SegmentLog;
use conbus_core::summary;

use crate::quota::{self, Quota};
use crate::warm;
use crate::wire::{self, Request, Response};

/// Decide from the log and the Codex probe. `from` is a `bus_id` prefix.
pub fn decide_from(records: &[Record], from: Option<&str>) -> io::Result<String> {
    let (current, other, decision) = plan(records, from)?;
    Ok(decision.describe(&current.harness)
        + &match (&decision, other.harness.as_str()) {
            (Decision::Stay, _) => String::new(),
            (_, name) => format!(" · other {name} reached={}", other.reached),
        })
}

pub fn run(paths: &Paths, from: Option<String>, apply: bool) -> io::Result<()> {
    let scan = SegmentLog::scan(paths.log_dir())?;
    let (current, other, decision) = plan(&scan.records, from.as_deref())?;
    println!("route     {}", decision.describe(&current.harness));
    if let Decision::Hop { to, hop, reason } = &decision {
        println!("next      {} · {reason}", to.as_str());
        if apply {
            apply_hop(
                paths,
                &scan.records,
                from.as_deref(),
                &current,
                to,
                *hop,
                reason,
            )?;
        } else {
            println!(
                "record    conbus hop --apply{}",
                match &from {
                    Some(id) => format!(" --from {id}"),
                    None => String::new(),
                }
            );
        }
    }
    let _ = other;
    Ok(())
}

fn plan(records: &[Record], from: Option<&str>) -> io::Result<(Leg, Leg, Decision)> {
    let focus = focus(records, from).ok_or_else(|| {
        io::Error::other("no conversation to route — pass --from <bus_id> or record a turn first")
    })?;
    let now = now_secs();
    let quota = quota::read().ok();
    let current = leg(
        records,
        &focus.harness,
        focus.last_turn.clone(),
        quota.as_ref(),
        now,
    );
    let other_harness =
        route::opposite(&focus.harness).unwrap_or_else(|| Harness::new(Harness::CODEX));
    let other_turn = latest_turn(records, other_harness.as_str());
    let other = leg(records, &other_harness, other_turn, quota.as_ref(), now);
    let decision = route::decide(&current, &other);
    Ok((current, other, decision))
}

struct Focus {
    harness: Harness,
    last_turn: Option<TurnState>,
    vendor_session_id: Option<String>,
    workspace: Option<String>,
    bus_id: BusId,
}

fn focus(records: &[Record], from: Option<&str>) -> Option<Focus> {
    let wanted = from.map(|prefix| prefix.to_string());
    let mut chosen: Option<&Record> = None;
    for record in records {
        if let Some(prefix) = &wanted {
            if !record.bus_id.to_string().starts_with(prefix.as_str()) {
                continue;
            }
        } else if record.source.backfilled_from.is_some() {
            continue;
        }
        if matches!(
            record.event,
            Event::TurnStarted { .. } | Event::TurnEnded { .. }
        ) {
            chosen = Some(record);
        }
    }
    let record = chosen?;
    Some(Focus {
        harness: record.source.harness.clone(),
        last_turn: latest_turn_on(records, record.bus_id),
        vendor_session_id: record.source.vendor_session_id.clone(),
        workspace: record.source.workspace.clone(),
        bus_id: record.bus_id,
    })
}

fn latest_turn(records: &[Record], harness: &str) -> Option<TurnState> {
    records
        .iter()
        .rev()
        .find(|record| {
            record.source.harness.as_str() == harness
                && matches!(record.event, Event::TurnEnded { .. })
        })
        .and_then(|record| match &record.event {
            Event::TurnEnded { state, .. } => Some(state.clone()),
            _ => None,
        })
}

fn latest_turn_on(records: &[Record], bus_id: BusId) -> Option<TurnState> {
    records
        .iter()
        .rev()
        .find(|record| record.bus_id == bus_id && matches!(record.event, Event::TurnEnded { .. }))
        .and_then(|record| match &record.event {
            Event::TurnEnded { state, .. } => Some(state.clone()),
            _ => None,
        })
}

fn leg(
    _records: &[Record],
    harness: &Harness,
    last_turn: Option<TurnState>,
    quota: Option<&Quota>,
    now: i64,
) -> Leg {
    let mut used_percent = None;
    let mut minutes_to_reset = None;
    let mut reached = false;
    if harness.as_str() == Harness::CODEX
        && let Some(quota) = quota
    {
        used_percent = quota.primary.as_ref().map(|w| w.used_percent);
        minutes_to_reset = quota.primary.as_ref().map(|w| (w.resets_at - now) / 60);
        reached = quota.reached.is_some();
    }
    Leg {
        harness: harness.clone(),
        last_turn,
        used_percent,
        minutes_to_reset,
        reached,
    }
}

fn apply_hop(
    paths: &Paths,
    records: &[Record],
    from: Option<&str>,
    current: &Leg,
    to: &Harness,
    hop: Hop,
    reason: &str,
) -> io::Result<()> {
    let focus = predecessor(records, from, current)?;
    let kind = match hop {
        Hop::Displaced => "displaced",
        Hop::HandedOff => "handed_off",
    };

    if to.as_str() == Harness::CODEX {
        let text = digest(records, focus.bus_id);
        match lease_of(records, focus.bus_id) {
            Lease::Held(holder) => {
                return Err(io::Error::other(format!(
                    "this conversation is already leased ({holder}); wait for it to end"
                )));
            }
            Lease::Warm(id) => {
                warm::sync(&id, &text)?;
                println!("warm      synced {id}");
                return Ok(());
            }
            Lease::Free => {
                let thread = warm::open(focus.workspace.as_deref(), &text)?;
                record_hop(paths, &focus, to, hop, reason, &warm::holder(&thread.id))?;
                println!("recorded  routed {kind}");
                println!("warm      opened {}", thread.id);
                return Ok(());
            }
        }
    }

    if matches!(lease_of(records, focus.bus_id), Lease::Held(_)) {
        return Err(io::Error::other(
            "this conversation is already leased for a hop; wait for it to end",
        ));
    }
    record_hop(paths, &focus, to, hop, reason, "conbus-hop")?;
    println!("recorded  routed {kind}");
    println!(
        "open      the {} TUI in this checkout — the bus does not drive the vendor window",
        to.as_str()
    );
    Ok(())
}

fn predecessor(records: &[Record], from: Option<&str>, current: &Leg) -> io::Result<Focus> {
    focus(records, from)
        .filter(|chosen| chosen.harness == current.harness)
        .or_else(|| {
            records.iter().rev().find_map(|record| {
                (record.source.harness == current.harness).then(|| Focus {
                    harness: record.source.harness.clone(),
                    last_turn: current.last_turn.clone(),
                    vendor_session_id: record.source.vendor_session_id.clone(),
                    workspace: record.source.workspace.clone(),
                    bus_id: record.bus_id,
                })
            })
        })
        .ok_or_else(|| io::Error::other("no predecessor session to record under"))
}

fn digest(records: &[Record], bus_id: BusId) -> String {
    let chapter: Vec<&Record> = records
        .iter()
        .filter(|record| record.bus_id == bus_id)
        .collect();
    match Chapter::read(&chapter) {
        Some(chapter) => summary::extract(&chapter),
        None => format!("conbus digest · {bus_id}"),
    }
}

/// Live Codex threads the bus still holds a lease on.
pub fn live_warm_threads(records: &[Record]) -> Vec<String> {
    let mut by_bus = std::collections::HashMap::<BusId, Option<String>>::new();
    for record in records {
        match &record.event {
            Event::Leased { holder } => {
                by_bus.insert(record.bus_id, warm::thread_id(holder).map(str::to_string));
            }
            Event::SessionEnded { .. } => {
                by_bus.insert(record.bus_id, None);
            }
            _ => {}
        }
    }
    by_bus.into_values().flatten().collect()
}

fn record_hop(
    paths: &Paths,
    focus: &Focus,
    to: &Harness,
    hop: Hop,
    reason: &str,
    holder: &str,
) -> io::Result<()> {
    let event = Event::Routed {
        next: to.as_str().to_string(),
        hop,
        reason: reason.to_string(),
    };
    let Some(session) = focus.vendor_session_id.clone() else {
        return Err(io::Error::other(
            "predecessor has no vendor session id; cannot record a hop",
        ));
    };

    let request = Request {
        protocol_version: Some(wire::PROTOCOL_VERSION),
        harness: focus.harness.clone(),
        harness_version: None,
        vendor_session_id: session,
        vendor_turn_id: None,
        vendor_agent_id: None,
        workspace: focus.workspace.clone(),
        entrypoint: None,
        brief: false,
        event,
    };
    match submit(paths, &request) {
        Ok(Response::Recorded { .. } | Response::Briefed { .. }) => {
            let _ = submit(
                paths,
                &Request {
                    event: Event::Leased {
                        holder: holder.into(),
                    },
                    brief: false,
                    ..request.clone()
                },
            );
            Ok(())
        }
        Ok(Response::Orphaned) => Err(io::Error::other(
            "daemon has no binding for this session; resume it or restart the daemon",
        )),
        Ok(other) => Err(io::Error::other(format!(
            "daemon refused the hop: {other:?}"
        ))),
        Err(err) => Err(io::Error::other(format!(
            "daemon not listening ({err}); hops are recorded through the daemon"
        ))),
    }
}

enum Lease {
    Free,
    Warm(String),
    Held(String),
}

fn lease_of(records: &[Record], bus_id: BusId) -> Lease {
    let mut lease = Lease::Free;
    for record in records {
        if record.bus_id != bus_id {
            continue;
        }
        match &record.event {
            Event::Leased { holder } => {
                lease = match warm::thread_id(holder) {
                    Some(id) => Lease::Warm(id.to_string()),
                    None => Lease::Held(holder.clone()),
                };
            }
            Event::SessionEnded { .. } => lease = Lease::Free,
            _ => {}
        }
    }
    lease
}

fn submit(paths: &Paths, request: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(paths.socket())?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(500)));
    writeln!(
        stream,
        "{}",
        serde_json::to_string(request).map_err(io::Error::other)?
    )?;
    stream.flush()?;
    let mut lines = BufReader::new(stream).lines();
    let line = lines
        .next()
        .ok_or_else(|| io::Error::other("daemon closed without answering"))??;
    serde_json::from_str(&line).map_err(io::Error::other)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
