//! `conbus log` and `conbus doctor`.

use std::io;
use std::os::unix::net::UnixStream;

use conbus_core::event::{Event, ToolOutcome};
use conbus_core::paths::Paths;

use crate::cli::LogFilter;
use conbus_core::record::Record;
use conbus_core::segment::SegmentLog;
use std::fs::OpenOptions;

pub fn log(paths: &Paths, json: bool, filter: &LogFilter) -> io::Result<()> {
    let scan = SegmentLog::scan(paths.log_dir())?;
    for record in select(&scan.records, filter) {
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

/// The records a filter admits, in log order.
///
/// `bus_id` and `since` match by prefix so that a partial id or a bare date
/// works — the id is a 36-character UUID and the timestamp is fixed-width, so a
/// prefix is unambiguous in both.
fn select<'a>(records: &'a [Record], filter: &LogFilter) -> Vec<&'a Record> {
    let matching: Vec<&Record> = records
        .iter()
        .filter(|record| {
            filter
                .bus_id
                .as_ref()
                .is_none_or(|wanted| record.bus_id.to_string().starts_with(wanted))
                && filter
                    .since
                    .as_ref()
                    .is_none_or(|from| record.ts.to_string().as_str() >= from.as_str())
                && filter
                    .grep
                    .as_ref()
                    .is_none_or(|needle| summarise(record).contains(needle))
        })
        .collect();
    match filter.tail {
        Some(n) if n < matching.len() => matching[matching.len() - n..].to_vec(),
        _ => matching,
    }
}

/// Can the log actually be appended to?
///
/// Probes the operation the writer performs — opening the current segment for
/// append — rather than creating a file in the directory. POSIX checks
/// directory permission for the one and file permission for the other, so a
/// directory probe is wrong in both directions: it passes while a read-only
/// segment refuses every record, and it fails while a daemon holding an open
/// handle keeps recording fine.
///
/// Deliberately does not create the log directory. A diagnostic that repairs
/// what it is measuring cannot report on it.
fn appendable(paths: &Paths) -> io::Result<()> {
    let dir = paths.log_dir();
    if !dir.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist yet", dir.display()),
        ));
    }
    let segment = SegmentLog::current_segment(&dir)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&segment)
        .map(|_| ())
}

fn describe_appendable(result: &io::Result<()>) -> String {
    match result {
        Ok(()) => "appendable".to_string(),
        Err(err) => format!("NOT APPENDABLE: {err} — records will be refused"),
    }
}

