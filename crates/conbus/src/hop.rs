//! `conbus hop` — decide the next leg, and record the decision when asked.
//!
//! The vendor TUI stays the window. This command does not open a chat UI.
//! `--apply` writes takeover for `conbus run` and opens the destination:
//! bus-owned warms (`codex://threads/{id}`, `claude://code/continue?session={id}`),
//! or Cursor's official `cursor -n <workspace>`.
//! A hop opens — or syncs — a bus-owned warm on both Claude/Codex legs so
//! neither direction is a cold start. Those writes are not model turns.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{SystemTime, UNIX_EPOCH};

use conbus_core::attach;
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

/// Decide from the log and the quota observations. `from` is a `bus_id` prefix.
pub fn decide_from(paths: &Paths, records: &[Record], from: Option<&str>) -> io::Result<String> {
    let (current, other, decision) = plan(paths, records, from)?;
    Ok(decision.describe(&current.harness)
        + &match (&decision, other.harness.as_str()) {
            (Decision::Stay, _) => String::new(),
            (_, name) => format!(" · other {name} reached={}", other.reached),
        })
}

pub fn run(paths: &Paths, from: Option<String>, to: Option<String>, apply: bool) -> io::Result<()> {
    let scan = SegmentLog::scan(paths.log_dir())?;
    let (current, other, automatic) = plan(paths, &scan.records, from.as_deref())?;
    let decision = match to.as_deref() {
        Some(name) => directed(&current, &other, name)?,
        None => automatic,
    };
    println!("route     {}", decision.describe(&current.harness));
    let workspace = focus(&scan.records, from.as_deref()).and_then(|f| f.workspace);
    let freshness = crate::code::probe(workspace.as_deref());
    println!("code      {}", freshness.line());
    if let Decision::Hop {
        to: next,
        hop,
        reason,
    } = &decision
    {
        println!("next      {} · {reason}", next.as_str());
        if apply {
            apply_hop(
                paths,
                &scan.records,
                from.as_deref(),
                &current,
                next,
                *hop,
                reason,
            )?;
        } else {
            println!(
                "record    conbus hop --apply{}",
                match (&from, &to) {
                    (Some(id), Some(dest)) => format!(" --from {id} --to {dest}"),
                    (Some(id), None) => format!(" --from {id}"),
                    (None, Some(dest)) => format!(" --to {dest}"),
                    (None, None) => String::new(),
                }
            );
        }
    } else if apply && let Some(name) = to.as_deref() {
        resume_here(paths, &scan.records, from.as_deref(), name)?;
    }
    let _ = other;
    Ok(())
}

/// A recorded failover, not a live `--apply`. Opens no vendor window.
pub fn demo() -> io::Result<()> {
    let records = demo_records();
    let meter = conbus_core::meter::Meter::fold(&records);
    println!("demo      recorded failover (no vendor window, no --apply)");
    println!("bus       {}", records[0].bus_id);
    println!("from      claude_code · StopFailure rate_limit");
    println!("route     hop to codex · displaced · rate_limit");
    println!("next      codex");
    println!(
        "meter     routing {} · failover {} · edges failover {}",
        meter.routing_hops, meter.failover_hops, meter.failover_edges
    );
    println!("record    conbus hop --from {} --apply", records[0].bus_id);
    println!("then      conbus run   # same TTY; do not start the vendor yourself");
    println!("docs      docs/hop-demo.md");
    Ok(())
}

