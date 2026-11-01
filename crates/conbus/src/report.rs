//! `conbus log` and `conbus doctor`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use conbus_core::event::{Event, Harness, ToolOutcome};
use conbus_core::ids::BusId;
use conbus_core::paths::Paths;
use conbus_core::registry::{Registry, SessionKey};

use crate::cli::LogFilter;
use conbus_core::record::Record;
use conbus_core::segment::{Scan, SegmentLog};
use std::fs::OpenOptions;

pub fn log(paths: &Paths, json: bool, filter: &LogFilter) -> io::Result<()> {
    // A bounded read only opens the newest segments it must; the record-level
    // filters still apply below, so --tail --bus-id/--grep stay exact.
    let scan = match filter.tail {
        Some(n) => {
            let (records, defects) = SegmentLog::tail(paths.log_dir(), n, |record| {
                filter
                    .bus_id
                    .as_ref()
                    .is_none_or(|wanted| record.bus_id.to_string().starts_with(wanted.as_str()))
                    && filter
                        .since
                        .as_ref()
                        .is_none_or(|from| record.ts.to_string().as_str() >= from.as_str())
                    && filter
                        .grep
                        .as_ref()
                        .is_none_or(|needle| summarise(record).contains(needle.as_str()))
            })?;
            Scan { records, defects }
        }
        None => SegmentLog::scan(paths.log_dir())?,
    };
    for record in select(&scan.records, filter) {
        let revealed;
        let record = if filter.reveal {
            revealed = Record {
                event: conbus_core::retention::reveal_event(&record.event),
                ..record.clone()
            };
            &revealed
        } else {
            record
        };
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
    let aliased = filter
        .bus_id
        .as_ref()
        .map(|wanted| conbus_core::identity::expand_aliases(records, wanted));
    let matching: Vec<&Record> = records
        .iter()
        .filter(|record| {
            filter.bus_id.as_ref().is_none_or(|wanted| {
                aliased
                    .as_ref()
                    .is_some_and(|set| !set.is_empty() && set.contains(&record.bus_id))
                    || record.bus_id.to_string().starts_with(wanted)
            }) && filter
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
        // A first install reads like a fault otherwise: nothing has run yet,
        // and the daemon makes the directory the first time it starts.
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            format!("not created yet ({err}) — the daemon makes it the first time it starts")
        }
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
        Event::PermissionAsked { tool, .. } => format!("permission asked for {tool}"),
        Event::Notified { kind, .. } => format!("notified {kind}"),
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
        Event::Routed {
            next,
            hop,
            reason,
            code_freshness,
            ..
        } => {
            let code = code_freshness
                .as_deref()
                .map(|c| format!(" · {c}"))
                .unwrap_or_default();
            format!("routed {hop:?} → {next} ({reason}){code}")
        }
        Event::Continued { predecessor, hop } => {
            format!("continued {hop:?} from {predecessor}")
        }
        Event::Summarised {
            backend,
            covered_turns,
            ..
        } => format!("summarised {backend} after {covered_turns} turns"),
        Event::Leased { holder } => format!("leased by {holder}"),
        Event::Aliased { also } => format!("aliased with {also}"),
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
        | Event::PermissionAsked { .. }
        | Event::Notified { .. }
        | Event::Briefed { .. }
        | Event::Routed { .. }
        | Event::Continued { .. }
        | Event::Summarised { .. }
        | Event::Leased { .. }
        | Event::Aliased { .. } => String::new(),
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
pub fn install(apply: bool, profile: Option<&str>) -> io::Result<()> {
    let current = std::env::current_exe()?;
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other("no HOME to install into"))?;
    // A profile is `$HOME/.claude-NAME`, and it only exists once the vendor's
    // own login has created it — conbus never creates one, and never opens
    // anything inside it but `settings.json`.
    let profile_dir = match profile {
        None => None,
        Some(name) if !crate::profile::valid(name) => {
            return Err(io::Error::other(format!(
                "a profile name is [a-z0-9-]; {name:?} is not one"
            )));
        }
        Some(name) => {
            let dir = crate::profile::dir(&home, name);
            if !dir.is_dir() {
                return Err(io::Error::other(format!(
                    "{} is not a profile yet — {}",
                    dir.display(),
                    crate::profile::login_hint(&home, name)
                )));
            }
            Some(dir)
        }
    };
    let dest = crate::install::installed_binary(&home);
    println!(
        "{:<12} {} → {}",
        "binary",
        current.display(),
        dest.display()
    );
    if apply {
        if crate::install::place_binary(&current, &dest)? {
            println!("{:<12} placed (beside, then rename)", "");
        } else {
            println!("{:<12} already the installed file", "");
        }
        if dest.parent().is_some_and(|dir| !path_lists(dir)) {
            println!(
                "{:<12} put {} on PATH if `conbus` is not found",
                "path",
                dest.parent().unwrap_or(dest.as_path()).display()
            );
        }
    } else if crate::install::is_installed(&current, &dest) {
        println!("{:<12} already the installed file", "");
    } else {
        println!("{:<12} would place on --apply", "");
    }
    let binary = dest;

    // `--profile` is exactly the Claude Code registration, pointed at the
    // profile's own settings.json. The other vendors' files are the
    // machine's, not the account's, so a profiled install does not touch
    // them.
    let targets = match &profile_dir {
        Some(dir) => vec![crate::install::Target {
            harness: "claude_code",
            settings: dir.join("settings.json"),
            events: &crate::install::CLAUDE_EVENTS,
            command: crate::install::hook_command(&binary, "claude_code"),
        }],
        None => crate::install::targets(&binary, &home),
    };
    for target in targets {
        let harness = target.harness;
        let settings = target.settings.clone();
        if profile_dir.is_none() && !crate::install::vendor_present(harness, &home) {
            print_absent(harness, &home);
            continue;
        }
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

    // A profiled install registers the profile's Claude Code and nothing
    // else — the other vendors' registrations are the machine's business,
    // and the shared daemon section below still runs.
    if profile_dir.is_none() {
        let plugin = crate::install::opencode_plugin_path(&home);
        if !crate::install::vendor_present("opencode", &home) {
            print_absent("opencode", &home);
        } else if crate::install::opencode_plugin_current(&binary, &home) {
            println!(
                "{:<12} {} — already registered",
                "opencode",
                plugin.display()
            );
        } else {
            let doing = if crate::install::opencode_plugin_registered(&home) {
                "not the plugin this build writes — would replace it"
            } else {
                "would register session.created, message.updated, message.part.updated, session.error"
            };
            println!("{:<12} {} — {doing}", "opencode", plugin.display());
            if apply {
                crate::install::apply_opencode_plugin(&binary, &home)?;
                println!(
                    "{:<12} written (official plugin bus, not remaining JSON)",
                    ""
                );
            }
        }

        let cursor_hooks = crate::install::cursor_hooks_path(&home);
        if !crate::install::vendor_present("cursor", &home) {
            print_absent("cursor", &home);
        } else if crate::install::cursor_hooks_registered(&home) {
            println!(
                "{:<12} {} — already registered",
                "cursor",
                cursor_hooks.display()
            );
        } else {
            println!(
                "{:<12} {} — would register sessionStart, beforeSubmitPrompt, stop, sessionEnd, afterAgentResponse",
                "cursor",
                cursor_hooks.display()
            );
            if apply {
                crate::install::apply_cursor_hooks(&binary, &home)?;
                println!(
                    "{:<12} written (native hooks.json; Claude settings do not carry this event)",
                    ""
                );
            }
        }
    }

    // Where the service manager's redirection writes the daemon's log — the
    // same root `paths` resolves, so an XDG_STATE_HOME install does not point
    // the unit at a `~/.conbus` the recorder never uses.
    let log = conbus_core::paths::Paths::resolve()
        .map(|paths| paths.root().join("daemon.log"))
        .unwrap_or_else(|_| home.join(".conbus").join("daemon.log"));
    let agent = daemon_service_path(&home);
    println!(
        "{:<12} {} — {}",
        "daemon",
        agent.display(),
        crate::install::DAEMON_LABEL
    );
    if apply {
        if let Some(parent) = log.parent() {
            conbus_core::paths::keep_dir_private(parent)?;
        }
        let load = std::env::var_os("CONBUS_SKIP_LAUNCHCTL").is_none();
        let applied = crate::install::apply_daemon(&home, &binary, &log, load)?;
        if applied.changed {
            println!("{:<12} written", "");
        } else {
            println!("{:<12} already loaded this file", "");
        }
        match applied.manager {
            crate::install::Manager::Loaded => {
                #[cfg(target_os = "macos")]
                println!(
                    "{:<12} reloaded: launchctl bootout, then bootstrap, so macOS checks the new binary",
                    ""
                );
                #[cfg(not(target_os = "macos"))]
                println!(
                    "{:<12} reloaded: systemctl --user daemon-reload, then enable --now",
                    ""
                );
            }
            #[cfg(any(test, not(target_os = "macos")))]
            crate::install::Manager::Unavailable => {
                println!(
                    "{:<12} no systemctl --user answered — the unit is written but nothing starts it here;",
                    ""
                );
                println!(
                    "{:<12} run the recorder yourself: {} daemon &",
                    "",
                    binary.display()
                );
            }
            crate::install::Manager::NotAsked => {}
        }
    } else {
        #[cfg(target_os = "macos")]
        println!("{:<12} would write the launchd agent on --apply", "");
        #[cfg(not(target_os = "macos"))]
        println!("{:<12} would write the systemd user unit on --apply", "");
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

/// The service file `install --apply` manages: the launchd agent on a Mac,
/// the systemd user unit on Linux.
#[cfg(target_os = "macos")]
fn daemon_service_path(home: &std::path::Path) -> std::path::PathBuf {
    crate::install::launch_agent_path(home)
}

/// The service file `install --apply` manages: the launchd agent on a Mac,
/// the systemd user unit on Linux.
#[cfg(not(target_os = "macos"))]
fn daemon_service_path(home: &std::path::Path) -> std::path::PathBuf {
    crate::install::daemon_unit_path(home)
}

/// A vendor this machine does not have: say so, and how to add it later.
fn print_absent(harness: &str, home: &std::path::Path) {
    println!(
        "{harness:<12} not found (no {}) — skipped; run install again after installing it",
        crate::install::vendor_home(harness, home).display()
    );
}

fn path_lists(dir: &std::path::Path) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|entry| entry == dir))
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
    crate::install::is_conbus_hook(command)
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
/// invoke. That is the same shape of lie as an empty drop log.
///
/// Read, never written. Codex keeps the consent record as `trusted_hash` under
/// `[hooks.state."<key>"]` in `config.toml`, and its own `hooks/list` hands out
/// the very value that would forge it — so conbus detects and reports, and
/// approving stays a person's act. A missing record proves untrusted. A present
/// one is trusted *or* modified; only `hooks/list`'s `trustStatus` tells them
/// apart, and a modified hook is as silent as an untrusted one.
fn report_codex_trust(home: &Path) {
    let (state, from_list) = match live_codex_trust() {
        Some(state) => (state, true),
        None => match codex_trust(home) {
            Some(state) => (state, false),
            None => return,
        },
    };
    if state.blocked() == 0 && state.trusted == 0 {
        return;
    }
    if state.blocked() == 0 {
        println!("codex     {} hook(s) trusted", state.trusted);
        if !from_list {
            println!(
                "          consent record present; Codex may still review if the command changed (hooks/list unavailable)"
            );
        }
        return;
    }
    println!(
        "codex     {} of {} hook(s) Codex will not spawn:",
        state.blocked(),
        state.blocked() + state.trusted
    );
    if !state.untrusted.is_empty() {
        println!("          NOT TRUSTED: {}", state.untrusted.join(", "));
    }
    if !state.modified.is_empty() {
        println!(
            "          MODIFIED (hash changed; Codex will not spawn until re-trusted): {}",
            state.modified.join(", ")
        );
    }
    if !from_list {
        println!(
            "          file probe only — modified hooks can look trusted until hooks/list answers"
        );
    }
    // The gesture, named. Approving is a person's act inside Codex and there is
    // no command conbus could run instead: the record is a hash, and Codex's
    // own `hooks/list` hands out the value that would forge it.
    println!(
        "          start Codex; it asks about hooks that are new or changed at startup. conbus reads that record and must never write it."
    );
    if state.others_untrusted > 0 {
        println!(
            "          note: {} other untrusted or modified hook(s) share that file and are approved in the same gesture",
            state.others_untrusted
        );
    }
}

/// Ask Codex what it will actually spawn. The file-backed record cannot tell
/// trusted from modified, and modified is the live failure mode after a deploy.
///
/// Starts a fresh app-server and waits up to 8s. A missing `codex` binary or
/// a hung MCP handshake returns `None` and the file probe takes over.
fn live_codex_trust() -> Option<CodexTrust> {
    let mut server = crate::appserver::AppServer::start(Duration::from_secs(8)).ok()?;
    let result = server.call("hooks/list", serde_json::json!({})).ok()?;
    let state = trust_from_hooks_list(&result);
    (state.trusted + state.blocked() > 0).then_some(state)
}

/// Parse `hooks/list`. `trustStatus` is the only field that distinguishes a
/// hash Codex will honour from one it will silently ignore.
fn trust_from_hooks_list(result: &serde_json::Value) -> CodexTrust {
    let mut seen = HashSet::new();
    let mut trusted = 0;
    let mut untrusted = Vec::new();
    let mut modified = Vec::new();
    let mut others_untrusted = 0;
    for group in result
        .get("data")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        for hook in group
            .get("hooks")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let command = hook
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let event_name = hook
                .get("eventName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            // Every conbus entry shares one command string. Dedup on key, and
            // if Codex omits it, on command+event — never command alone.
            let key = hook
                .get("key")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{command}\0{event_name}"));
            if !seen.insert(key) {
                continue;
            }
            let status = hook
                .get("trustStatus")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let event = hook
                .get("eventName")
                .and_then(serde_json::Value::as_str)
                .map(title_event)
                .unwrap_or_else(|| "?".into());
            match (is_ours(command), status) {
                (true, "trusted") => trusted += 1,
                (true, "modified") => modified.push(event),
                (true, _) => untrusted.push(event),
                (false, "trusted") => {}
                (false, _) => others_untrusted += 1,
            }
        }
    }
    untrusted.sort();
    modified.sort();
    CodexTrust {
        trusted,
        untrusted,
        modified,
        others_untrusted,
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
        modified: Vec::new(),
        others_untrusted,
    })
}

/// What Codex's own consent record says about the hooks in one file.
struct CodexTrust {
    /// Ours, with a hash Codex will honour.
    trusted: usize,
    /// Ours, with no consent record — the events Codex will not run.
    untrusted: Vec<String>,
    /// Ours, with a stale hash. Same silence as untrusted after a deploy.
    modified: Vec<String>,
    /// Everybody else's, without a matching hash. Not ours to report on
    /// beyond the number, but part of what a person approves in the same
    /// gesture.
    others_untrusted: usize,
}

impl CodexTrust {
    fn blocked(&self) -> usize {
        self.untrusted.len() + self.modified.len()
    }
}

/// `hooks/list` names events in camelCase; hooks.json and this report use
/// PascalCase. First-letter title case is enough — the rest is already camel.
fn title_event(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
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
        if !crate::install::vendor_present(target.harness, home) {
            println!("hooks     {:<12} not installed here", target.harness);
            continue;
        }
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
            if command.contains("hook --statusline") {
                let forward = crate::install::statusline_forward_path(&target.settings);
                println!(
                    "statusline {:<9} observes rate_limits{}",
                    target.harness,
                    if forward.exists() {
                        " · forwards existing command"
                    } else {
                        ""
                    }
                );
                continue;
            }
            match hook_runs(&command) {
                Ok(()) => println!("hooks     {:<12} runs — {command}", target.harness),
                Err(why) => println!(
                    "hooks     {:<12} CANNOT RUN ({why}) — {command}",
                    target.harness
                ),
            }
        }
        // Which events reach it. A command that runs says nothing about the
        // events it was never registered for.
        let missing = missing_events(&settings, target.events);
        if missing.is_empty() {
            println!(
                "events    {:<12} {}/{} registered",
                target.harness,
                target.events.len(),
                target.events.len()
            );
        } else {
            println!(
                "events    {:<12} MISSING {} — run `conbus install --apply`",
                target.harness,
                missing.join(", ")
            );
        }
    }
    if !any {
        println!("hooks     nothing registered; run `conbus install --apply`");
    }
    if !crate::install::vendor_present("opencode", home) {
        println!("hooks     {:<12} not installed here", "opencode");
    } else if crate::install::opencode_plugin_registered(home) {
        println!(
            "hooks     {:<12} runs — {} plugin",
            "opencode",
            crate::install::opencode_plugin_path(home).display()
        );
    } else {
        println!(
            "hooks     {:<12} MISSING plugin — run `conbus install --apply`",
            "opencode"
        );
    }
    if !crate::install::vendor_present("cursor", home) {
        println!("hooks     {:<12} not installed here", "cursor");
    } else if crate::install::cursor_hooks_registered(home) {
        println!(
            "hooks     {:<12} runs — {} {}",
            "cursor",
            crate::install::cursor_hooks_path(home).display(),
            crate::install::CURSOR_EVENTS.join(", ")
        );
    } else {
        println!(
            "hooks     {:<12} MISSING {} — run `conbus install --apply`",
            "cursor",
            crate::install::CURSOR_EVENTS.join("/")
        );
    }
    report_codex_trust(home);
}

