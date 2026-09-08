//! `conbus summary --bus-id` — model digest on the opposite leg.
//!
//! The hook path stays extractive. This command is the only model call,
//! and it is forbidden from using the harness it is protecting.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use conbus_core::event::{Body, Event, Harness};
use conbus_core::ids::BusId;
use conbus_core::pack::Chapter;
use conbus_core::paths::Paths;
use conbus_core::record::Record;
use conbus_core::segment::SegmentLog;
use conbus_core::summary::{self, Backend, EXTRACTIVE};

use crate::appserver;
use crate::hook::client::INTERNAL_MARKER;
use crate::hop;
use crate::wire::{self, Request, Response};

const BOUND: Duration = Duration::from_secs(20);

pub fn run(paths: &Paths, bus_id: Option<String>) -> io::Result<()> {
    let scan = SegmentLog::scan(paths.log_dir())?;
    let wanted = bus_id.ok_or_else(|| io::Error::other("pass --bus-id <id>"))?;
    let chapter_records: Vec<&Record> = scan
        .records
        .iter()
        .filter(|record| record.bus_id.to_string().starts_with(&wanted))
        .collect();
    let Some(record) = chapter_records.last() else {
        return Err(io::Error::other(format!(
            "no conversation starts with {wanted}"
        )));
    };
    let bus_id = record.bus_id;
    let chapter = Chapter::read(&chapter_records)
        .ok_or_else(|| io::Error::other("that conversation has no chapter yet"))?;
    let extract = summary::extract(&chapter);
    let protecting = &chapter.about.harness;
    let opposite_usable = opposite_binary(protecting).is_some();
    let (backend, digest) = match summary::backend(protecting, opposite_usable) {
        Backend::Extractive => (EXTRACTIVE.to_string(), extract),
        Backend::Opposite { harness } => match refresh(&harness, &extract) {
            Ok(text) if !text.trim().is_empty() => (harness.as_str().to_string(), text),
            Ok(_) | Err(_) => {
                eprintln!("conbus summary: opposite leg failed; wrote extractive");
                (EXTRACTIVE.to_string(), extract)
            }
        },
    };
    persist(
        paths,
        bus_id,
        record,
        protecting,
        &backend,
        chapter.about.turns,
        &digest,
    )?;
    println!("backend   {backend}");
    println!("covered   {} turn(s)", chapter.about.turns);
    println!("digest    {digest}");
    Ok(())
}

fn opposite_binary(protecting: &Harness) -> Option<String> {
    match summary::backend(protecting, true) {
        Backend::Opposite { harness } if harness.as_str() == Harness::CODEX => {
            Some(appserver::codex_binary().to_string_lossy().into_owned())
        }
        Backend::Opposite { harness } if harness.as_str() == Harness::CLAUDE_CODE => {
            Some(claude_binary())
        }
        _ => None,
    }
}

fn claude_binary() -> String {
    std::env::var("CONBUS_CLAUDE_BIN")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "claude".into())
}

fn refresh(harness: &Harness, extract: &str) -> io::Result<String> {
    let prompt = format!(
        "Summarise this coding conversation for the next agent. Keep the goal, the decisions, and unanswered work. Do not address the user.\n\n{extract}"
    );
    let mut child = match harness.as_str() {
        Harness::CODEX => Command::new(appserver::codex_binary())
            .args(["exec", &prompt])
            .env(INTERNAL_MARKER, "1")
            .env("PATH", appserver::path_for_vendor())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
        Harness::CLAUDE_CODE => Command::new(claude_binary())
            .args(["-p", &prompt, "--output-format", "text"])
            .env(INTERNAL_MARKER, "1")
            .env("PATH", appserver::path_for_vendor())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
        _ => return Err(io::Error::other("no opposite binary")),
    };
    let deadline = Instant::now() + BOUND;
    loop {
        match child.try_wait()? {
            Some(status) if status.success() => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                return Ok(out);
            }
            Some(_) => return Err(io::Error::other("opposite summariser exited failed")),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other("opposite summariser exceeded 20s"));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn persist(
    paths: &Paths,
    bus_id: BusId,
    last: &Record,
    protecting: &Harness,
    backend: &str,
    covered_turns: u64,
    digest: &str,
) -> io::Result<()> {
    let session = last
        .source
        .vendor_session_id
        .clone()
        .unwrap_or_else(|| bus_id.to_string());
    let request = Request {
        protocol_version: Some(wire::PROTOCOL_VERSION),
        harness: protecting.clone(),
        harness_version: None,
        vendor_session_id: session,
        vendor_turn_id: None,
        vendor_agent_id: None,
        workspace: last.source.workspace.clone(),
        entrypoint: None,
        brief: false,
        provenance: None,
        event: Event::Summarised {
            backend: backend.into(),
            covered_turns,
            digest: Body::kept(digest),
        },
    };
    match hop::submit(paths, &request) {
        Ok(Response::Recorded { .. } | Response::Briefed { .. } | Response::Deferred) => Ok(()),
        Ok(other) => Err(io::Error::other(format!(
            "daemon refused the digest: {other:?}"
        ))),
        Err(err) => Err(io::Error::other(format!(
            "daemon not listening ({err}); summaries go through the daemon"
        ))),
    }
}

/// Latest stored digest, if any. Hop uses this so a model refresh lands
/// in the next warm without a hop-path model call.
pub fn latest_digest(records: &[&Record]) -> Option<String> {
    records.iter().rev().find_map(|record| match &record.event {
        Event::Summarised {
            digest: Body::Kept { text },
            ..
        }
        | Event::Summarised {
            digest: Body::Clipped { text, .. },
            ..
        } => Some(text.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protected_leg_is_never_chosen_when_the_opposite_exists() {
        match summary::backend(&Harness::new(Harness::CLAUDE_CODE), true) {
            Backend::Opposite { harness } => assert_eq!(harness.as_str(), Harness::CODEX),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hop_prefers_a_stored_digest_over_a_live_extract() {
        let bus_id: BusId = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id");
        let record = Record {
            schema_version: conbus_core::record::SCHEMA_VERSION,
            ts: "2026-09-07T00:00:00.000Z".parse().expect("ts"),
            seq: 1,
            bus_id,
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: conbus_core::event::Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: None,
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
            },
            event: Event::Summarised {
                backend: "codex".into(),
                covered_turns: 8,
                digest: Body::kept("ship the hop, do not re-ask"),
            },
        };
        assert_eq!(
            latest_digest(&[&record]).as_deref(),
            Some("ship the hop, do not re-ask")
        );
    }
}