fn demo_records() -> Vec<Record> {
    let bus_id = "01a072b0-5105-7243-b58e-1d9b92e5889e"
        .parse()
        .expect("demo bus");
    let source = |harness: &str| conbus_core::event::Source {
        harness: Harness::new(harness),
        harness_version: None,
        vendor_session_id: Some("s-demo".into()),
        workspace: Some("/repo".into()),
        entrypoint: None,
        backfilled_from: None,
    };
    let rec = |seq, ts: &str, harness: &str, event| Record {
        schema_version: conbus_core::record::SCHEMA_VERSION,
        ts: ts.parse().expect("ts"),
        seq,
        bus_id,
        vendor_turn_id: None,
        vendor_agent_id: None,
        source: source(harness),
        event,
    };
    vec![
        rec(
            1,
            "2026-09-03T18:00:00.000Z",
            Harness::CLAUDE_CODE,
            Event::TurnStarted {
                author: Some(conbus_core::event::Author::new(
                    conbus_core::event::Author::TYPED,
                )),
                prompt: conbus_core::event::Body::kept("AUTOHOP"),
            },
        ),
        rec(
            2,
            "2026-09-03T18:00:01.000Z",
            Harness::CLAUDE_CODE,
            Event::Routed {
                next: Harness::CODEX.into(),
                hop: Hop::Displaced,
                reason: "rate_limit".into(),
                code_freshness: None,
            },
        ),
        rec(
            3,
            "2026-09-03T18:00:02.000Z",
            Harness::CODEX,
            Event::Continued {
                predecessor: bus_id,
                hop: Hop::Displaced,
            },
        ),
        rec(
            4,
            "2026-09-03T18:00:03.000Z",
            Harness::CODEX,
            Event::TurnStarted {
                author: Some(conbus_core::event::Author::new(
                    conbus_core::event::Author::TYPED,
                )),
                prompt: conbus_core::event::Body::kept("AUTOHOP2"),
            },
        ),
    ]
}

/// A person named the next leg. That is a routing hop, not a failover.
fn directed(current: &Leg, other: &Leg, name: &str) -> io::Result<Decision> {
    let slug = attach::resolve(name).ok_or_else(|| {
        let catalog = attach::slugs().collect::<Vec<_>>().join(", ");
        io::Error::other(format!("unknown harness {name}; catalog is {catalog}"))
    })?;
    let to = Harness::new(slug);
    if current.harness.as_str() == to.as_str() {
        return Ok(Decision::Stay);
    }
    if other.harness == to && other.reached {
        return Ok(Decision::Wait {
            reason: "destination exhausted".into(),
        });
    }
    Ok(Decision::Hop {
        to,
        hop: Hop::HandedOff,
        reason: "directed".into(),
    })
}