/// The `$HOME/.claude-*` profile directories whose `settings.json` runs the
/// recorder — the profiles a person registered by hand. A profile without
/// conbus hooks is the vendor's business, not a finding.
fn report_profiles(home: &Path) {
    let Ok(entries) = std::fs::read_dir(home) else {
        return;
    };
    let mut registered: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let name = name.strip_prefix(".claude-")?.to_string();
            let settings = std::fs::read_to_string(entry.path().join("settings.json")).ok()?;
            let settings: serde_json::Value = serde_json::from_str(&settings).ok()?;
            (!registered_hooks(&settings).is_empty()).then_some(name)
        })
        .collect();
    registered.sort_unstable();
    if registered.is_empty() {
        println!("profiles  none — no $HOME/.claude-*/settings.json carries conbus hooks");
    } else {
        println!("profiles  {}", registered.join(", "));
    }
}

/// Vendor sessions the log holds twice — once as the recorder mirrored them
/// and once as backfill reconstructed them — and where each one's binding
/// lands.
///
/// A twin is what `backfill --apply` left behind when it recovered a
/// transcript the recorder had already seen live: the copy carries the
/// transcript's own UUID as its `bus_id`, so one session is two conversations.
/// Which of the two a restart continues is the registry's decision, and this
/// reports that decision rather than re-deriving it, so that `doctor` and the
/// daemon cannot disagree about it.
struct Twins {
    total: usize,
    /// Bound to a conversation the recorder wrote live.
    bound_live: usize,
    /// Bound to the reconstruction, so a restart carries on under the copy.
    bound_to_copy: usize,
    /// Live records under more than one conversation: a session that came
    /// back from a restart bound to its copy and kept going there. An
    /// append-only log cannot take that back, so the least that can be done
    /// is to say it happened.
    split: usize,
    /// Split sessions whose live ids are already joined by `aliased` records.
    split_aliased: usize,
}

