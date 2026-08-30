//! `conbus log` and `conbus doctor`.

use std::io;
use std::os::unix::net::UnixStream;

use conbus_core::event::Event;
use conbus_core::paths::Paths;
use conbus_core::record::Record;
use conbus_core::segment::SegmentLog;

pub fn log(paths: &Paths, json: bool) -> io::Result<()> {
    let scan = SegmentLog::scan(paths.log_dir())?;
    for record in &scan.records {
        if json {
            println!(
                "{}",
                serde_json::to_string(record).map_err(io::Error::other)?
            );
        } else {
            println!("{}", summarise(record));
        }
    }
    for defect in &scan.defects {
        eprintln!(
            "unreadable: {} line {}: {}",
            defect.segment.display(),
            defect.line,
            defect.reason
        );
    }
    Ok(())
}

fn summarise(record: &Record) -> String {
    let what = match &record.event {
        Event::SessionCandidate { trigger, .. } => {
            format!("session-start {}", trigger.as_deref().unwrap_or("?"))
        }
        Event::TurnStarted { .. } => "turn-started".to_string(),
        Event::ToolUse { tool, .. } => format!("tool {tool}"),
        Event::TurnEnded { state, .. } => format!("turn-ended {state:?}"),
        Event::SessionEnded { reason } => {
            format!("session-end {}", reason.as_deref().unwrap_or("?"))
        }
    };
    format!(
        "{} {:>6} {} {} {what}",
        record.ts,
        record.seq,
        record.bus_id,
        record.source.harness.as_str()
    )
}

pub fn doctor(paths: &Paths) -> io::Result<()> {
    println!(
        "root      {} ({:?})",
        paths.root().display(),
        paths.origin()
    );
    println!("log       {}", paths.log_dir().display());

    let socket = paths.socket();
    let daemon = match UnixStream::connect(&socket) {
        Ok(_) => "listening".to_string(),
        Err(err) => format!("not answering ({err})"),
    };
    println!("socket    {} — {daemon}", socket.display());

    let report = crate::harden::harden();
    println!(
        "hardening core_dumps_disabled={} memory_locked={}{}",
        report.core_dumps_disabled,
        report.memory_locked,
        report
            .memory_lock_error
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default()
    );

    let scan = SegmentLog::scan(paths.log_dir())?;
    let conversations: std::collections::HashSet<_> =
        scan.records.iter().map(|r| r.bus_id).collect();
    println!(
        "log       {} record(s), {} conversation(s), {} unreadable line(s)",
        scan.records.len(),
        conversations.len(),
        scan.defects.len()
    );
    Ok(())
}