fn plan(paths: &Paths, records: &[Record], from: Option<&str>) -> io::Result<(Leg, Leg, Decision)> {
    let focus = focus(records, from).ok_or_else(|| {
        io::Error::other("no conversation to route — pass --from <bus_id> or record a turn first")
    })?;
    let now = now_secs();
    let current = leg(paths, &focus.harness, focus.last_turn.clone(), now);
    let other_harness = route::overflow_target(&focus.harness)
        .or_else(|| route::opposite(&focus.harness))
        .unwrap_or_else(|| Harness::new(Harness::CODEX));
    let other_turn = latest_turn(records, other_harness.as_str());
    let other = leg(paths, &other_harness, other_turn, now);
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

fn observed_quota(paths: &Paths, harness: &str) -> Option<Quota> {
    match harness {
        Harness::CODEX => quota::read().ok(),
        Harness::CLAUDE_CODE => quota::read_cached(paths, Harness::CLAUDE_CODE),
        Harness::AGY => quota::read_agy().ok(),
        _ => None,
    }
}

fn leg(paths: &Paths, harness: &Harness, last_turn: Option<TurnState>, now: i64) -> Leg {
    let mut used_percent = None;
    let mut minutes_to_reset = None;
    let mut reached = false;
    if let Some(quota) = observed_quota(paths, harness.as_str()) {
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
    let code_freshness = Some(crate::code::probe(focus.workspace.as_deref()).line());
    let kind = match hop {
        Hop::Displaced => "displaced",
        Hop::HandedOff => "handed_off",
    };

    if matches!(lease_of(records, focus.bus_id), Lease::Held(_)) {
        return Err(io::Error::other(
            "this conversation is already leased for a hop; wait for it to end",
        ));
    }

    let text = digest(records, focus.bus_id);
    // Destination first. Overflow to Codex was waiting on `claude -p`
    // (MCP spawn) before `codex://` could open. The other leg still
    // warms after the vendor window is up; a failure there is not a
    // failed hop.
    match to.as_str() {
        Harness::CODEX => {
            let thread = take_codex_warm(paths, records, &focus, &text)?;
            finish_hop(
                paths,
                records,
                &focus,
                to,
                hop,
                reason,
                kind,
                Some(&warm::holder(&thread)),
                Some(&thread),
                code_freshness,
            )?;
            if let Err(err) = ensure_claude_warm(paths, records, &focus, &text) {
                println!("warm      claude skipped ({err})");
            }
        }
        Harness::CLAUDE_CODE => {
            let thread = ensure_claude_warm(paths, records, &focus, &text)?;
            finish_hop(
                paths,
                records,
                &focus,
                to,
                hop,
                reason,
                kind,
                Some(&warm::claude_holder(&thread)),
                Some(&thread),
                code_freshness,
            )?;
            if let Err(err) = take_codex_warm(paths, records, &focus, &text) {
                println!("warm      codex skipped ({err})");
            }
        }
        _ => {
            if let Err(err) = take_codex_warm(paths, records, &focus, &text) {
                println!("warm      codex skipped ({err})");
            }
            if let Err(err) = ensure_claude_warm(paths, records, &focus, &text) {
                println!("warm      claude skipped ({err})");
            }
            finish_hop(
                paths,
                records,
                &focus,
                to,
                hop,
                reason,
                kind,
                None,
                None,
                code_freshness,
            )?;
        }
    }
    Ok(())
}

fn take_codex_warm(
    paths: &Paths,
    records: &[Record],
    focus: &Focus,
    text: &str,
) -> io::Result<String> {
    let had_codex = matches!(lease_of(records, focus.bus_id), Lease::Warm(_));
    let codex = ensure_codex_warm(records, focus, text)?;
    if !had_codex {
        lease_only(paths, focus, &warm::holder(&codex))?;
    }
    Ok(codex)
}

fn finish_hop(
    paths: &Paths,
    records: &[Record],
    focus: &Focus,
    to: &Harness,
    hop: Hop,
    reason: &str,
    kind: &str,
    holder: Option<&str>,
    thread: Option<&str>,
    code_freshness: Option<String>,
) -> io::Result<()> {
    if routed_to(records, focus.bus_id, to) {
        return handoff(paths, focus, to.as_str(), thread);
    }
    record_hop(paths, focus, to, hop, reason, holder, code_freshness)?;
    println!("recorded  routed {kind}");
    handoff(paths, focus, to.as_str(), thread)
}

fn ensure_claude_warm(
    paths: &Paths,
    records: &[Record],
    focus: &Focus,
    text: &str,
) -> io::Result<String> {
    match claude_warm_of(records, focus.bus_id) {
        Some(id) => {
            warm::sync_claude(&id, text)?;
            println!("warm      claude synced {id}");
            Ok(id)
        }
        None => {
            let thread = warm::open_claude(focus.workspace.as_deref(), text)?;
            lease_only(paths, focus, &warm::claude_holder(&thread.id))?;
            println!("warm      claude opened {}", thread.id);
            Ok(thread.id)
        }
    }
}

fn ensure_codex_warm(records: &[Record], focus: &Focus, text: &str) -> io::Result<String> {
    match lease_of(records, focus.bus_id) {
        Lease::Warm(id) => {
            warm::sync(&id, text)?;
            println!("warm      synced {id}");
            Ok(id)
        }
        Lease::Free => {
            let thread = warm::open(focus.workspace.as_deref(), text)?;
            println!("warm      opened {}", thread.id);
            Ok(thread.id)
        }
        Lease::Held(holder) => Err(io::Error::other(format!(
            "this conversation is already leased ({holder}); wait for it to end"
        ))),
    }
}

fn handoff(paths: &Paths, focus: &Focus, next: &str, thread: Option<&str>) -> io::Result<()> {
    crate::takeover::write(
        paths,
        &crate::takeover::Takeover {
            next: next.into(),
            thread: thread.map(str::to_string),
            bus_id: focus.bus_id.to_string(),
            cwd: focus.workspace.clone(),
        },
    )?;
    match crate::landing::enter(next, focus.workspace.as_deref(), thread) {
        Ok(true) => match thread {
            Some(id) => println!("window    opened {next} · {id}"),
            None => println!("window    opened {next}"),
        },
        Ok(false) => {
            println!("window    takeover → {next} — conbus run execs the next leg in this TTY")
        }
        Err(err) => println!(
            "window    takeover → {next} — desktop open failed ({err}); conbus run still execs the TTY"
        ),
    }
    Ok(())
}

/// Same-leg resume: reopen the destination without recording a second hop.
fn resume_here(
    paths: &Paths,
    records: &[Record],
    from: Option<&str>,
    name: &str,
) -> io::Result<()> {
    let slug = attach::resolve(name).ok_or_else(|| {
        let catalog = attach::slugs().collect::<Vec<_>>().join(", ");
        io::Error::other(format!("unknown harness {name}; catalog is {catalog}"))
    })?;
    let Some(chosen) = focus(records, from) else {
        return Err(io::Error::other(
            "no conversation to resume — pass --from <bus_id> or record a turn first",
        ));
    };
    let thread = match slug {
        Harness::CODEX => match lease_of(records, chosen.bus_id) {
            Lease::Warm(id) => Some(id),
            _ => None,
        },
        Harness::CLAUDE_CODE => claude_warm_of(records, chosen.bus_id),
        _ => None,
    };
    handoff(paths, &chosen, slug, thread.as_deref())
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
    if let Some(stored) = crate::summarise::latest_digest(&chapter) {
        return stored;
    }
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
                if let Some(id) = warm::thread_id(holder) {
                    by_bus.insert(record.bus_id, Some(id.to_string()));
                }
            }
            Event::SessionEnded { .. } => {
                if by_bus
                    .get(&record.bus_id)
                    .and_then(|id| id.as_deref())
                    .is_none()
                {
                    by_bus.insert(record.bus_id, None);
                }
            }
            _ => {}
        }
    }
    by_bus.into_values().flatten().collect()
}