fn twins(records: &[Record]) -> Twins {
    let mut live: HashMap<SessionKey, BTreeSet<BusId>> = HashMap::new();
    let mut copied: HashSet<SessionKey> = HashSet::new();
    for record in records {
        let Some(vendor_session_id) = &record.source.vendor_session_id else {
            continue;
        };
        let key = SessionKey::new(record.source.harness.clone(), vendor_session_id.clone());
        if record.source.backfilled_from.is_some() {
            copied.insert(key);
        } else {
            live.entry(key).or_default().insert(record.bus_id);
        }
    }

    let registry = Registry::restore(records);
    let mut summary = Twins {
        total: 0,
        bound_live: 0,
        bound_to_copy: 0,
        split: 0,
        split_aliased: 0,
    };
    for (key, conversations) in &live {
        // Split is a fact about the live records alone: a torn first record
        // followed by a restart forks a session with no backfill involved.
        if conversations.len() > 1 {
            summary.split += 1;
            if let Some(first) = conversations.iter().next() {
                let joined = conbus_core::identity::expand_aliases(records, &first.to_string());
                if conversations.iter().all(|id| joined.contains(id)) {
                    summary.split_aliased += 1;
                }
            }
        }
        if !copied.contains(key) {
            continue;
        }
        summary.total += 1;
        match registry.bus_id(key) {
            Some(bound) if conversations.contains(&bound) => summary.bound_live += 1,
            _ => summary.bound_to_copy += 1,
        }
    }
    summary
}