fn summarise(record: &Record) -> String {
    let what = match &record.event {
        Event::SessionCandidate { trigger, .. } => {
            format!("session-start {}", trigger.as_deref().unwrap_or("?"))
        }
        // The author is rendered because a machine-authored turn reading
        // exactly like a person's is how a survival meter starts lying.
        Event::TurnStarted { author, .. } => match author {
            Some(author) => format!("turn-started <{}>", author.as_str()),
            None => "turn-started".to_string(),
        },
        Event::ToolBatchEnded {
            tool_calls,
            unreadable,
        } => match unreadable {
            0 => format!("tool-batch of {} resolved", tool_calls.len()),
            n => format!(
                "tool-batch of {} resolved ({n} unreadable)",
                tool_calls.len()
            ),
        },
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
        Event::ToolBatchEnded { .. }
        | Event::SessionCandidate { .. }
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

/// Register the hook with every harness that will call it.
///
/// Reports by default. `--apply` writes, and keeps a copy of whatever was there
/// first: these are the user's own settings files and they usually already hold
/// their own hooks.
pub fn install(apply: bool) -> io::Result<()> {
    let binary = std::env::current_exe()?;
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other("no HOME to install into"))?;

    for target in crate::install::targets(&binary, &home) {
        let harness = target.harness;
        let settings = target.settings.clone();
        let plan = crate::install::plan(target)?;

        if let Some(err) = &plan.unreadable {
            println!("{harness:<12} {} — NOT TOUCHED: {err}", settings.display());
            println!(
                "{:<12} fix or move that file; conbus will not replace what it cannot read",
                ""
            );
            continue;
        }

        let adding: Vec<&str> = plan
            .changes
            .iter()
            .filter(|c| matches!(c, crate::install::Change::Add(_)))
            .map(|c| c.event())
            .collect();
        if adding.is_empty() {
            println!("{harness:<12} {} — already registered", settings.display());
            continue;
        }
        println!(
            "{harness:<12} {} — would register {}",
            settings.display(),
            adding.join(", ")
        );
        if apply {
            let backup = crate::install::apply(&plan)?;
            match backup {
                Some(path) => println!("{:<12} written; previous copy at {}", "", path.display()),
                None => println!("{:<12} written (the file did not exist)", ""),
            }
        }
    }

    if !apply {
        println!("\nNothing was written. Re-run with --apply to register.");
    }
    println!(
        "\nCodex will not run a hook until you trust it in Codex itself. conbus does not write\n\
         that trust state: it is a consent gate, and the hash it wants is one we could trivially\n\
         forge. Until you approve there, Codex hooks are registered and silently do not fire."
    );
    Ok(())
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

    println!("writable  {}", describe_appendable(&appendable(paths)));

    println!(
        "drops     {}",
        crate::drops::describe(&crate::drops::summary(paths))
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
    use conbus_core::event::{
        Author, Body, Harness, Source, ToolOutcome, VendorAgentId, VendorToolUseId,
    };
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
                workspace: None,
                backfilled_from: None,
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

    fn at(seq: u64, ts: &str, bus: &str) -> Record {
        Record {
            seq,
            ts: ts.parse().expect("ts"),
            bus_id: bus.parse().expect("bus id"),
            ..tool(None, None)
        }
    }

    fn corpus() -> Vec<Record> {
        let a = "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e";
        let b = "01a05407-694a-7261-a9a4-d27a40bbcb20";
        vec![
            at(0, "2026-08-30T10:00:00.000Z", a),
            at(1, "2026-08-30T11:00:00.000Z", b),
            at(2, "2026-08-31T09:00:00.000Z", a),
            at(3, "2026-08-31T10:00:00.000Z", b),
        ]
    }

    #[test]
    fn a_conversation_can_be_pulled_out_by_a_prefix_of_its_id() {
        let filter = LogFilter {
            bus_id: Some("01996b1e".into()),
            ..LogFilter::default()
        };

        let records = corpus();
        let selected = select(&records, &filter);

        assert_eq!(
            selected.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    #[test]
    fn since_takes_a_bare_date_because_the_timestamp_is_fixed_width() {
        let filter = LogFilter {
            since: Some("2026-08-31".into()),
            ..LogFilter::default()
        };

        let records = corpus();
        let selected = select(&records, &filter);

        assert_eq!(
            selected.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn tail_keeps_the_last_matches_not_the_first() {
        let filter = LogFilter {
            tail: Some(2),
            ..LogFilter::default()
        };

        let records = corpus();
        let selected = select(&records, &filter);

        assert_eq!(
            selected.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn the_selectors_compose() {
        let filter = LogFilter {
            bus_id: Some("01996b1e".into()),
            since: Some("2026-08-31".into()),
            ..LogFilter::default()
        };

        let records = corpus();
        let selected = select(&records, &filter);

        assert_eq!(selected.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn a_tail_larger_than_the_log_returns_the_whole_log() {
        let filter = LogFilter {
            tail: Some(99),
            ..LogFilter::default()
        };

        let records = corpus();
        assert_eq!(select(&records, &filter).len(), 4);
    }

    #[test]
    fn grep_matches_the_rendered_line_not_the_raw_json() {
        let mut records = corpus();
        records[1].event = Event::TurnEnded {
            state: conbus_core::event::TurnState::InterruptedQuota {
                reason: "rate_limit".into(),
            },
            assistant_message: None,
        };
        let filter = LogFilter {
            grep: Some("rate_limit".into()),
            ..LogFilter::default()
        };

        let selected = select(&records, &filter);

        assert_eq!(selected.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1]);
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
    fn a_machine_authored_turn_does_not_read_like_a_persons() {
        let human = summarise(&record(Event::TurnStarted {
            author: None,
            prompt: Body::kept("p"),
        }));
        let machine = summarise(&record(Event::TurnStarted {
            author: Some(Author::new(Author::SCHEDULE_WAKEUP)),
            prompt: Body::kept("p"),
        }));

        assert!(machine.contains("schedule_wakeup"), "{machine}");
        assert_ne!(human, machine);
    }

    #[test]
    fn a_batch_that_named_nothing_reads_differently_from_one_we_could_not_read() {
        let empty = summarise(&record(Event::ToolBatchEnded {
            tool_calls: vec![],
            unreadable: 0,
        }));
        let unread = summarise(&record(Event::ToolBatchEnded {
            tool_calls: vec![],
            unreadable: 2,
        }));

        assert_ne!(empty, unread);
        assert!(unread.contains("2 unreadable"), "{unread}");
    }

    #[test]
    fn a_batch_boundary_says_how_many_calls_it_closed() {
        let line = summarise(&record(Event::ToolBatchEnded {
            tool_calls: vec![
                VendorToolUseId::new("toolu_A"),
                VendorToolUseId::new("toolu_B"),
            ],
            unreadable: 0,
        }));

        assert!(line.contains("tool-batch of 2"), "{line}");
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
