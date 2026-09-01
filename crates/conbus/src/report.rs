//! `conbus log` and `conbus doctor`.

use std::collections::BTreeSet;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        // The conversations are named the way a tool call names its join key,
        // so that "what was this session told about" is one glance rather than
        // a second query.
        Event::Briefed {
            about,
            units,
            dropped,
        } => {
            let named: Vec<String> = about.iter().map(|id| id.to_string()).collect();
            let short = match *dropped {
                0 => String::new(),
                n => format!(", {n} did not fit"),
            };
            format!("briefed about {} ({units} units{short})", named.join(" "))
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
        | Event::SessionEnded { .. }
        | Event::Briefed { .. } => String::new(),
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

/// The conbus hook commands a vendor settings file actually registers.
///
/// Read out of the file rather than composed from our own path, because what
/// matters is the command the vendor will run. A registration left pointing at
/// a binary that has moved, or been replaced, is exactly the failure worth
/// finding, and composing the command ourselves would hide it.
fn registered_hooks(settings: &serde_json::Value) -> BTreeSet<String> {
    fn walk(value: &serde_json::Value, found: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, child) in fields {
                    if key == "command"
                        && let Some(command) = child.as_str()
                        && is_ours(command)
                    {
                        found.insert(command.to_string());
                    }
                    walk(child, found);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, found);
                }
            }
            _ => {}
        }
    }

    let mut found = BTreeSet::new();
    walk(settings, &mut found);
    found
}

/// Whether a registered command is one of ours, by the shape `install` writes:
/// a path to a `conbus` binary followed by the `hook` subcommand.
fn is_ours(command: &str) -> bool {
    command
        .split_once(" hook")
        .map(|(program, _)| program.trim())
        .and_then(|program| Path::new(program).file_name())
        .is_some_and(|name| name == "conbus")
}