/// The events a settings document does not register the recorder for, out of
/// the ones this build expects.
///
/// A conbus binary at any path counts as registered — the question here is
/// whether the event reaches *a* recorder, and which binary is the probe's
/// job. Somebody else's hook on the same event does not count. This is the
/// drift the first audit found by hand: eleven events expected, ten on the
/// machine, and every other line of `doctor` reporting health.
fn missing_events(settings: &serde_json::Value, events: &[&str]) -> Vec<String> {
    events
        .iter()
        .filter(|event| {
            let registered = settings
                .get("hooks")
                .and_then(|hooks| hooks.get(**event))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|block| {
                    block
                        .get("hooks")
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .filter_map(|hook| hook.get("command")?.as_str())
                .any(is_ours);
            !registered
        })
        .map(|event| (*event).to_string())
        .collect()
}

/// Conversations keyed on an empty vendor session id, with how many records
/// each holds and which harness wrote them.
///
/// An empty id is not an identity (`hook::NO_SESSION`), and this build refuses
/// to key on one. Records an older build wrote that way stay in the log —
/// it is append-only — so the line says what they are rather than letting a
/// merged conversation pass as one real session.
fn nameless(records: &[Record]) -> Vec<(BusId, String, usize)> {
    let mut found: Vec<(BusId, String, usize)> = Vec::new();
    for record in records {
        let empty = record
            .source
            .vendor_session_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty());
        if !empty {
            continue;
        }
        match found.iter_mut().find(|(bus, _, _)| *bus == record.bus_id) {
            Some((_, _, n)) => *n += 1,
            None => found.push((record.bus_id, record.source.harness.as_str().to_string(), 1)),
        }
    }
    found
}

