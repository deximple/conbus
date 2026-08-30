//! `conbus log` and `conbus doctor`.

use std::io;
use std::os::unix::net::UnixStream;

use conbus_core::event::{Event, ToolOutcome};
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
        Event::ToolRequested { tool, .. } => format!("tool {tool} requested"),
        Event::ToolDenied { tool, reason, .. } => format!("tool {tool} denied ({reason})"),
        Event::ToolUse {
            tool,
            duration_ms,
            outcome,
            ..
        } => {
            let ending = match outcome {
                ToolOutcome::Succeeded { .. } => "",
                ToolOutcome::Failed {
                    interrupted: false, ..
                } => " failed",
                ToolOutcome::Failed {
                    interrupted: true, ..
                } => " interrupted",
            };
            match duration_ms {
                Some(ms) => format!("tool {tool}{ending} ({ms}ms)"),
                None => format!("tool {tool}{ending}"),
            }
        }
        Event::TurnEnded { state, .. } => format!("turn-ended {state:?}"),
        Event::SessionEnded { reason } => {
            format!("session-end {}", reason.as_deref().unwrap_or("?"))
        }
    };
    // Parallel calls of the same tool render identically without their id, and
    // the request-to-result join would be invisible in the view people read.
    // Listed exhaustively rather than with a wildcard: a tool event added later
    // would otherwise silently stop showing its join key.
    let call = match &record.event {
        Event::ToolRequested { tool_use_id, .. }
        | Event::ToolDenied { tool_use_id, .. }
        | Event::ToolUse { tool_use_id, .. } => tool_use_id
            .as_ref()
            .map(|id| format!(" #{}", id.as_str()))
            .unwrap_or_default(),
        Event::SessionCandidate { .. }
        | Event::TurnStarted { .. }
        | Event::TurnEnded { .. }
        | Event::SessionEnded { .. } => String::new(),
    };
    // A subagent's events arrive under the parent's session id, so without
    // this marker the human view interleaves two agents with no sign of it.
    let agent = record
        .vendor_agent_id
        .as_ref()
        .map(|id| format!(" [subagent {}]", id.as_str()))
        .unwrap_or_default();
    format!(
        "{} {:>6} {} {} {what}{call}{agent}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::event::{Body, Harness, Source, ToolOutcome, VendorAgentId, VendorToolUseId};
    use conbus_core::record::SCHEMA_VERSION;

    fn record(event: Event) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 3,
            ts: "2026-08-31T04:12:00.000Z".parse().expect("ts"),
            bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
            vendor_turn_id: None,
            vendor_agent_id: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: None,
            },
            event,
        }
    }

    fn tool(tool_use_id: Option<&str>, duration_ms: Option<u64>) -> Record {
        record(Event::ToolUse {
            tool: "Read".into(),
            tool_use_id: tool_use_id.map(VendorToolUseId::new),
            duration_ms,
            input: Body::kept("i"),
            outcome: ToolOutcome::Succeeded {
                output: Body::kept("o"),
            },
        })
    }

    #[test]
    fn a_measured_tool_call_shows_its_duration_in_milliseconds() {
        let line = summarise(&tool(None, Some(37)));

        assert!(line.ends_with("tool Read (37ms)"), "{line}");
    }

    #[test]
    fn an_unmeasured_tool_call_shows_no_duration_rather_than_zero() {
        let line = summarise(&tool(None, None));

        assert!(line.ends_with("tool Read"), "{line}");
    }

    fn failed_tool(interrupted: bool) -> Record {
        record(Event::ToolUse {
            tool: "Bash".into(),
            tool_use_id: None,
            duration_ms: Some(12),
            input: Body::kept("i"),
            outcome: ToolOutcome::Failed {
                error: Body::kept("exit status 1"),
                interrupted,
            },
        })
    }

    #[test]
    fn parallel_calls_are_told_apart_by_their_id_in_the_human_view() {
        // Two `Read`s requested together render identically without it, and the
        // join this change exists to enable would be invisible in the view a
        // person actually reads.
        let first = summarise(&record(Event::ToolRequested {
            tool: "Read".into(),
            tool_use_id: Some(VendorToolUseId::new("toolu_A")),
            input: Body::kept("i"),
        }));
        let second = summarise(&record(Event::ToolRequested {
            tool: "Read".into(),
            tool_use_id: Some(VendorToolUseId::new("toolu_B")),
            input: Body::kept("i"),
        }));

        assert_ne!(first, second, "two parallel requests read identically");
        assert!(first.contains("toolu_A"), "{first}");
        assert!(summarise(&tool(Some("toolu_A"), Some(6))).contains("toolu_A"));
    }

    #[test]
    fn a_call_with_no_id_carries_no_empty_marker() {
        let line = summarise(&tool(None, Some(6)));

        assert!(!line.contains('#'), "{line}");
    }

    #[test]
    fn a_denied_tool_says_why_in_the_human_view() {
        let line = summarise(&record(Event::ToolDenied {
            tool: "Bash".into(),
            tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
            input: Body::kept("i"),
            reason: "User rejected the command".into(),
        }));

        assert!(line.contains("denied"), "{line}");
        assert!(
            line.contains("User rejected the command"),
            "an unresolved request with no reason is what this event exists to fix: {line}"
        );
        assert!(line.contains("toolu_01ABC"), "{line}");
    }

    #[test]
    fn a_request_does_not_read_as_a_completed_call() {
        let requested = summarise(&record(Event::ToolRequested {
            tool: "Bash".into(),
            tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
            input: Body::kept("i"),
        }));
        let finished = summarise(&tool(None, Some(12)));

        assert!(requested.contains("requested"), "{requested}");
        assert_ne!(requested, finished);
    }

    #[test]
    fn a_failed_tool_call_does_not_read_as_a_successful_one() {
        let succeeded = summarise(&tool(None, Some(12)));
        let failed = summarise(&failed_tool(false));

        assert!(failed.contains("failed"), "{failed}");
        assert_ne!(
            succeeded.split_whitespace().skip(4).collect::<Vec<_>>(),
            failed.split_whitespace().skip(4).collect::<Vec<_>>(),
            "a failure reads identically to a success"
        );
    }

    #[test]
    fn a_tool_call_a_person_stopped_reads_differently_from_one_that_broke() {
        let broke = summarise(&failed_tool(false));
        let stopped = summarise(&failed_tool(true));

        assert!(stopped.contains("interrupted"), "{stopped}");
        assert_ne!(broke, stopped);
    }

    #[test]
    fn a_subagents_record_is_marked_so_two_agents_do_not_read_as_one() {
        let mut record = tool(None, Some(37));
        record.vendor_agent_id = Some(VendorAgentId::new("AGENT-7"));

        let line = summarise(&record);

        assert!(line.contains("[subagent AGENT-7]"), "{line}");
    }

    #[test]
    fn the_summary_leads_with_the_timestamp_sequence_conversation_and_harness() {
        let line = summarise(&tool(None, None));

        assert!(line.starts_with("2026-08-31T04:12:00.000Z "), "{line}");
        assert!(
            line.contains("01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e"),
            "{line}"
        );
        assert!(line.contains("claude_code"), "{line}");
    }
}