/// Can the vendor actually run this command?
///
/// The check that was missing when the recorder went silent for two hours on a
/// real machine. The binary had been overwritten in place while a running
/// daemon held it mapped, which taints the vnode: every exec of that path was
/// killed by the kernel before `main` ran. Nothing recorded it and nothing
/// could have — the drop log is written by the hook, so a hook that cannot
/// start writes no drops, and an empty drop log was read as health.
///
/// Run through a shell, because that is how the vendor runs it, and marked
/// internal so the probe returns before it can reach the log.
fn hook_runs(command: &str) -> Result<(), String> {
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .env(crate::hook::client::INTERNAL_MARKER, "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("could not run it: {err}"))?;

    if !output.status.success() {
        // A signal has no code, and the signal is the interesting case here:
        // 137 is the kernel killing a tainted binary, not the program failing.
        let how = match output.status.code() {
            Some(code) => format!("exit {code}"),
            None => "killed by a signal".to_string(),
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            how
        } else {
            format!("{how}: {stderr}")
        });
    }
    // Exiting zero is not enough. Silence on stdout is what took a real session
    // down with `cache_control cannot be set for empty text blocks`, so the
    // probe insists on the well-formed answer too.
    if serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err() {
        return Err(format!(
            "ran, but said {:?} instead of hook JSON",
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    Ok(())
}

/// Whether Codex will actually run the hooks we registered with it.
///
/// A registered Codex hook does not run until a person approves it inside
/// Codex. It is not executed-and-ignored — the process is never spawned — so
/// the executability probe above says "runs" for a command Codex will never
/// invoke. That is the same shape of lie as an empty drop log, and on this
/// machine it is the live state: six conbus hooks registered, none trusted,
/// none ever run.
///
/// Read, never written. Codex keeps the consent record as `trusted_hash` under
/// `[hooks.state."<key>"]` in `config.toml`, and its own `hooks/list` hands out
/// the very value that would forge it — so conbus detects and reports, and
/// approving stays a person's act. A missing record proves untrusted; a present
/// one proves trusted-or-modified, which this cannot tell apart without asking
/// Codex, so it does not claim to.
fn report_codex_trust(home: &Path) {
    let Some(state) = codex_trust(home) else {
        return;
    };
    if state.untrusted.is_empty() && state.trusted == 0 {
        return;
    }
    if state.untrusted.is_empty() {
        println!("codex     {} hook(s) trusted", state.trusted);
        return;
    }
    println!(
        "codex     {} of {} hook(s) NOT TRUSTED, so Codex never spawns them: {}",
        state.untrusted.len(),
        state.untrusted.len() + state.trusted,
        state.untrusted.join(", ")
    );
    // The gesture, named. Approving is a person's act inside Codex and there is
    // no command conbus could run instead: the record is a hash, and Codex's
    // own `hooks/list` hands out the value that would forge it.
    println!(
        "          start Codex; it asks about hooks that are new or changed at startup. conbus reads that record and must never write it."
    );
    if state.others_untrusted > 0 {
        println!(
            "          note: {} other untrusted hook(s) share that file and are approved in the same gesture",
            state.others_untrusted
        );
    }
}

/// How many of our Codex hooks Codex has a trust record for, and which it does
/// not, in the order Codex lists them.
fn codex_trust(home: &Path) -> Option<CodexTrust> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let hooks_path = codex_home.join("hooks.json");
    let text = std::fs::read_to_string(&hooks_path).ok()?;
    let settings = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    let config = std::fs::read_to_string(codex_home.join("config.toml")).unwrap_or_default();

    let mut untrusted = Vec::new();
    let mut trusted = 0;
    // Approving happens per file, not per hook: Codex reviews everything that
    // is new or changed at once. Somebody who says yes on our account is also
    // saying yes to whatever else is sitting untrusted in the same file, so the
    // count is part of what they are being asked.
    let mut others_untrusted = 0;
    for (event, groups) in settings
        .get("hooks")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
    {
        for (group, entry) in groups.as_array().into_iter().flatten().enumerate() {
            for (index, hook) in entry
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let ours = hook
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(is_ours);
                let key = format!(
                    "{}:{}:{group}:{index}",
                    hooks_path.display(),
                    snake_case(event)
                );
                let has_record = config.contains(&format!("[hooks.state.\"{key}\"]"));
                match (ours, has_record) {
                    (true, true) => trusted += 1,
                    (true, false) => untrusted.push(event.clone()),
                    (false, false) => others_untrusted += 1,
                    (false, true) => {}
                }
            }
        }
    }

    untrusted.sort();
    Some(CodexTrust {
        trusted,
        untrusted,
        others_untrusted,
    })
}

/// What Codex's own consent record says about the hooks in one file.
struct CodexTrust {
    /// Ours, with a trust record.
    trusted: usize,
    /// Ours, without one — the events Codex will not run.
    untrusted: Vec<String>,
    /// Everybody else's, without one. Not ours to report on beyond the number,
    /// but part of what a person approves in the same gesture.
    others_untrusted: usize,
}

/// Codex names its events in snake_case in the trust key, where its hook file
/// names them in PascalCase.
fn snake_case(event: &str) -> String {
    let mut out = String::with_capacity(event.len() + 4);
    for (index, character) in event.char_indices() {
        if character.is_uppercase() && index > 0 {
            out.push('_');
        }
        out.extend(character.to_lowercase());
    }
    out
}

/// One line per registered hook command, saying whether it can run.
fn report_hooks(home: &Path) {
    let mut any = false;
    for target in crate::install::targets(Path::new("conbus"), home) {
        let Ok(text) = std::fs::read_to_string(&target.settings) else {
            println!(
                "hooks     {:<12} {} — no settings file",
                target.harness,
                target.settings.display()
            );
            continue;
        };
        let Ok(settings) = serde_json::from_str::<serde_json::Value>(&text) else {
            println!(
                "hooks     {:<12} {} — unreadable json",
                target.harness,
                target.settings.display()
            );
            continue;
        };
        let commands = registered_hooks(&settings);
        if commands.is_empty() {
            println!("hooks     {:<12} not registered", target.harness);
            continue;
        }
        for command in commands {
            any = true;
            match hook_runs(&command) {
                Ok(()) => println!("hooks     {:<12} runs — {command}", target.harness),
                Err(why) => println!(
                    "hooks     {:<12} CANNOT RUN ({why}) — {command}",
                    target.harness
                ),
            }
        }
    }
    if !any {
        println!("hooks     nothing registered; run `conbus install --apply`");
    }
    report_codex_trust(home);
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

    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) => report_hooks(&home),
        None => println!("hooks     no HOME to read registrations from"),
    }

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
    // When the last record was written, because "the recorder is installed and
    // the daemon is up" says nothing about whether anything is arriving. The
    // two-hour outage this check exists for looked healthy on every other line.
    // The most recent by time, not the last appended. Backfill appends
    // months-old conversations to the end, so append order answers a different
    // question than the one being asked here, which is whether anything is
    // still arriving.
    match scan.records.iter().max_by_key(|record| record.ts) {
        Some(last) => println!("last      {}", summarise(last)),
        None => println!("last      nothing recorded yet"),
    }

    // What a session opening in each checkout would have been told, replayed
    // over the whole log. Nothing is injected yet; this is the measurement that
    // has to come first, because how often it speaks and how wide it speaks is
    // the only way to know whether it is chrome before it is chrome in front of
    // a person.
    let home = std::env::var("HOME").ok();
    let replay = conbus_core::continuation::replay(&scan.records, home.as_deref());
    println!("briefing  {} conversation(s) judged", replay.total());
    for (verdict, count) in &replay.verdicts {
        println!("          {count:>6}  {verdict}");
    }
    let briefs = replay.verdicts.get("brief").copied().unwrap_or_default();
    if briefs > 0 {
        println!(
            "          {:>6.1}  conversations named per briefing (cap {})",
            replay.named as f64 / briefs as f64,
            conbus_core::continuation::BRIEF_MAX
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hook_that_cannot_run_is_not_reported_as_healthy() {
        // The two-hour outage: the binary was overwritten in place while a
        // running daemon held it mapped, so the kernel killed every exec of
        // that path before `main` ran. A killed process writes no drop record,
        // so the drop log stayed empty and every other line of `doctor` said
        // the recorder was fine.
        assert!(
            hook_runs("exit 137").is_err(),
            "a killed hook must be caught"
        );
        assert!(
            hook_runs("kill -9 $$").is_err(),
            "a signalled hook must be caught"
        );
        assert!(
            hook_runs("this-command-does-not-exist-anywhere").is_err(),
            "a registration pointing at nothing must be caught"
        );
    }

    #[test]
    fn a_hook_that_exits_zero_in_silence_is_not_reported_as_healthy() {
        // Exiting zero is not the contract. A hook that says nothing on stdout
        // reached the API as an empty text block carrying `cache_control` and
        // took a real session down with it, so silence has to fail here too.
        assert!(hook_runs("true").is_err(), "empty stdout must be caught");
        assert!(
            hook_runs("echo not json").is_err(),
            "junk stdout must be caught"
        );
        assert!(hook_runs("echo '{\"continue\":true}'").is_ok());
    }

    #[test]
    fn the_probe_cannot_reach_the_log() {
        // It runs the real registered command, so it has to be inert. The
        // internal marker is what makes it so, and this is the only thing
        // stopping `doctor` from writing a record every time it is run.
        let saw = hook_runs(&format!(
            "test -n \"${{{}}}\" && echo '{{\"continue\":true}}'",
            crate::hook::client::INTERNAL_MARKER
        ));

        assert!(
            saw.is_ok(),
            "the probe must mark the child internal: {saw:?}"
        );
    }

    #[test]
    fn a_codex_hook_codex_will_never_run_is_reported_as_such() {
        // The trap this exists for. A Codex hook that is registered but not
        // trusted is never spawned at all, so the executability probe says
        // "runs" about a command Codex will not invoke — and on the machine
        // this was written on, that was the live state of all six.
        let home = tempfile::tempdir().expect("tempdir");
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).expect("mkdir");
        let hooks = codex.join("hooks.json");
        std::fs::write(
            &hooks,
            serde_json::json!({
                "hooks": {
                    "UserPromptSubmit": [
                        {"hooks": [{"command": "/x/conbus hook --harness codex"}]}
                    ],
                    "PostToolUse": [
                        {"hooks": [{"command": "paseo hooks codex PostToolUse"}]},
                        {"hooks": [{"command": "/x/conbus hook --harness codex"}]}
                    ]
                }
            })
            .to_string(),
        )
        .expect("write");

        let state = codex_trust(home.path()).expect("read");
        assert_eq!(state.trusted, 0);
        assert_eq!(state.untrusted, vec!["PostToolUse", "UserPromptSubmit"]);
        assert_eq!(
            state.others_untrusted, 1,
            "the neighbour in the same file is approved in the same gesture"
        );

        // Codex keys the record by file, snake-cased event, matcher-group index
        // and position within the group — so the second group's index is what
        // makes the PostToolUse entry a different hook from paseo's.
        std::fs::write(
            codex.join("config.toml"),
            format!(
                "[hooks.state.\"{}:post_tool_use:1:0\"]\ntrusted_hash = \"sha256:abc\"\n",
                hooks.display()
            ),
        )
        .expect("write");

        let state = codex_trust(home.path()).expect("read");
        assert_eq!(state.trusted, 1);
        assert_eq!(
            state.untrusted,
            vec!["UserPromptSubmit"],
            "a record for one hook says nothing about another"
        );
    }

    #[test]
    fn the_trust_key_uses_codexs_own_spelling_of_an_event() {
        assert_eq!(snake_case("UserPromptSubmit"), "user_prompt_submit");
        assert_eq!(snake_case("PreToolUse"), "pre_tool_use");
        assert_eq!(snake_case("Stop"), "stop");
    }

    #[test]
    fn only_our_own_registrations_are_probed() {
        // A settings file is shared. Probing a neighbour's hook would run
        // someone else's command, and reporting it would blame them for our
        // diagnosis.
        let settings = serde_json::json!({
            "hooks": {
                "PostToolUse": [
                    {"hooks": [{"command": "/Users/x/.local/bin/conbus hook", "type": "command"}]},
                    {"hooks": [{"command": "paseo hooks codex PostToolUse", "type": "command"}]},
                    {"hooks": [{"command": "python3 /Users/x/.codex/hooks/guard.py", "type": "command"}]}
                ],
                "SessionStart": [
                    {"hooks": [{"command": "/opt/conbus hook --harness codex", "type": "command"}]}
                ]
            }
        });

        let found = registered_hooks(&settings);

        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            vec![
                "/Users/x/.local/bin/conbus hook".to_string(),
                "/opt/conbus hook --harness codex".to_string(),
            ]
        );
    }

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
                entrypoint: None,
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
            author: Some(Author::new(Author::SDK)),
            prompt: Body::kept("p"),
        }));

        assert!(machine.contains("sdk"), "{machine}");
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
