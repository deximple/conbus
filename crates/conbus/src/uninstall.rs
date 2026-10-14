//! `conbus uninstall` — take back everything `install --apply` wrote.
//!
//! The same rule as installing, run backwards: these files belong to the
//! person. Only entries that run this recorder's hook come out; every other
//! hook stays where it was, including one that shares a block with ours. Files
//! that are wholly ours — the grok hook file, the opencode plugin, the launchd
//! agent, the installed binary — are removed. The log is never touched: it is
//! the person's record of their own work, and deleting it is theirs to decide.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::install::{self, is_conbus_hook};

/// The first line of the plugin `install` writes, which is how it is known
/// to be ours rather than a plugin somebody happened to call `conbus.js`.
const PLUGIN_MARK: &str = "// Written by conbus install.";

/// What taking the recorder out of one Claude-schema hooks document removes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Unmerged {
    /// Events that ran the recorder and no longer do.
    pub removed: Vec<String>,
    /// Events where hooks registered after ours move up a place. Codex keys
    /// its trust record by position, so it will ask about those again.
    pub shifted: Vec<String>,
}

/// Remove the recorder from a Claude-schema document — Claude Code's
/// `settings.json`, Codex's `hooks.json`, grok's hook files. The input is
/// never mutated.
pub fn unmerge(existing: &Value) -> (Value, Unmerged) {
    let mut document = existing.as_object().cloned().unwrap_or_default();
    let mut report = Unmerged::default();
    let Some(hooks) = document.get("hooks").and_then(Value::as_object).cloned() else {
        return (Value::Object(document), report);
    };
    let mut kept_hooks = serde_json::Map::new();
    for (event, blocks) in hooks {
        let Some(blocks) = blocks.as_array() else {
            kept_hooks.insert(event, blocks);
            continue;
        };
        let mut kept = Vec::new();
        let mut first_removed: Option<usize> = None;
        for (index, block) in blocks.iter().enumerate() {
            let (block, removed) = without_our_commands(block);
            if removed && first_removed.is_none() {
                first_removed = Some(index);
            }
            if let Some(block) = block {
                kept.push(block);
            }
        }
        if let Some(at) = first_removed {
            report.removed.push(event.clone());
            if blocks.len() > at + 1 {
                report.shifted.push(event.clone());
            }
        }
        if !kept.is_empty() || first_removed.is_none() {
            kept_hooks.insert(event, Value::Array(kept));
        }
    }
    if kept_hooks.is_empty() {
        document.remove("hooks");
    } else {
        document.insert("hooks".into(), Value::Object(kept_hooks));
    }
    (Value::Object(document), report)
}

/// One registration block without our hook entries. `None` when nothing of the
/// block is left; the flag says whether anything was ours.
fn without_our_commands(block: &Value) -> (Option<Value>, bool) {
    let Some(entries) = block.get("hooks").and_then(Value::as_array) else {
        return (Some(block.clone()), false);
    };
    let kept: Vec<Value> = entries
        .iter()
        .filter(|entry| {
            !entry
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(is_conbus_hook)
        })
        .cloned()
        .collect();
    if kept.len() == entries.len() {
        return (Some(block.clone()), false);
    }
    if kept.is_empty() {
        return (None, true);
    }
    let mut block = block.clone();
    if let Some(object) = block.as_object_mut() {
        object.insert("hooks".into(), Value::Array(kept));
    }
    (Some(block), true)
}

/// Remove the recorder from Cursor's flat `hooks.json`, where each entry is a
/// `{ command }` rather than Claude's nested block.
pub fn unmerge_cursor(existing: &Value) -> (Value, Vec<String>) {
    let mut document = existing.as_object().cloned().unwrap_or_default();
    let mut removed = Vec::new();
    let Some(hooks) = document.get("hooks").and_then(Value::as_object).cloned() else {
        return (Value::Object(document), removed);
    };
    let mut kept_hooks = serde_json::Map::new();
    for (event, entries) in hooks {
        let Some(entries) = entries.as_array() else {
            kept_hooks.insert(event, entries);
            continue;
        };
        let kept: Vec<Value> = entries
            .iter()
            .filter(|entry| {
                !entry
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(is_conbus_hook)
            })
            .cloned()
            .collect();
        if kept.len() != entries.len() {
            removed.push(event.clone());
            if kept.is_empty() {
                continue;
            }
        }
        kept_hooks.insert(event, Value::Array(kept));
    }
    document.insert("hooks".into(), Value::Object(kept_hooks));
    (Value::Object(document), removed)
}