fn report_nameless(records: &[Record]) {
    for (bus, harness, n) in nameless(records) {
        println!(
            "nameless  {bus} — {n} {harness} record(s) keyed on an empty session id by an older \
             build; they came from sessions that named none and may belong to several"
        );
    }
}

fn report_twins(records: &[Record]) {
    let summary = twins(records);
    if summary.total == 0 {
        println!("twins     none");
        return;
    }
    println!(
        "twins     {} vendor session(s) recorded both live and by backfill; {} bound to their \
         live conversation",
        summary.total, summary.bound_live
    );
    if summary.bound_to_copy > 0 {
        println!(
            "          {} bound to their backfilled copy — a restart continues them under the copy",
            summary.bound_to_copy
        );
    }
    if summary.split > 0 {
        if summary.split_aliased == summary.split {
            println!(
                "          {} recorded live under more than one conversation; aliased — \
                 `conbus log --bus-id` follows either id",
                summary.split
            );
        } else {
            println!(
                "          {} recorded live under more than one conversation; {} aliased so a \
                 reader following one id finds the rest",
                summary.split, summary.split_aliased
            );
        }
    }
}

pub fn doctor(paths: &Paths) -> io::Result<()> {
    println!("build     {}", crate::build_info::describe());
    println!(
        "root      {} ({:?})",
        paths.root().display(),
        paths.origin()
    );
    println!("log       {}", paths.log_dir().display());

    let socket = paths.socket();
    let daemon = match (paths.socket_problem(), UnixStream::connect(&socket)) {
        (Some(problem), _) => format!("UNUSABLE — {problem}"),
        (None, Ok(_)) => "listening".to_string(),
        (None, Err(err)) => format!("not answering ({err})"),
    };
    println!("socket    {} — {daemon}", socket.display());
    // Which build answers on that socket. The daemon writes it at start; a
    // binary replaced underneath a running daemon shows up here as DIFFERS,
    // which is the window this line exists for.
    match crate::build_info::daemon_build(paths) {
        crate::build_info::DaemonBuild::Running(stamp)
            if stamp.build == crate::build_info::BUILD && stamp.serving.is_some() =>
        {
            println!(
                "daemon    build {} pid {} since {}",
                stamp.build, stamp.pid, stamp.started
            );
        }
        crate::build_info::DaemonBuild::Running(stamp)
            if stamp.build == crate::build_info::BUILD =>
        {
            println!("recorder  starting since {}", stamp.started);
        }
        crate::build_info::DaemonBuild::Running(stamp) => println!(
            "daemon    build {} pid {} since {} — DIFFERS from this binary ({}); restart the daemon",
            stamp.build,
            stamp.pid,
            stamp.started,
            crate::build_info::BUILD
        ),
        crate::build_info::DaemonBuild::Stale(stamp) => println!(
            "daemon    build {} left by pid {}, which is gone; the stamp is stale",
            stamp.build, stamp.pid
        ),
        crate::build_info::DaemonBuild::Unstamped => {
            println!("daemon    build unknown — no stamp; started by a build older than this one")
        }
    }

    println!("{}", describe_hardening(paths));

    // There is no OS key store to hold a sealing key here, and conbus keeps
    // none of its own: on Linux `encrypted_tools` elides tool bodies rather
    // than sealing them under a key kept in a file.
    #[cfg(not(target_os = "macos"))]
    println!("sealing   none — no OS key store; tool bodies are elided, not sealed");

    println!("writable  {}", describe_appendable(&appendable(paths)));

    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) => {
            report_hooks(&home);
            report_profiles(&home);
        }
        None => println!("hooks     no HOME to read registrations from"),
    }
    // Chat LAM hard-disables hooks. The official export is the identity-
    // preserving channel; the daemon ingests this folder on start and
    // whenever a newer file lands.
    println!(
        "chat      Enterprise Inference hooks → conbus hook --inference-listen; or drop conversations.json / data-export-*.zip in {} — UUID kept as claude_chat; Desktop plugin hooks still vendor-off",
        paths.chat_export().display()
    );

    println!(
        "drops     {}",
        crate::drops::describe(&crate::drops::summary(paths))
    );

    // The local PreToolUse policy: a file that cannot be a policy decides
    // nothing, and the line says so because fail-open is silent by design.
    match crate::policy::load(&paths.policy()) {
        crate::policy::Load::Absent => {
            println!("policy    off — no {}", paths.policy().display())
        }
        crate::policy::Load::Invalid(err) => {
            println!("policy    {err} — decides nothing")
        }
        crate::policy::Load::Ready(policy) => println!(
            "policy    {} rule(s) in {}",
            policy.rule_count(),
            paths.policy().display()
        ),
    }

    let scan = SegmentLog::scan_meta(paths.log_dir(), Some(&paths.cache_dir()))?;
    let conversations: std::collections::HashSet<_> =
        scan.records.iter().map(|r| r.bus_id).collect();
    println!(
        "log       {} record(s), {} conversation(s), {} unreadable line(s)",
        scan.records.len(),
        conversations.len(),
        scan.defects.len()
    );
    // Hooks in transit: the spool a daemon has not drained yet.
    let waiting = std::fs::read_dir(paths.spool_dir())
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                .count()
        })
        .unwrap_or(0);
    if waiting > 0 {
        println!("spool     {waiting} waiting");
    }
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
    // Sessions the log holds twice, and which copy a restart would continue.
    // The binding is the daemon's decision, so this asks the same code the
    // daemon runs at start rather than guessing at it.
    report_twins(&scan.records);
    report_nameless(&scan.records);

    // What a session opening in each checkout would have been told, replayed
    // over the whole log. The briefing is live since 2026-09-01; this replay is
    // the offline measurement of the same decision, the one that
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

    // The router's signal, where every other health fact is. Read from a
    // fresh app-server; "unavailable" is a fact too, not an omission.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match crate::quota::read() {
        Ok(quota) => println!("quota     codex {}", crate::quota::describe(&quota, now)),
        Err(err) => println!("quota     codex unavailable — {err}"),
    }
    match crate::quota::read_agy() {
        Ok(quota) => println!("quota     agy {}", crate::quota::describe(&quota, now)),
        Err(err) => println!("quota     agy unavailable — {err}"),
    }
    println!(
        "quota     claude {}",
        crate::quota::describe_claude(paths, now)
    );

    let meter = conbus_core::meter::Meter::fold(&scan.records);
    println!("{}", meter.describe());
    println!("{}", meter.describe_provenance());
    println!("{}", meter.describe_cost());
    println!("{}", meter.describe_turns());
    println!("verdict   conbus verdict — 4-week dogfood score; open window is not a kill");
    println!("{}", cursor_line(&scan.records));
    match crate::hop::decide_from(paths, &scan.records, None) {
        Ok(line) => println!("route     {line}"),
        Err(err) => println!("route     unavailable — {err}"),
    }
    if let Some(line) = crate::takeover::describe(paths) {
        println!("takeover  {line}");
    }
    let warm = crate::hop::live_warm_threads(&scan.records);
    let claude = crate::hop::live_claude_sessions(&scan.records);
    if warm.is_empty() && claude.is_empty() {
        println!("warm      none");
    } else {
        if !warm.is_empty() {
            println!("warm      {}", warm.join(" "));
        }
        if !claude.is_empty() {
            println!("warm      claude {}", claude.join(" "));
        }
    }
    Ok(())
}