/// Live Claude sessions the bus still holds a lease on.
pub fn live_claude_sessions(records: &[Record]) -> Vec<String> {
    let mut by_bus = std::collections::HashMap::<BusId, Option<String>>::new();
    for record in records {
        if let Event::Leased { holder } = &record.event
            && let Some(id) = warm::claude_id(holder)
        {
            by_bus.insert(record.bus_id, Some(id.to_string()));
        }
    }
    by_bus.into_values().flatten().collect()
}

fn routed_to(records: &[Record], bus_id: BusId, to: &Harness) -> bool {
    records
        .iter()
        .rev()
        .find_map(|record| {
            if record.bus_id != bus_id {
                return None;
            }
            match &record.event {
                Event::Routed { next, .. } => Some(next == to.as_str()),
                _ => None,
            }
        })
        .unwrap_or(false)
}

fn lease_only(paths: &Paths, focus: &Focus, holder: &str) -> io::Result<()> {
    let Some(session) = focus.vendor_session_id.clone() else {
        return Err(io::Error::other(
            "predecessor has no vendor session id; cannot lease a warm thread",
        ));
    };
    match submit(
        paths,
        &Request {
            protocol_version: Some(wire::PROTOCOL_VERSION),
            harness: focus.harness.clone(),
            harness_version: None,
            vendor_session_id: session,
            vendor_turn_id: None,
            vendor_agent_id: None,
            workspace: focus.workspace.clone(),
            entrypoint: None,
            brief: false,
            event: Event::Leased {
                holder: holder.into(),
            },
        },
    ) {
        Ok(Response::Recorded { .. } | Response::Briefed { .. }) => Ok(()),
        Ok(Response::Orphaned) => Err(io::Error::other(
            "daemon has no binding for this session; resume it or restart the daemon",
        )),
        Ok(other) => Err(io::Error::other(format!(
            "daemon refused the lease: {other:?}"
        ))),
        Err(err) => Err(io::Error::other(format!(
            "daemon not listening ({err}); hops are recorded through the daemon"
        ))),
    }
}