/// Put back the StatusLine `install` wrapped: the person's own command when
/// one was saved beside the settings file, nothing when there was none. A
/// StatusLine that is not ours any more is left alone.
///
/// Returns whether the document changed.
pub fn restore_statusline(document: &mut Value, forward: Option<&str>) -> bool {
    let ours = document
        .get("statusLine")
        .and_then(|line| line.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|command| is_conbus_hook(command) && command.contains("--statusline"));
    if !ours {
        return false;
    }
    let Some(object) = document.as_object_mut() else {
        return false;
    };
    match forward.map(str::trim).filter(|command| !command.is_empty()) {
        Some(original) => {
            if let Some(line) = object.get_mut("statusLine").and_then(Value::as_object_mut) {
                line.insert("command".into(), Value::String(original.to_string()));
            }
        }
        None => {
            object.remove("statusLine");
        }
    }
    true
}

/// `conbus uninstall [--apply]`. Reports by default, like `install`.
pub fn run(apply: bool) -> io::Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("no HOME to uninstall from"))?;
    let binary = install::installed_binary(&home);
    let mut changed = false;

    for target in install::targets(&binary, &home) {
        changed |= settings_file(&target, apply)?;
    }
    changed |= cursor_file(&install::cursor_hooks_path(&home), apply)?;
    changed |= plugin_file(&install::opencode_plugin_path(&home), apply)?;
    #[cfg(target_os = "macos")]
    let service = install::launch_agent_path(&home);
    #[cfg(not(target_os = "macos"))]
    let service = install::daemon_unit_path(&home);
    changed |= daemon_service(&service, apply)?;
    changed |= binary_file(&binary, apply)?;

    let root = std::env::var_os("CONBUS_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".conbus"));
    if root.exists() {
        println!(
            "{:<12} {} — kept. It is your log; remove it yourself if you want it gone: rm -rf {}",
            "data",
            root.display(),
            shell_quote(&root)
        );
    }
    if !changed {
        println!("\nNothing of conbus is registered here.");
    } else if !apply {
        println!("\nNothing was changed. Re-run with --apply to remove.");
    }
    Ok(())
}

fn settings_file(target: &install::Target, apply: bool) -> io::Result<bool> {
    let path = &target.settings;
    let Some(existing) = read_json(target.harness, path)? else {
        return Ok(false);
    };
    let (mut document, report) = unmerge(&existing);
    let forward_path = install::statusline_forward_path(path);
    let statusline = target.harness == "claude_code" && {
        let forward = fs::read_to_string(&forward_path).ok();
        restore_statusline(&mut document, forward.as_deref())
    };
    let wholly_ours =
        target.harness == "grok" && document.as_object().is_some_and(serde_json::Map::is_empty);

    if report.removed.is_empty() && !statusline {
        return Ok(false);
    }
    let mut what = report.removed.join(", ");
    if statusline {
        what = if what.is_empty() {
            "StatusLine".into()
        } else {
            format!("{what}, StatusLine")
        };
    }
    println!(
        "{:<12} {} — {} {what}",
        target.harness,
        path.display(),
        if apply { "removed" } else { "would remove" }
    );
    if target.harness == "codex" && !report.shifted.is_empty() {
        println!(
            "{:<12} other hooks on {} move up a place; Codex will ask you to trust them again",
            "",
            report.shifted.join(", ")
        );
    }
    if !apply {
        return Ok(true);
    }
    if wholly_ours {
        fs::remove_file(path)?;
        println!("{:<12} the file was only ours; deleted", "");
    } else {
        let backup = path.with_extension("json.conbus-uninstall-backup");
        fs::copy(path, &backup)?;
        let text = serde_json::to_string_pretty(&document).map_err(io::Error::other)?;
        install::write_atomically(path, &format!("{text}\n"))?;
        println!("{:<12} written; previous copy at {}", "", backup.display());
    }
    if statusline && forward_path.exists() {
        fs::remove_file(&forward_path)?;
    }
    Ok(true)
}

