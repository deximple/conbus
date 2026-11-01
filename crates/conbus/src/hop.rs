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
use conbus_core::event::{Event, Harness, Hop, HopProvenance, TurnState};
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
    let scan = SegmentLog::scan_meta(paths.log_dir(), Some(&paths.cache_dir()))?;
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
                &Chosen {
                    to: next,
                    hop: *hop,
                    reason,
                    provenance: provenance_of(to.is_some()),
                },
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
        profile: None,
    };
    let rec = |seq, ts: &str, harness: &str, event| Record {
        schema_version: conbus_core::record::SCHEMA_VERSION,
        ts: ts.parse().expect("ts"),
        seq,
        bus_id,
        vendor_turn_id: None,
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
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
                provenance: Some(HopProvenance::VendorSignal),
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
    // The current leg reads the quota of the account it is actually running
    // under; a target is always read as the default profile, because a hop
    // never carries a profile — no path moves a conversation between them.
    let current = leg(
        paths,
        &focus.harness,
        focus.last_turn.clone(),
        now,
        focus.profile.as_deref(),
    );
    let other_harness = route::overflow_target(&focus.harness)
        .or_else(|| route::opposite(&focus.harness))
        .unwrap_or_else(|| Harness::new(Harness::CODEX));
    let other_turn = latest_turn(records, other_harness.as_str());
    let other = leg(paths, &other_harness, other_turn, now, None);
    let mut decision = route::decide_driven(&current, &other, focus.machine_driven);
    // An automatic hop back to claude_code lands on the default account —
    // every landing (`warm-claude:` resume, a fresh window) is the default
    // profile's. For a conversation whose Claude legs ran under a profile
    // that is moving it between two of the person's accounts with no hand on
    // it, so the decision waits and says who the conversation is under.
    // `--to`/`go` skips this — the person chose.
    if let Decision::Hop { to, .. } = &decision
        && to.as_str() == Harness::CLAUDE_CODE
        && let Some(name) = claude_profile_of(records, focus.bus_id)
    {
        decision = Decision::Wait {
            reason: format!(
                "this conversation runs under Claude profile {name}; switching accounts is \
                 manual — conbus go claude_code, or conbus run claude_code --profile {name}"
            ),
        };
    }
    Ok((current, other, decision))
}

/// The profile the conversation's most recent `claude_code` record ran
/// under — the account an automatic hop back would silently bypass.
fn claude_profile_of(records: &[Record], bus_id: BusId) -> Option<&str> {
    records
        .iter()
        .rev()
        .find(|record| {
            record.bus_id == bus_id && record.source.harness.as_str() == Harness::CLAUDE_CODE
        })
        .and_then(|record| record.source.profile.as_deref())
}