/// How the router decides. Kept next to the numbers so a person can see
/// the rule and the count on one page, without opening a chat.
const ROUTING_RULES: &str = "\
rules     auto overflow Claude↔Codex (and attach legs → Codex) on \
InterruptedQuota or account reached; predictive handoff at 80% used with \
reset > 120m; --to names any catalog leg as HandedOff; policy/auth is \
Refused, not failover";

fn cursor_line(records: &[Record]) -> String {
    let n = records
        .iter()
        .filter(|record| record.source.harness.as_str() == Harness::CURSOR)
        .count();
    format!("cursor    adapter live — {n} record(s); briefing on sessionStart additional_context")
}

/// The daemon's lock report, not this process's heap. Doctor calling
/// `harden()` itself was true on Darwin even with the daemon down.
fn describe_hardening(paths: &Paths) -> String {
    match crate::build_info::daemon_build(paths) {
        crate::build_info::DaemonBuild::Running(stamp) => match stamp.memory_locked {
            Some(locked) => format!(
                "hardening core_dumps_disabled={} memory_locked={}{}",
                stamp.core_dumps_disabled.unwrap_or(false),
                locked,
                stamp
                    .memory_lock_error
                    .as_deref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            ),
            None => "hardening unknown — this daemon did not stamp a lock report".into(),
        },
        _ => "hardening unknown — no live daemon stamp".into(),
    }
}