fn cursor_file(path: &Path, apply: bool) -> io::Result<bool> {
    let Some(existing) = read_json("cursor", path)? else {
        return Ok(false);
    };
    let (document, removed) = unmerge_cursor(&existing);
    if removed.is_empty() {
        return Ok(false);
    }
    println!(
        "{:<12} {} — {} {}",
        "cursor",
        path.display(),
        if apply { "removed" } else { "would remove" },
        removed.join(", ")
    );
    if apply {
        let backup = path.with_extension("json.conbus-uninstall-backup");
        fs::copy(path, &backup)?;
        let text = serde_json::to_string_pretty(&document).map_err(io::Error::other)?;
        install::write_atomically(path, &format!("{text}\n"))?;
        println!("{:<12} written; previous copy at {}", "", backup.display());
    }
    Ok(true)
}

fn plugin_file(path: &Path, apply: bool) -> io::Result<bool> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(false);
    };
    if !text.starts_with(PLUGIN_MARK) {
        println!(
            "{:<12} {} — not written by conbus; left alone",
            "opencode",
            path.display()
        );
        return Ok(false);
    }
    println!(
        "{:<12} {} — {}",
        "opencode",
        path.display(),
        if apply { "deleted" } else { "would delete" }
    );
    if apply {
        fs::remove_file(path)?;
    }
    Ok(true)
}

fn daemon_service(path: &Path, apply: bool) -> io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    println!(
        "{:<12} {} — {}",
        "daemon",
        path.display(),
        if apply {
            "stopped and deleted"
        } else {
            "would stop the daemon and delete the service"
        }
    );
    if apply {
        // After the hooks are gone, so nothing is left calling a stopped daemon.
        stop_daemon_service();
        fs::remove_file(path)?;
    }
    Ok(true)
}

#[cfg(target_os = "macos")]
fn stop_daemon_service() {
    if std::env::var_os("CONBUS_SKIP_LAUNCHCTL").is_none() {
        let uid = unsafe { libc::getuid() };
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/{}", install::DAEMON_LABEL)])
            .status();
    }
}