struct Focus {
    harness: Harness,
    /// The Claude Code profile the focus record came under — which account's
    /// quota reading "current" means. Never carried into a hop or takeover.
    profile: Option<String>,
    last_turn: Option<TurnState>,
    /// Whether a program drove this conversation rather than a person. The
    /// briefing already refuses these; routing did not, so a scheduled task
    /// crossing 80% was moved to another harness mid-run.
    machine_driven: bool,
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
    let machine_driven = conbus_core::continuation::unattended(record.source.entrypoint.as_ref())
        || matches!(
            &record.event,
            Event::TurnStarted { prompt, .. }
                if conbus_core::continuation::machine_authored(prompt)
        );
    Some(Focus {
        harness: record.source.harness.clone(),
        profile: record.source.profile.clone(),
        machine_driven,
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

fn observed_quota(paths: &Paths, harness: &str, profile: Option<&str>) -> Option<Quota> {
    match harness {
        Harness::CODEX => quota::read().ok(),
        // `claude_code@work` reads `quota/claude_code@work.json`; the default
        // profile keeps the file every pre-profile build wrote.
        Harness::CLAUDE_CODE => quota::read_cached(
            paths,
            &profile.map_or_else(|| harness.to_string(), |p| format!("{harness}@{p}")),
        ),
        Harness::AGY => quota::read_agy().ok(),
        _ => None,
    }
}

fn leg(
    paths: &Paths,
    harness: &Harness,
    last_turn: Option<TurnState>,
    now: i64,
    profile: Option<&str>,
) -> Leg {
    let mut used_percent = None;
    let mut minutes_to_reset = None;
    let mut reached = false;
    if let Some(quota) = observed_quota(paths, harness.as_str(), profile) {
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

/// One decision, as the log will record it: where, why, and who decided.
struct Chosen<'a> {
    to: &'a Harness,
    hop: Hop,
    reason: &'a str,
    provenance: HopProvenance,
}

fn apply_hop(
    paths: &Paths,
    records: &[Record],
    from: Option<&str>,
    current: &Leg,
    chosen: &Chosen<'_>,
) -> io::Result<()> {
    let Chosen {
        to,
        hop,
        reason,
        provenance,
    } = *chosen;
    let started = std::time::Instant::now();
    let mut steps = Vec::new();
    let focus = predecessor(records, from, current)?;
    let code_freshness = Some(crate::code::probe(focus.workspace.as_deref()).line());
    if matches!(lease_of(records, focus.bus_id), Lease::Held(_)) {
        return Err(io::Error::other(
            "this conversation is already leased for a hop; wait for it to end",
        ));
    }

    let text = digest(records, focus.bus_id);
    steps.push(("decide", started.elapsed().as_millis()));
    let kind = match hop {
        Hop::Displaced => "displaced",
        Hop::HandedOff => "handed_off",
    };

    // The decision is durable before anything is opened at the vendor.
    // The other order — open, then record — leaves a warm thread nothing
    // points at whenever the record fails, and the next hop, finding no
    // lease, opens a second one and orphans the first with no account of
    // why. Recording first can only cost a `routed` line for a hop whose
    // destination then failed to open, which the log can say and a person
    // can read.
    let lease_at = std::time::Instant::now();
    if !routed_to(records, focus.bus_id, to) {
        record_hop(paths, &focus, to, hop, reason, code_freshness, provenance)?;
        println!("recorded  routed {kind}");
    }
    steps.push(("lease", lease_at.elapsed().as_millis()));

    // Destination first among the warms. Overflow to Codex was waiting on
    // `claude -p` (MCP spawn) before `codex://` could open. The other leg
    // still warms after the vendor window is up; a failure there is not a
    // failed hop. Each warm writes its own lease, checked, where it opens —
    // this used to write a second, unchecked copy from here as well.
    let warm_at = std::time::Instant::now();
    let thread = match to.as_str() {
        Harness::CODEX => {
            let thread = take_codex_warm(paths, records, &focus, &text)?;
            if let Err(err) = ensure_claude_warm(paths, records, &focus, &text) {
                println!("warm      claude skipped ({err})");
            }
            Some(thread)
        }
        Harness::CLAUDE_CODE => {
            let thread = ensure_claude_warm(paths, records, &focus, &text)?;
            if let Err(err) = take_codex_warm(paths, records, &focus, &text) {
                println!("warm      codex skipped ({err})");
            }
            Some(thread)
        }
        _ => {
            if let Err(err) = take_codex_warm(paths, records, &focus, &text) {
                println!("warm      codex skipped ({err})");
            }
            if let Err(err) = ensure_claude_warm(paths, records, &focus, &text) {
                println!("warm      claude skipped ({err})");
            }
            None
        }
    };
    steps.push(("warm", warm_at.elapsed().as_millis()));

    let open_at = std::time::Instant::now();
    let opened = handoff(paths, &focus, to.as_str(), thread.as_deref());
    steps.push(("open", open_at.elapsed().as_millis()));
    // The daemon's own line quotes this one — the 21-minute hop left no
    // line at all once, so the log now says where the time went.
    let total = started.elapsed().as_millis();
    println!(
        "steps     {}",
        steps
            .iter()
            .map(|(name, ms)| format!("{name} {ms} ms"))
            .chain([format!("total {total} ms")])
            .collect::<Vec<_>>()
            .join(" · ")
    );
    opened
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
                    profile: record.source.profile.clone(),
                    // This fallback only names where to *record* a hop the
                    // caller already decided on, so authorship is not
                    // consulted again here.
                    machine_driven: false,
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
            Event::SessionEnded { .. }
                if by_bus
                    .get(&record.bus_id)
                    .and_then(|id| id.as_deref())
                    .is_none() =>
            {
                by_bus.insert(record.bus_id, None);
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
            hook_id: None,
            spooled_at: None,
            workspace: focus.workspace.clone(),
            entrypoint: None,
            brief: false,
            provenance: None,
            profile: focus.profile.clone(),
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

/// Write the routing decision, and nothing else.
///
/// It used to write the companion `Leased` too, with `let _ = submit(...)` —
/// a discarded Result on the one write that says which warm thread this hop
/// belongs to. When it failed the log kept a `routed` with no lease, and the
/// next `hop --apply` opened a second thread at the vendor. Both warms write
/// their own lease where they open it, checked; this writes the decision.
/// What a run declares about itself, through `CONBUS_PROVENANCE`.
///
/// Any non-empty value counts as a declaration: an unrecognised marker is
/// still a marker, and the conservative direction is out of the organic
/// count. A probe that provokes a real vendor 429 is still a probe, so this
/// wins over the signal that caused the hop.
pub fn declared_synthetic() -> bool {
    std::env::var_os("CONBUS_PROVENANCE").is_some_and(|value| !value.is_empty())
}

/// Who decided this hop. `--to` on the command line, or a chat order, is the
/// operator's decision and not the product's.
fn provenance_of(directed: bool) -> HopProvenance {
    if declared_synthetic() {
        HopProvenance::DeclaredSynthetic
    } else if directed {
        HopProvenance::Directed
    } else {
        HopProvenance::VendorSignal
    }
}

fn record_hop(
    paths: &Paths,
    focus: &Focus,
    to: &Harness,
    hop: Hop,
    reason: &str,
    code_freshness: Option<String>,
    provenance: HopProvenance,
) -> io::Result<()> {
    let event = Event::Routed {
        next: to.as_str().to_string(),
        hop,
        reason: reason.to_string(),
        code_freshness,
        provenance: Some(provenance),
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
        hook_id: None,
        spooled_at: None,
        workspace: focus.workspace.clone(),
        entrypoint: None,
        brief: false,
        provenance: None,
        profile: focus.profile.clone(),
        event,
    };
    match submit(paths, &request) {
        Ok(Response::Recorded { .. } | Response::Briefed { .. }) => Ok(()),
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
            Event::SessionEnded { .. } if !matches!(lease, Lease::Warm(_)) => {
                lease = Lease::Free;
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
            hook_id: None,
            spooled_at: None,
            source: conbus_core::event::Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some("s".into()),
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
                profile: None,
            },
            event: Event::Routed {
                next: Harness::CODEX.into(),
                hop: Hop::Displaced,
                reason: "rate_limit".into(),
                code_freshness: None,
                provenance: None,
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
            hook_id: None,
            spooled_at: None,
            source: conbus_core::event::Source {
                harness: Harness::new(Harness::CODEX),
                harness_version: None,
                vendor_session_id: Some(session.into()),
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
                profile: None,
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

    /// A stand-in daemon that answers `Recorded` and remembers what it was
    /// asked, for a fixed window. The window, rather than a request count,
    /// is the point: a test that accepted exactly one connection could not
    /// see a second write, which is the defect being pinned.
    fn fake_daemon(socket: std::path::PathBuf) -> std::thread::JoinHandle<Vec<String>> {
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            let until = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < until {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).expect("blocking");
                let mut lines = BufReader::new(&stream).lines();
                let Some(Ok(line)) = lines.next() else {
                    continue;
                };
                let value: serde_json::Value = serde_json::from_str(&line).expect("request");
                let event = value
                    .get("event")
                    .and_then(|e| e.as_str().map(str::to_string))
                    .or_else(|| {
                        value
                            .get("event")?
                            .get("event")?
                            .as_str()
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| line.clone());
                seen.push(event);
                let reply = serde_json::json!({
                    "status": "recorded",
                    "bus_id": "01a07243-a666-7d50-b64b-773b257d06e1",
                    "seqs": [1],
                });
                let mut stream = &stream;
                let _ = writeln!(stream, "{reply}");
                let _ = stream.flush();
            }
            seen
        })
    }

    #[test]
    fn recording_a_hop_writes_the_decision_and_no_lease() {
        // The companion `Leased` used to go out from here with its Result
        // discarded, while both warms already write their own — checked —
        // where they open the thread. A cold hop put `leased, routed, leased`
        // on the log, and when the unchecked one failed the log kept a routed
        // with no lease and the next hop opened a second vendor thread.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let daemon = fake_daemon(paths.socket());
        let focus = Focus {
            harness: Harness::new(Harness::CLAUDE_CODE),
            profile: None,
            machine_driven: false,
            last_turn: None,
            vendor_session_id: Some("vendor-abc".into()),
            workspace: Some("/repo".into()),
            bus_id: "01a07243-a666-7d50-b64b-773b257d06e1".parse().expect("id"),
        };

        record_hop(
            &paths,
            &focus,
            &Harness::new(Harness::CODEX),
            Hop::Displaced,
            "rate_limit",
            None,
            HopProvenance::VendorSignal,
        )
        .expect("record");

        assert_eq!(
            daemon.join().expect("daemon"),
            vec!["routed".to_string()],
            "the decision, and only the decision"
        );
    }

    #[test]
    fn a_hop_a_person_typed_is_not_the_router_deciding() {
        // `--to` on the command line, or a chat order, is the operator's
        // decision. Counting it as the product's is how eleven staged hops
        // read as "routing 4 (3.35/week)" against a kill criterion of three.
        assert_eq!(provenance_of(true), HopProvenance::Directed);
        assert_eq!(provenance_of(false), HopProvenance::VendorSignal);
    }

    /// A turn ended on `harness` in `state`, under `profile` when that leg
    /// names one.
    fn ended(bus_id: BusId, harness: &str, profile: Option<&str>, state: TurnState) -> Record {
        Record {
            schema_version: conbus_core::record::SCHEMA_VERSION,
            ts: conbus_core::clock::Timestamp::now(),
            seq: 1,
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            source: conbus_core::event::Source {
                harness: Harness::new(harness),
                harness_version: None,
                vendor_session_id: Some("sess".into()),
                workspace: Some("/repo".into()),
                entrypoint: None,
                backfilled_from: None,
                profile: profile.map(str::to_string),
            },
            event: Event::TurnEnded {
                state,
                assistant_message: None,
                tokens: None,
            },
        }
    }

    #[test]
    fn an_automatic_hop_back_to_claude_never_lands_on_another_account() {
        // The laundering path: the conversation ran under profile `work`,
        // hopped to codex when `work` was spent, and now codex is spent too.
        // Routing it back to `claude_code` would land on the DEFAULT account
        // — a switch between two of the person's accounts with no hand on
        // it. The decision waits and says whose profile it is.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let bus = BusId::mint();
        let records = [
            ended(bus, Harness::CLAUDE_CODE, Some("work"), TurnState::Complete),
            ended(
                bus,
                Harness::CODEX,
                None,
                TurnState::InterruptedQuota {
                    reason: "rate_limit".into(),
                },
            ),
        ];

        let (_, _, decision) = plan(&paths, &records, None).expect("plan");

        let Decision::Wait { reason } = decision else {
            panic!("a profiled conversation must not be hopped to another account: {decision:?}");
        };
        assert!(
            reason.contains("work"),
            "the reason names the profile: {reason}"
        );
        assert!(reason.contains("manual"), "{reason}");
    }

    #[test]
    fn a_directed_hop_back_to_claude_is_the_persons_choice() {
        // `conbus go claude_code` / `--to` is the person moving the
        // conversation by hand — allowed, profile or no.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let bus = BusId::mint();
        let records = [
            ended(bus, Harness::CLAUDE_CODE, Some("work"), TurnState::Complete),
            ended(
                bus,
                Harness::CODEX,
                None,
                TurnState::InterruptedQuota {
                    reason: "rate_limit".into(),
                },
            ),
        ];
        let (current, other, _) = plan(&paths, &records, None).expect("plan");

        let decision = directed(&current, &other, "claude_code").expect("directed");

        assert!(
            matches!(decision, Decision::Hop { ref to, .. } if to.as_str() == Harness::CLAUDE_CODE),
            "a directed hop may name claude_code: {decision:?}"
        );
    }

    #[test]
    fn a_default_profile_history_still_overflows_back_to_claude() {
        // No profile anywhere: the automatic hop home is exactly what it
        // always was.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let bus = BusId::mint();
        let records = [
            ended(bus, Harness::CLAUDE_CODE, None, TurnState::Complete),
            ended(
                bus,
                Harness::CODEX,
                None,
                TurnState::InterruptedQuota {
                    reason: "rate_limit".into(),
                },
            ),
        ];

        let (_, _, decision) = plan(&paths, &records, None).expect("plan");

        assert!(
            matches!(decision, Decision::Hop { ref to, .. } if to.as_str() == Harness::CLAUDE_CODE),
            "unprofiled conversations route home as before: {decision:?}"
        );
    }

    /// A record under a profiled `claude` — `source.profile` is the only
    /// thing that differs.
    fn profiled(bus_id: BusId, harness: &str, profile: Option<&str>) -> Record {
        Record {
            schema_version: conbus_core::record::SCHEMA_VERSION,
            ts: conbus_core::clock::Timestamp::now(),
            seq: 1,
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            source: conbus_core::event::Source {
                harness: Harness::new(harness),
                harness_version: None,
                vendor_session_id: Some("sess".into()),
                workspace: Some("/repo".into()),
                entrypoint: None,
                backfilled_from: None,
                profile: profile.map(str::to_string),
            },
            event: Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: None,
                tokens: None,
            },
        }
    }

    /// What a StatusLine write leaves behind, named for the account it is
    /// about: `claude_code` or `claude_code@NAME`.
    fn cache_quota(paths: &Paths, name: &str, reached: bool) {
        let path = paths.quota_cache(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("quota dir");
        let now = now_secs();
        std::fs::write(
            path,
            serde_json::json!({
                "schema_version": 1,
                "harness": name,
                "observed_at": now,
                "primary": {"used_percent": 10, "minutes": 300, "resets_at": now + 18000},
                "secondary": null,
                "reached": reached.then_some("rate_limit"),
            })
            .to_string(),
        )
        .expect("cache");
    }

    #[test]
    fn the_current_leg_reads_the_profile_it_runs_under() {
        // The default account is spent and `work` is fine: a conversation in
        // `work` is not displaced by a limit it is not under.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let records = [profiled(BusId::mint(), Harness::CLAUDE_CODE, Some("work"))];
        cache_quota(&paths, Harness::CLAUDE_CODE, true);
        cache_quota(&paths, "claude_code@work", false);

        let (current, _, _) = plan(&paths, &records, None).expect("plan");
        assert!(!current.reached, "the profile's reading is fine");
    }

    #[test]
    fn a_spent_profile_routes_like_any_spent_leg() {
        // And the reverse: `work` is spent while the default is fine. The
        // current leg is reached on its own account — the router still never
        // names a profile, so the hop lands on the default leg, never on
        // another of the person's accounts.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let records = [profiled(BusId::mint(), Harness::CLAUDE_CODE, Some("work"))];
        cache_quota(&paths, Harness::CLAUDE_CODE, false);
        cache_quota(&paths, "claude_code@work", true);

        let (current, _, _) = plan(&paths, &records, None).expect("plan");
        assert!(current.reached, "the profile's own reading is spent");
    }

    #[test]
    fn a_claude_target_is_read_as_the_default_profile() {
        // A hop never carries a profile, so the claude leg a codex
        // conversation might land on is the default account's.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let records = [profiled(BusId::mint(), Harness::CODEX, None)];
        cache_quota(&paths, Harness::CLAUDE_CODE, true);
        cache_quota(&paths, "claude_code@work", false);

        let (_, other, _) = plan(&paths, &records, None).expect("plan");
        assert_eq!(other.harness.as_str(), Harness::CLAUDE_CODE);
        assert!(other.reached, "the target read the default, not a profile");
    }

    #[test]
    fn a_takeover_never_carries_a_profile() {
        // The runner applies --profile from the run's own argv, so a hop
        // file that could name one would be a way to move a conversation
        // between accounts with nobody's hand on it. There is no field.
        let takeover = crate::takeover::Takeover {
            next: "claude_code".into(),
            thread: Some("warm-claude:abc".into()),
            bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
            cwd: None,
        };
        let json = serde_json::to_value(&takeover).expect("serialise");
        assert!(
            json.get("profile").is_none(),
            "a takeover names a leg, never an account: {json}"
        );
    }
}