fn record_hop(
    paths: &Paths,
    focus: &Focus,
    to: &Harness,
    hop: Hop,
    reason: &str,
    holder: Option<&str>,
    code_freshness: Option<String>,
) -> io::Result<()> {
    let event = Event::Routed {
        next: to.as_str().to_string(),
        hop,
        reason: reason.to_string(),
        code_freshness,
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
            if let Some(holder) = holder {
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
            }
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

fn claude_warm_of(records: &[Record], bus_id: BusId) -> Option<String> {
    let mut id = None;
    for record in records {
        if record.bus_id != bus_id {
            continue;
        }
        if let Event::Leased { holder } = &record.event
            && let Some(session) = warm::claude_id(holder)
        {
            id = Some(session.to_string());
        }
    }
    id
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
                    None if warm::claude_id(holder).is_some() => match lease {
                        Lease::Warm(id) => Lease::Warm(id),
                        other => other,
                    },
                    None => Lease::Held(holder.clone()),
                };
            }
            Event::SessionEnded { .. } => {
                if !matches!(lease, Lease::Warm(_)) {
                    lease = Lease::Free;
                }
            }
            _ => {}
        }
    }
    lease
}

pub(crate) fn submit(paths: &Paths, request: &Request) -> io::Result<Response> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> Leg {
        Leg {
            harness: Harness::new(Harness::CLAUDE_CODE),
            last_turn: Some(TurnState::Complete),
            used_percent: None,
            minutes_to_reset: None,
            reached: false,
        }
    }

    fn codex() -> Leg {
        Leg {
            harness: Harness::new(Harness::CODEX),
            last_turn: Some(TurnState::Complete),
            used_percent: Some(0),
            minutes_to_reset: Some(300),
            reached: false,
        }
    }

    #[test]
    fn a_quota_routed_line_is_not_a_second_decision() {
        let bus_id = "01a07243-a666-7d50-b64b-773b257d06e1".parse().expect("id");
        let to = Harness::new(Harness::CODEX);
        let routed = Record {
            schema_version: conbus_core::record::SCHEMA_VERSION,
            ts: "2026-09-06T00:00:00.000Z".parse().expect("ts"),
            seq: 1,
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: conbus_core::event::Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some("s".into()),
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
            },
            event: Event::Routed {
                next: Harness::CODEX.into(),
                hop: Hop::Displaced,
                reason: "rate_limit".into(),
                code_freshness: None,
            },
        };
        assert!(routed_to(&[routed], bus_id, &to));
    }

    #[test]
    fn a_directed_hop_is_a_routing_hop_not_a_failover() {
        match directed(&claude(), &codex(), Harness::CODEX).expect("ok") {
            Decision::Hop { to, hop, reason } => {
                assert_eq!(to.as_str(), Harness::CODEX);
                assert_eq!(hop, Hop::HandedOff);
                assert_eq!(reason, "directed");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn directing_at_the_current_leg_stays() {
        assert_eq!(
            directed(&claude(), &codex(), Harness::CLAUDE_CODE).expect("ok"),
            Decision::Stay
        );
    }

    #[test]
    fn directing_into_an_exhausted_leg_waits() {
        let mut other = codex();
        other.reached = true;
        match directed(&claude(), &other, Harness::CODEX).expect("ok") {
            Decision::Wait { reason } => assert!(reason.contains("exhausted"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn directing_at_an_unknown_harness_is_refused() {
        let err = directed(&claude(), &codex(), "not_a_harness").expect_err("must refuse");
        assert!(err.to_string().contains("unknown harness"), "{err}");
    }

    #[test]
    fn directing_at_a_catalog_leg_is_a_handshake() {
        match directed(&claude(), &codex(), "cursor").expect("ok") {
            Decision::Hop { to, hop, reason } => {
                assert_eq!(to.as_str(), Harness::CURSOR);
                assert_eq!(hop, Hop::HandedOff);
                assert_eq!(reason, "directed");
            }
            other => panic!("{other:?}"),
        }
    }

    fn rec(bus_id: BusId, session: &str, event: Event) -> Record {
        Record {
            schema_version: conbus_core::record::SCHEMA_VERSION,
            ts: conbus_core::clock::Timestamp::now(),
            seq: 0,
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: conbus_core::event::Source {
                harness: Harness::new(Harness::CODEX),
                harness_version: None,
                vendor_session_id: Some(session.into()),
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
            },
            event,
        }
    }

    #[test]
    fn a_successor_session_end_does_not_drop_the_warm_thread() {
        let bus_id = BusId::mint();
        let records = [
            rec(
                bus_id,
                "claude-1",
                Event::Leased {
                    holder: warm::holder("thr_bus"),
                },
            ),
            rec(
                bus_id,
                "thr_bus",
                Event::SessionEnded {
                    reason: Some("other".into()),
                },
            ),
        ];

        assert!(matches!(lease_of(&records, bus_id), Lease::Warm(id) if id == "thr_bus"));
        assert_eq!(live_warm_threads(&records), vec!["thr_bus".to_string()]);
    }

    #[test]
    fn a_session_end_releases_a_plain_holder_but_not_a_warm_thread() {
        let bus_id = BusId::mint();
        let held = [
            rec(
                bus_id,
                "claude-1",
                Event::Leased {
                    holder: "conbus-hop".into(),
                },
            ),
            rec(bus_id, "claude-1", Event::SessionEnded { reason: None }),
        ];
        assert!(matches!(lease_of(&held, bus_id), Lease::Free));

        let warm = [
            rec(
                bus_id,
                "claude-1",
                Event::Leased {
                    holder: warm::holder("thr_bus"),
                },
            ),
            rec(bus_id, "claude-1", Event::SessionEnded { reason: None }),
        ];
        assert!(matches!(lease_of(&warm, bus_id), Lease::Warm(id) if id == "thr_bus"));
        assert_eq!(live_warm_threads(&warm), vec!["thr_bus".to_string()]);
    }

    #[test]
    fn a_claude_warm_does_not_drop_the_codex_lease() {
        let bus_id = BusId::mint();
        let records = [
            rec(
                bus_id,
                "claude-1",
                Event::Leased {
                    holder: warm::holder("thr_bus"),
                },
            ),
            rec(
                bus_id,
                "claude-1",
                Event::Leased {
                    holder: warm::claude_holder("sess_bus"),
                },
            ),
        ];
        assert!(matches!(lease_of(&records, bus_id), Lease::Warm(id) if id == "thr_bus"));
        assert_eq!(
            claude_warm_of(&records, bus_id).as_deref(),
            Some("sess_bus")
        );
        assert_eq!(live_warm_threads(&records), vec!["thr_bus".to_string()]);
        assert_eq!(live_claude_sessions(&records), vec!["sess_bus".to_string()]);
    }

    #[test]
    fn a_claude_warm_alone_is_not_a_held_lease() {
        let bus_id = BusId::mint();
        let records = [rec(
            bus_id,
            "claude-1",
            Event::Leased {
                holder: warm::claude_holder("sess_bus"),
            },
        )];
        assert!(matches!(lease_of(&records, bus_id), Lease::Free));
        assert_eq!(
            claude_warm_of(&records, bus_id).as_deref(),
            Some("sess_bus")
        );
    }

    #[test]
    fn the_recorded_demo_is_one_failover_not_a_routing_hop() {
        let meter = conbus_core::meter::Meter::fold(&demo_records());
        assert_eq!(meter.failover_hops, 1);
        assert_eq!(meter.routing_hops, 0);
        assert_eq!(meter.failover_edges, 1);
    }
}