/// `disable --now` stops the unit and unlinks it in one act; a machine with
/// no `systemctl --user` answers nothing and the file still comes out.
#[cfg(not(target_os = "macos"))]
fn stop_daemon_service() {
    if std::env::var_os("CONBUS_SKIP_LAUNCHCTL").is_none() {
        let _ = std::process::Command::new("systemctl")
            .args([
                "--user",
                "disable",
                "--now",
                &format!("{}.service", install::DAEMON_LABEL),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn binary_file(binary: &Path, apply: bool) -> io::Result<bool> {
    if !binary.is_file() {
        return Ok(false);
    }
    println!(
        "{:<12} {} — {}",
        "binary",
        binary.display(),
        if apply { "deleted" } else { "would delete" }
    );
    if apply {
        // Unlinking a running executable is fine on Unix: this process keeps
        // its mapped image, and the next exec of the path finds nothing.
        fs::remove_file(binary)?;
    }
    Ok(true)
}

/// A vendor file as JSON, or `None` when it is not there. A file that does not
/// parse is reported and left alone, exactly as `install` treats it.
fn read_json(label: &str, path: &Path) -> io::Result<Option<Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    match serde_json::from_str(&text) {
        Ok(value) => Ok(Some(value)),
        Err(err) => {
            println!(
                "{label:<12} {} — NOT TOUCHED: {err}; conbus will not rewrite what it cannot read",
                path.display()
            );
            Ok(None)
        }
    }
}

fn shell_quote(path: &Path) -> String {
    let text = path.display().to_string();
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c))
    {
        text
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OURS: &str = "/Users/x/.local/bin/conbus hook";

    #[test]
    fn only_the_recorder_comes_out_and_every_other_hook_stays_in_place() {
        let installed = json!({
            "model": "opus",
            "hooks": {
                "Stop": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "their-own-stop" }] },
                    { "hooks": [{ "type": "command", "command": OURS }] }
                ],
                "PreToolUse": [
                    { "hooks": [
                        { "type": "command", "command": "their-guard" },
                        { "type": "command", "command": OURS }
                    ] }
                ],
                "SessionStart": [{ "hooks": [{ "type": "command", "command": OURS }] }],
                "PreCompact": [{ "hooks": [{ "type": "command", "command": "another" }] }]
            }
        });

        let (document, report) = unmerge(&installed);

        assert_eq!(
            document,
            json!({
                "model": "opus",
                "hooks": {
                    "Stop": [
                        { "matcher": "*", "hooks": [{ "type": "command", "command": "their-own-stop" }] }
                    ],
                    "PreToolUse": [
                        { "hooks": [{ "type": "command", "command": "their-guard" }] }
                    ],
                    "PreCompact": [{ "hooks": [{ "type": "command", "command": "another" }] }]
                }
            })
        );
        let mut removed = report.removed.clone();
        removed.sort();
        assert_eq!(removed, ["PreToolUse", "SessionStart", "Stop"]);
        assert!(report.shifted.is_empty(), "ours was last everywhere");
    }

    #[test]
    fn a_registration_left_by_an_older_install_elsewhere_comes_out_too() {
        let installed = json!({ "hooks": { "Stop": [
            { "hooks": [{ "type": "command", "command": "/Applications/conbus/target/release/conbus hook" }] }
        ] } });

        let (document, report) = unmerge(&installed);

        assert_eq!(document, json!({}));
        assert_eq!(report.removed, ["Stop"]);
    }

    #[test]
    fn a_hook_registered_after_ours_is_reported_as_moving() {
        // Codex's trust record is keyed by position; the person should hear
        // that Codex will ask again rather than find it out.
        let installed = json!({ "hooks": { "PermissionRequest": [
            { "hooks": [{ "type": "command", "command": "/opt/conbus hook --harness codex" }] },
            { "hooks": [{ "type": "command", "command": "paseo hooks codex PermissionRequest" }] }
        ] } });

        let (_, report) = unmerge(&installed);

        assert_eq!(report.shifted, ["PermissionRequest"]);
    }

    #[test]
    fn uninstalling_what_was_never_installed_changes_nothing() {
        let theirs = json!({ "hooks": { "Stop": [
            { "hooks": [{ "type": "command", "command": "their-own-stop" }] }
        ] }, "model": "opus" });

        let (document, report) = unmerge(&theirs);

        assert_eq!(document, theirs);
        assert_eq!(report, Unmerged::default());
    }

    #[test]
    fn cursor_entries_come_out_and_the_version_stays() {
        let installed = json!({
            "version": 1,
            "hooks": {
                "sessionEnd": [{ "command": "their-session-end" }, { "command": "/opt/conbus hook --harness cursor" }],
                "sessionStart": [{ "command": "/opt/conbus hook --harness cursor" }]
            }
        });

        let (document, removed) = unmerge_cursor(&installed);

        assert_eq!(
            document,
            json!({ "version": 1, "hooks": { "sessionEnd": [{ "command": "their-session-end" }] } })
        );
        let mut removed = removed;
        removed.sort();
        assert_eq!(removed, ["sessionEnd", "sessionStart"]);
    }

    #[test]
    fn the_statusline_goes_back_to_the_persons_own_command_with_its_other_fields() {
        let mut document = json!({ "statusLine": {
            "type": "command", "command": "/opt/conbus hook --statusline", "padding": 0
        } });

        assert!(restore_statusline(
            &mut document,
            Some("their-status-bar\n")
        ));

        assert_eq!(
            document,
            json!({ "statusLine": { "type": "command", "command": "their-status-bar", "padding": 0 } })
        );
    }

    #[test]
    fn a_statusline_conbus_created_is_removed_and_one_it_did_not_is_left() {
        let mut created = json!({ "statusLine": { "type": "command", "command": "/opt/conbus hook --statusline" } });
        assert!(restore_statusline(&mut created, None));
        assert_eq!(created, json!({}));

        let mut theirs =
            json!({ "statusLine": { "type": "command", "command": "their-status-bar" } });
        assert!(!restore_statusline(&mut theirs, Some("anything")));
        assert_eq!(
            theirs,
            json!({ "statusLine": { "type": "command", "command": "their-status-bar" } })
        );
    }
}