fn hop_history(records: &[Record], limit: usize) -> Vec<String> {
    records
        .iter()
        .rev()
        .filter(|record| matches!(record.event, Event::Routed { .. } | Event::Continued { .. }))
        .take(limit)
        .map(summarise)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// The control plane. Not a chat: rules, quota, hop history, cost.
pub fn status(paths: &Paths) -> io::Result<()> {
    let scan = SegmentLog::scan_meta(paths.log_dir(), Some(&paths.cache_dir()))?;
    let meter = conbus_core::meter::Meter::fold(&scan.records);
    println!("{ROUTING_RULES}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match crate::quota::read() {
        Ok(quota) => println!("quota     codex {}", crate::quota::describe(&quota, now)),
        Err(err) => println!("quota     codex unavailable — {err}"),
    }
    match crate::quota::read_agy() {
        Ok(quota) => println!("quota     agy {}", crate::quota::describe(&quota, now)),
        Err(err) => println!("quota     agy unavailable — {err}"),
    }
    println!(
        "quota     claude {}",
        crate::quota::describe_claude(paths, now)
    );
    println!("{}", meter.describe());
    println!("{}", meter.describe_provenance());
    println!("{}", meter.describe_cost());
    let hops = hop_history(&scan.records, 8);
    if hops.is_empty() {
        println!("history   no routed or continued line yet");
    } else {
        for line in hops {
            println!("history   {line}");
        }
    }
    if let Some(line) = crate::takeover::describe(paths) {
        println!("takeover  {line}");
    }
    Ok(())
}

/// `conbus log index`: build the derived sidecars for every closed segment
/// and say what was written — writes only under `<root>/cache/`.
pub fn log_index(paths: &Paths) -> io::Result<()> {
    let report = SegmentLog::build_cache(paths.log_dir(), paths.cache_dir())?;
    println!(
        "cache     {} closed segment(s): {} built, {} already valid",
        report.closed, report.built, report.valid
    );
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
    fn hooks_list_separates_modified_from_trusted() {
        // The live lie this exists for: a consent record is still in
        // config.toml after a dirty deploy, so the file probe said
        // UserPromptSubmit was trusted while Codex's own hooks/list said
        // modified — and modified hooks never spawn.
        let listed = serde_json::json!({
            "data": [{
                "cwd": "/tmp/one",
                "hooks": [
                    {
                        "key": "/x/hooks.json:user_prompt_submit:0:0",
                        "eventName": "userPromptSubmit",
                        "command": "/x/conbus hook --harness codex",
                        "trustStatus": "modified"
                    },
                    {
                        "key": "/x/hooks.json:session_start:0:0",
                        "eventName": "sessionStart",
                        "command": "/x/conbus hook --harness codex",
                        "trustStatus": "trusted"
                    },
                    {
                        "key": "/x/hooks.json:permission_request:1:0",
                        "eventName": "permissionRequest",
                        "command": "/x/conbus hook --harness codex",
                        "trustStatus": "untrusted"
                    },
                    {
                        "key": "/x/hooks.json:pre_tool_use:0:0",
                        "eventName": "preToolUse",
                        "command": "paseo hooks codex PreToolUse",
                        "trustStatus": "modified"
                    }
                ]
            }, {
                "cwd": "/tmp/two",
                "hooks": [
                    {
                        "key": "/x/hooks.json:user_prompt_submit:0:0",
                        "eventName": "userPromptSubmit",
                        "command": "/x/conbus hook --harness codex",
                        "trustStatus": "modified"
                    }
                ]
            }]
        });
        let state = trust_from_hooks_list(&listed);
        assert_eq!(state.trusted, 1);
        assert_eq!(state.modified, vec!["UserPromptSubmit"]);
        assert_eq!(state.untrusted, vec!["PermissionRequest"]);
        assert_eq!(state.others_untrusted, 1);
        assert_eq!(state.blocked(), 2);
    }

    #[test]
    fn a_hooks_list_entry_without_a_key_is_not_collapsed_by_command() {
        let listed = serde_json::json!({
            "data": [{
                "hooks": [
                    {
                        "eventName": "userPromptSubmit",
                        "command": "/x/conbus hook --harness codex",
                        "trustStatus": "modified"
                    },
                    {
                        "eventName": "stop",
                        "command": "/x/conbus hook --harness codex",
                        "trustStatus": "modified"
                    }
                ]
            }]
        });
        let state = trust_from_hooks_list(&listed);
        assert_eq!(state.modified, vec!["Stop", "UserPromptSubmit"]);
    }

    #[test]
    fn an_event_the_settings_do_not_register_is_named() {
        // The drift the first audit found by hand: the code expected eleven
        // Claude Code events and the machine had ten registered, because
        // `install --apply` had not been re-run after `SubagentStop` was
        // added. `doctor` said the hooks ran, which was true of the ten.
        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks": [{"command": "/Users/x/.local/bin/conbus hook"}]}],
                "UserPromptSubmit": [{"hooks": [{"command": "/opt/elsewhere/conbus hook"}]}],
                "Stop": [{"hooks": [{"command": "paseo hooks claude Stop"}]}]
            }
        });

        let missing = missing_events(&settings, &["SessionStart", "UserPromptSubmit", "Stop"]);

        assert_eq!(
            missing,
            vec!["Stop".to_string()],
            "a conbus binary at any path registers the event; somebody else's hook does not"
        );
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
            hook_id: None,
            spooled_at: None,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: None,
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
                profile: None,
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
            tokens: None,
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
    fn a_permission_ask_does_not_read_as_a_denial() {
        let asked = summarise(&record(Event::PermissionAsked {
            tool: "Bash".into(),
            input: Body::kept("i"),
        }));
        let notified = summarise(&record(Event::Notified {
            kind: "example_kind".into(),
            title: None,
            message: "a message".into(),
        }));

        assert!(asked.contains("permission asked"), "{asked}");
        assert!(!asked.contains("denied"), "{asked}");
        assert!(notified.contains("notified example_kind"), "{notified}");
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

    /// A record of the one vendor session these tests are about, as either the
    /// recorder or backfill would have written it.
    fn twin_record(seq: u64, bus: &str, backfilled: bool) -> Record {
        Record {
            seq,
            bus_id: bus.parse().expect("bus id"),
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: Some("vendor-abc".into()),
                workspace: None,
                entrypoint: None,
                backfilled_from: backfilled
                    .then(|| conbus_core::backfill::CLAUDE_TRANSCRIPT.to_string()),
                profile: None,
            },
            ..tool(None, None)
        }
    }

    #[test]
    fn a_session_recorded_both_live_and_by_backfill_is_a_twin_bound_to_its_live_side() {
        // The shape the twin defect left on a real log: live records first,
        // then backfill's copy of the same session appended behind them.
        let live = "01a0572a-418e-7d71-8927-870a858b6ee1";
        let copy = "ea1d23e9-bb83-4df5-9a73-a6a12e138d85";
        let records = vec![
            twin_record(0, live, false),
            twin_record(1, copy, true),
            twin_record(2, copy, true),
        ];

        let summary = twins(&records);

        assert_eq!(summary.total, 1);
        assert_eq!(
            summary.bound_live, 1,
            "a restart must continue this session under what the recorder saw, not under the copy"
        );
        assert_eq!(summary.bound_to_copy, 0);
        assert_eq!(summary.split, 0);
    }

    #[test]
    fn a_session_whose_live_records_fell_under_two_conversations_is_reported_as_split() {
        // What a restart did on a real machine before the binding rule was
        // fixed: the session outlived a backfill, came back bound to its copy,
        // and the rest of its turns landed there. Nothing can un-split an
        // append-only log, so the least doctor can do is say it happened.
        let live = "01a0585c-0000-7000-8000-000000000002";
        let copy = "1ce6bfb1-0000-4000-8000-000000000002";
        let records = vec![
            twin_record(0, live, false),
            twin_record(1, copy, true),
            twin_record(2, copy, false),
        ];

        let summary = twins(&records);

        assert_eq!(summary.total, 1);
        assert_eq!(summary.split, 1);
    }

    #[test]
    fn a_session_only_backfill_knows_is_not_a_twin() {
        let records = vec![twin_record(0, "ea1d23e9-bb83-4df5-9a73-a6a12e138d85", true)];

        assert_eq!(twins(&records).total, 0);
    }

    #[test]
    fn a_session_split_without_a_backfilled_copy_is_still_reported_as_split() {
        // A torn first record followed by a restart forks a live session with
        // no backfill involved. The line says "recorded live under more than
        // one conversation", so it has to count that shape too, not only the
        // twins it happened to be written next to.
        let first = "01a0585c-0000-7000-8000-000000000003";
        let second = "01a0585c-0000-7000-8000-000000000004";
        let records = vec![twin_record(0, first, false), twin_record(1, second, false)];

        let summary = twins(&records);

        assert_eq!(summary.total, 0, "no reconstruction, so not a twin");
        assert_eq!(summary.split, 1);
    }

    #[test]
    fn doctor_names_a_conversation_an_older_build_keyed_on_an_empty_session_id() {
        let keyed = |id: Option<&str>, bus: &str, harness: &str| {
            let mut r = at(0, "2026-09-10T22:18:17.000Z", bus);
            r.source.vendor_session_id = id.map(str::to_string);
            r.source.harness = Harness::new(harness);
            r
        };
        let merged = "01a08d66-742e-70f2-a5e6-eb4bda1030ed";
        let real = "01a08d66-3ec0-7b81-af99-3c0549c4ad7c";
        let records = [
            keyed(Some(""), merged, Harness::CURSOR),
            keyed(Some(""), merged, Harness::CURSOR),
            keyed(Some("bc-cfe37b99"), real, Harness::CURSOR),
            keyed(None, real, Harness::CURSOR),
        ];

        let found = nameless(&records);

        assert_eq!(
            found,
            vec![(merged.parse().expect("id"), Harness::CURSOR.to_string(), 2)]
        );
        assert!(
            nameless(&records[2..]).is_empty(),
            "a missing id is not an empty one"
        );
    }

    #[test]
    fn doctor_names_the_cursor_adapter_and_how_many_records_it_wrote() {
        let mut cursor = record(Event::TurnStarted {
            author: None,
            prompt: Body::kept("hi"),
        });
        cursor.source.harness = Harness::new(Harness::CURSOR);
        assert_eq!(
            cursor_line(&[cursor]),
            "cursor    adapter live — 1 record(s); briefing on sessionStart additional_context"
        );
        assert!(cursor_line(&[]).contains("0 record(s)"));
    }

    #[test]
    fn doctor_hardening_is_the_daemon_stamp_not_this_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        assert!(describe_hardening(&paths).contains("no live daemon stamp"));

        crate::build_info::stamp(&paths, "conversation_only").expect("stamp");
        assert!(describe_hardening(&paths).contains("did not stamp a lock report"));

        crate::build_info::stamp_hardening(&paths, true, true, None).expect("locked");
        assert_eq!(
            describe_hardening(&paths),
            "hardening core_dumps_disabled=true memory_locked=true"
        );
    }

    #[test]
    fn status_hop_history_matches_the_meter() {
        use conbus_core::event::{Hop, HopProvenance};
        let routed = record(Event::Routed {
            next: Harness::CODEX.into(),
            hop: Hop::Displaced,
            reason: "rate_limit".into(),
            code_freshness: None,
            provenance: Some(HopProvenance::VendorSignal),
        });
        let continued = record(Event::Continued {
            predecessor: routed.bus_id,
            hop: Hop::Displaced,
        });
        let records = vec![routed, continued];
        let meter = conbus_core::meter::Meter::fold(&records);
        assert_eq!(meter.failover_hops, 1);
        assert_eq!(meter.failover_edges, 1);
        let history = hop_history(&records, 8);
        assert_eq!(history.len(), 2);
        assert!(history[0].contains("routed"), "{history:?}");
        assert!(history[1].contains("continued"), "{history:?}");
    }
}
